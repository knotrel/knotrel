//! Bounded CPU work and exclusive graph ownership for each request.

use crate::{
    Lifecycle, ServerMode,
    api::{
        BatchItemError, FeederDuplicateResponse, FeederEventsRequest, FeederEventsResponse,
        GapRequest, GapResponse, HeartbeatRequest, HeartbeatResponse, HistoricalOutcome,
        ImportMode, ImportRequest, ImportResponse, Operation, Outcome, PartialBatchFailure,
        SessionRequest, SessionResponse,
    },
};
use knotrel_core::{ConnectivityGraph, EngineConfig, GraphError};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

static START_INSTANT: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

#[cfg(any(test, feature = "test-support"))]
static CLOCK_OFFSET_MILLIS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn monotonic_now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    {
        Instant::now() + Duration::from_millis(CLOCK_OFFSET_MILLIS.load(Ordering::Acquire))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        Instant::now()
    }
}

fn instant_to_millis(instant: Instant) -> u64 {
    instant.duration_since(*START_INSTANT).as_millis() as u64
}

pub(crate) fn now_to_millis() -> u64 {
    instant_to_millis(monotonic_now())
}

/// Advances the mock monotonic clock by the specified duration for deterministic testing.
///
/// Only available when the `test-support` feature is enabled.
#[cfg(any(test, feature = "test-support"))]
pub fn advance_clock(duration: Duration) {
    CLOCK_OFFSET_MILLIS.fetch_add(duration.as_millis() as u64, Ordering::Release);
}

/// Resets the mock monotonic clock offset back to zero.
///
/// Only available when the `test-support` feature is enabled.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_clock() {
    CLOCK_OFFSET_MILLIS.store(0, Ordering::Release);
}

/// Locks the mock monotonic clock mutex during tests to prevent concurrent clock mutations.
///
/// Only available when the `test-support` feature is enabled.
#[cfg(any(test, feature = "test-support"))]
static CLOCK_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Acquires the lock on the test clock for deterministic testing.
#[cfg(any(test, feature = "test-support"))]
pub async fn lock_clock_for_test() -> tokio::sync::MutexGuard<'static, ()> {
    CLOCK_TEST_MUTEX.lock().await
}

/// Feeder synchronization status for integration mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SyncStatus {
    Uninitialized,
    Loading,
    CatchingUp,
    Ready,
    Stale,
    Desynchronized,
}

impl SyncStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Loading => "loading",
            Self::CatchingUp => "catching_up",
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Desynchronized => "desynchronized",
        }
    }

    pub(crate) const fn to_code(self) -> u8 {
        match self {
            Self::Uninitialized => 0,
            Self::Loading => 1,
            Self::CatchingUp => 2,
            Self::Ready => 3,
            Self::Stale => 4,
            Self::Desynchronized => 5,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Loading,
            2 => Self::CatchingUp,
            3 => Self::Ready,
            4 => Self::Stale,
            5 => Self::Desynchronized,
            _ => Self::Uninitialized,
        }
    }
}

/// Packed atomic readiness publication holding status (4 bits), publication sequence (16 bits),
/// and lease deadline in monotonic milliseconds (44 bits, up to 557 years).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublishedReadiness {
    pub(crate) status: SyncStatus,
    pub(crate) seq: u16,
    pub(crate) deadline_millis: u64,
}

impl PublishedReadiness {
    pub(crate) const fn pack(status: SyncStatus, seq: u16, deadline_millis: u64) -> u64 {
        let status_bits = (status.to_code() as u64) & 0x0F;
        let seq_bits = ((seq as u64) & 0xFFFF) << 4;
        let deadline_bits = (deadline_millis & 0x0FFF_FFFF_FFFF) << 20;
        status_bits | seq_bits | deadline_bits
    }

    pub(crate) const fn unpack(word: u64) -> Self {
        let status_code = (word & 0x0F) as u8;
        let seq = ((word >> 4) & 0xFFFF) as u16;
        let deadline_millis = (word >> 20) & 0x0FFF_FFFF_FFFF;
        Self {
            status: SyncStatus::from_code(status_code),
            seq,
            deadline_millis,
        }
    }
}

/// Canonical operation descriptor normalized for undirected graph edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalOp {
    AddNode { node: u64 },
    Link { source: u64, target: u64 },
    Cut { source: u64, target: u64 },
    Progress,
}

impl CanonicalOp {
    pub(crate) fn from_feeder_event(event: &crate::api::FeederEvent) -> Result<Self, FeederError> {
        match event.op.as_str() {
            "link" => {
                let (Some(source), Some(target), None) = (event.source, event.target, event.node)
                else {
                    return Err(FeederError::InvalidRequest(
                        "link operation requires source and target, and must not have node".into(),
                    ));
                };
                Ok(Self::Link {
                    source: source.0.min(target.0),
                    target: source.0.max(target.0),
                })
            }
            "cut" => {
                let (Some(source), Some(target), None) = (event.source, event.target, event.node)
                else {
                    return Err(FeederError::InvalidRequest(
                        "cut operation requires source and target, and must not have node".into(),
                    ));
                };
                Ok(Self::Cut {
                    source: source.0.min(target.0),
                    target: source.0.max(target.0),
                })
            }
            "add_node" => {
                let (Some(node), None, None) = (event.node, event.source, event.target) else {
                    return Err(FeederError::InvalidRequest(
                        "add_node operation requires node, and must not have source or target"
                            .into(),
                    ));
                };
                Ok(Self::AddNode { node: node.0 })
            }
            "progress" => {
                if event.node.is_some() || event.source.is_some() || event.target.is_some() {
                    return Err(FeederError::InvalidRequest(
                        "progress operation must not have node, source, or target".into(),
                    ));
                }
                Ok(Self::Progress)
            }
            _ => Err(FeederError::InvalidRequest(format!(
                "unsupported operation '{}'; must be link, cut, add_node, or progress",
                event.op
            ))),
        }
    }
}

/// An entry in the bounded deduplication window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DedupEntry {
    pub(crate) delivery_sequence: u64,
    pub(crate) event_id: String,
    pub(crate) source_position: String,
    pub(crate) canonical_op: CanonicalOp,
    pub(crate) changed: bool,
}

/// Result of looking up a sequence in the deduplication window.
pub(crate) enum DedupLookup<'a> {
    InWindow(&'a DedupEntry),
    Evicted,
    Future,
}

/// Bounded deduplication window tracking delivery sequence and event IDs.
#[derive(Debug)]
pub(crate) struct DedupWindow {
    capacity: usize,
    ring: Vec<Option<DedupEntry>>,
    event_ids: std::collections::HashMap<String, u64>,
}

impl DedupWindow {
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            ring: (0..capacity).map(|_| None).collect(),
            event_ids: std::collections::HashMap::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.ring.fill(None);
        self.event_ids.clear();
    }

    pub(crate) fn check_sequence(&self, seq: u64, applied_seq: u64) -> DedupLookup<'_> {
        if applied_seq == 0 || seq > applied_seq {
            return DedupLookup::Future;
        }
        let min_seq = if applied_seq >= self.capacity as u64 {
            applied_seq - self.capacity as u64 + 1
        } else {
            1
        };
        if seq < min_seq {
            return DedupLookup::Evicted;
        }
        let slot = (seq as usize) % self.capacity;
        if let Some(ref entry) = self.ring[slot]
            && entry.delivery_sequence == seq
        {
            return DedupLookup::InWindow(entry);
        }
        DedupLookup::Evicted
    }

    pub(crate) fn contains_event_id(&self, event_id: &str) -> bool {
        self.event_ids.contains_key(event_id)
    }

    pub(crate) fn record(&mut self, entry: DedupEntry) {
        let seq = entry.delivery_sequence;
        if let Some(evicted_seq) = seq.checked_sub(self.capacity as u64)
            && evicted_seq >= 1
        {
            let slot = (evicted_seq as usize) % self.capacity;
            if let Some(old) = self.ring[slot].take()
                && old.delivery_sequence == evicted_seq
            {
                self.event_ids.remove(&old.event_id);
            }
        }
        let slot = (seq as usize) % self.capacity;
        if let Some(old) = self.ring[slot].take() {
            self.event_ids.remove(&old.event_id);
        }
        self.event_ids.insert(entry.event_id.clone(), seq);
        self.ring[slot] = Some(entry);
    }
}

fn fresh_instance_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("srv-{pid:x}-{nanos:016x}-{count:04x}")
}

/// Encodes an opaque source position into an RFC 3986 percent-encoded string.
pub fn percent_encode_position(pos: &str) -> String {
    let mut encoded = String::with_capacity(pos.len() * 3);
    for b in pos.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'~' => {
                encoded.push(char::from(b));
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

/// Decodes a percent-encoded source position back to its raw UTF-8 string representation.
#[allow(dead_code)]
pub fn percent_decode_position(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let h1 = char::from(bytes[i + 1]).to_digit(16)? as u8;
            let h2 = char::from(bytes[i + 2]).to_digit(16)? as u8;
            decoded.push((h1 << 4) | h2);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn validate_replacement_precondition(
    state: &GraphState,
    if_match: &str,
) -> Result<(), FeederError> {
    let trimmed = if_match.trim();

    // Reject weak entity tags
    if trimmed.starts_with("W/") || trimmed.starts_with("w/") {
        return Err(FeederError::PreconditionFailed(
            "weak entity tags are not supported in If-Match header".into(),
        ));
    }

    // Reject wildcard
    if trimmed == "*" {
        return Err(FeederError::PreconditionFailed(
            "wildcard If-Match is not supported for replacement import".into(),
        ));
    }

    // Reject list forms (multiple entity tags)
    if trimmed.contains(',') {
        return Err(FeederError::PreconditionFailed(
            "multiple entity tags in If-Match header are not supported".into(),
        ));
    }

    // Must be enclosed in double quotes with length at least 2
    if !trimmed.starts_with('"') || !trimmed.ends_with('"') || trimmed.len() < 2 {
        return Err(FeederError::PreconditionFailed(
            "If-Match header must be enclosed in double quotes".into(),
        ));
    }

    // Extract content inside the quotes
    let inner = &trimmed[1..trimmed.len() - 1];

    // Reject repeated quotes or embedded quotes (e.g. ""1.0.pos"")
    if inner.contains('"') {
        return Err(FeederError::PreconditionFailed(
            "invalid repeated or embedded quotes in If-Match header".into(),
        ));
    }

    let parts: Vec<&str> = inner.split('.').collect();
    if parts.len() != 3 {
        return Err(FeederError::PreconditionFailed(
            "malformed composite If-Match header; expected '<generation>.<state_version>.<percent_encoded_source_position>'".into(),
        ));
    }

    if parts[0].is_empty() || !parts[0].bytes().all(|b| b.is_ascii_digit()) {
        return Err(FeederError::PreconditionFailed(
            "non-numeric generation in If-Match header".into(),
        ));
    }
    let expected_gen: u64 = parts[0].parse().map_err(|_| {
        FeederError::PreconditionFailed("generation in If-Match header exceeds u64 bounds".into())
    })?;

    let expected_ver = parts[1];
    if expected_ver.is_empty() || !expected_ver.bytes().all(|b| b.is_ascii_digit()) {
        return Err(FeederError::PreconditionFailed(
            "invalid state_version in If-Match header; must be decimal integer".into(),
        ));
    }

    let expected_pos_encoded = parts[2];

    let live_ver: String = state
        .version
        .iter()
        .rev()
        .map(|d| char::from(b'0' + d))
        .collect();
    let live_pos = state.source_position.as_deref().unwrap_or("");
    let live_pos_encoded = percent_encode_position(live_pos);

    if expected_gen != state.generation
        || expected_ver != live_ver
        || expected_pos_encoded != live_pos_encoded
    {
        return Err(FeederError::PreconditionFailed(format!(
            "If-Match '\"{inner}\"' does not match current state '\"{}.{}.{}\"'",
            state.generation, live_ver, live_pos_encoded
        )));
    }
    Ok(())
}

/// Feeder protocol or admission errors.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeederError {
    ImportInProgress,
    Unavailable,
    ModeMismatch,
    StaleInstanceId(String),
    InvalidInstanceId(String),
    InvalidGraphId(String),
    SessionRequired,
    FencedSession,
    LeaseExpired,
    GraphAlreadyInitialized,
    GraphUninitialized,
    GraphDesynchronized,
    GenerationExhausted,
    StaleGeneration(String),
    SourceMismatch(String),
    SourceEpochMismatch(String),
    OutOfOrderSequence { expected: u64, received: u64 },
    SourcePredecessorMismatch(String),
    ConflictingDuplicate(String),
    UnverifiableDuplicate(String),
    SequenceExhausted,
    PreconditionFailed(String),
    InvalidLeaseDuration,
    MalformedJson,
    UnprocessableJson(String),
    PayloadTooLarge,
    Domain(&'static str, String),
    InvalidRequest(String),
    BarrierPositionMismatch { expected: String, received: String },
    BarrierNotReached { expected: u64, received: u64 },
    SupersededBarrier { expected: u64, received: u64 },
}

/// Feeder barrier declaration request payload.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BarrierRequest {
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) generation: u64,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) barrier_delivery_sequence: u64,
    #[serde(default)]
    pub(crate) barrier_source_position: Option<String>,
}

/// Feeder barrier declaration success response payload.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct BarrierResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) graph_id: &'static str,
    pub(crate) generation: u64,
    pub(crate) state_version: String,
    pub(crate) applied_source_position: String,
    pub(crate) status: &'static str,
}

#[derive(Debug)]
pub(crate) enum FeederEventsResult {
    Applied {
        response: crate::api::FeederEventsResponse,
        metadata: ResponseMetadata,
    },
    Duplicate {
        response: crate::api::FeederDuplicateResponse,
        metadata: ResponseMetadata,
    },
    PartialFailure {
        failure: crate::api::PartialBatchFailure,
        metadata: ResponseMetadata,
        status: axum::http::StatusCode,
    },
}

impl From<ServiceError> for FeederError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Busy => Self::ImportInProgress,
            ServiceError::Unavailable => Self::Unavailable,
        }
    }
}

/// Shared graph state plus a strict limit on outstanding worker jobs.
#[derive(Clone)]
pub(crate) struct Service {
    graph: Arc<Mutex<GraphState>>,
    permits: Arc<Semaphore>,
    import_permits: Arc<Semaphore>,
    // Lock-free mirror of GraphState::failed so readiness never waits for graph work.
    unavailable: Arc<AtomicBool>,
    published_readiness: Arc<AtomicU64>,
    lifecycle: Lifecycle,
    info: crate::ServerInfo,
    mode: ServerMode,
    instance_id: String,
    max_import_body_bytes: usize,
}

/// Graph and revision share one mutex: snapshots cannot split a batch or observe
/// a mutation without its revision increment. Version starts at zero even when
/// embedding a pre-populated graph; it counts this service instance's changes.
struct GraphState {
    graph: ConnectivityGraph,
    // parking_lot does not poison on unwind. Mark each job before execution;
    // only normal completion clears this bit, while still holding the lock.
    // Queued jobs therefore cannot observe state left by a panicked worker.
    failed: bool,
    // Little-endian decimal digits avoid wraparound or saturation. Increment is
    // amortized O(1), worst-case O(log M), after M effective mutations. Storage
    // and snapshot conversion are O(log M); this adds no core-engine state.
    version: Vec<u8>,
    generation: u64,
    source_id: Option<String>,
    source_epoch: Option<String>,
    source_position: Option<String>,
    applied_delivery_sequence: u64,
    feeder_session_id: Option<String>,
    feeder_epoch: u64,
    lease_duration: Duration,
    lease_deadline: Option<Instant>,
    sync_status: SyncStatus,
    publication_seq: u16,
    last_expired_epoch: Option<u64>,
    dedup_window: DedupWindow,
}

impl GraphState {
    fn new(graph: ConnectivityGraph, dedup_capacity: usize) -> Self {
        Self {
            graph,
            failed: false,
            version: vec![0],
            generation: 0,
            source_id: None,
            source_epoch: None,
            source_position: None,
            applied_delivery_sequence: 0,
            feeder_session_id: None,
            feeder_epoch: 0,
            lease_duration: Duration::from_secs(10),
            lease_deadline: None,
            sync_status: SyncStatus::Uninitialized,
            publication_seq: 0,
            last_expired_epoch: None,
            dedup_window: DedupWindow::new(dedup_capacity),
        }
    }

    fn current_readiness_word(&mut self) -> u64 {
        let deadline_ms = self.lease_deadline.map(instant_to_millis).unwrap_or(0);
        self.publication_seq = self.publication_seq.wrapping_add(1);
        PublishedReadiness::pack(self.sync_status, self.publication_seq, deadline_ms)
    }

    fn apply(&mut self, operation: Operation) -> Outcome {
        let outcome = operation.apply(&mut self.graph);
        if matches!(outcome, Outcome::Changed { changed: true }) {
            self.increment_version();
        }
        outcome
    }

    fn increment_version(&mut self) {
        for digit in &mut self.version {
            if *digit < 9 {
                *digit += 1;
                return;
            }
            *digit = 0;
        }
        self.version.push(1);
    }

    fn version_string(&self) -> String {
        self.version
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + digit))
            .collect()
    }

    fn is_lease_expired_internal(&self) -> bool {
        if let Some(deadline) = self.lease_deadline {
            monotonic_now() > deadline
        } else {
            true
        }
    }

    fn effective_sync_status(&self) -> SyncStatus {
        if (self.sync_status == SyncStatus::Ready || self.sync_status == SyncStatus::CatchingUp)
            && self.is_lease_expired_internal()
        {
            SyncStatus::Stale
        } else {
            self.sync_status
        }
    }

    fn check_lease_expiry(&mut self) -> bool {
        if self.is_lease_expired_internal() {
            if self.sync_status == SyncStatus::Ready || self.sync_status == SyncStatus::CatchingUp {
                self.sync_status = SyncStatus::Stale;
            }
            self.last_expired_epoch = Some(self.feeder_epoch);
            true
        } else {
            false
        }
    }

    fn snapshot(&self, mode: ServerMode, instance_id: &str) -> GraphSnapshot {
        let node_count = self.graph.node_count();
        let edge_count = self.graph.edge_count();
        let limits = self.graph.limits();
        let state_version = self.version_string();
        let (instance_id, generation, source_position, sync_status) = match mode {
            ServerMode::Standalone => (None, None, None, None),
            ServerMode::SourceManaged => (
                Some(instance_id.to_string()),
                Some(self.generation),
                self.source_position.clone(),
                Some(self.effective_sync_status()),
            ),
        };
        GraphSnapshot {
            node_count,
            edge_count,
            max_nodes: limits.max_nodes,
            max_edges: limits.max_edges,
            remaining_nodes: limits.max_nodes.map(|limit| limit - node_count),
            remaining_edges: limits.max_edges.map(|limit| limit - edge_count),
            state_version,
            instance_id,
            generation,
            source_position,
            sync_status,
        }
    }

    fn response_metadata(&self, mode: ServerMode, instance_id: &str) -> ResponseMetadata {
        let state_version = self.version_string();
        let (generation, source_position, sync_status) = match mode {
            ServerMode::Standalone => (0, None, None),
            ServerMode::SourceManaged => (
                self.generation,
                self.source_position.clone(),
                Some(self.effective_sync_status()),
            ),
        };
        ResponseMetadata {
            instance_id: instance_id.to_string(),
            graph_id: "default",
            generation,
            state_version,
            source_position,
            sync_status,
        }
    }

    fn register_session(
        &mut self,
        req: SessionRequest,
        instance_id: String,
    ) -> Result<SessionResponse, FeederError> {
        let lease_duration = Duration::from_millis(req.lease_duration_ms);
        let now = monotonic_now();
        let was_expired = self.check_lease_expiry();

        if let Some(ref active_session) = self.feeder_session_id {
            if active_session == &req.feeder_session_id {
                if req.feeder_epoch < self.feeder_epoch {
                    return Err(FeederError::FencedSession);
                }
                if req.feeder_epoch > self.feeder_epoch {
                    if self.sync_status == SyncStatus::Ready {
                        self.sync_status = SyncStatus::Stale;
                    }
                } else if was_expired && self.sync_status == SyncStatus::Ready {
                    self.sync_status = SyncStatus::Stale;
                }
                self.feeder_epoch = req.feeder_epoch;
                self.lease_duration = lease_duration;
                self.lease_deadline = Some(now + lease_duration);
            } else {
                if req.feeder_epoch <= self.feeder_epoch {
                    return Err(FeederError::FencedSession);
                }
                if self.sync_status == SyncStatus::Ready {
                    self.sync_status = SyncStatus::Stale;
                }
                self.feeder_session_id = Some(req.feeder_session_id.clone());
                self.feeder_epoch = req.feeder_epoch;
                self.lease_duration = lease_duration;
                self.lease_deadline = Some(now + lease_duration);
            }
        } else {
            self.feeder_session_id = Some(req.feeder_session_id.clone());
            self.feeder_epoch = req.feeder_epoch;
            self.lease_duration = lease_duration;
            self.lease_deadline = Some(now + lease_duration);
        }

        Ok(SessionResponse {
            result: "session_acquired",
            instance_id,
            feeder_session_id: req.feeder_session_id,
            feeder_epoch: req.feeder_epoch,
            lease_duration_ms: req.lease_duration_ms,
            generation: self.generation,
            applied_delivery_sequence: self.applied_delivery_sequence,
            applied_source_position: self.source_position.clone(),
            status: self.sync_status,
        })
    }

    fn heartbeat(
        &mut self,
        req: HeartbeatRequest,
        instance_id: String,
    ) -> Result<HeartbeatResponse, FeederError> {
        let Some(ref active_session) = self.feeder_session_id else {
            return Err(FeederError::SessionRequired);
        };
        if active_session != &req.feeder_session_id || req.feeder_epoch != self.feeder_epoch {
            return Err(FeederError::FencedSession);
        }

        let _ = self.check_lease_expiry();

        let now = monotonic_now();
        self.lease_deadline = Some(now + self.lease_duration);

        Ok(HeartbeatResponse {
            result: "heartbeat_acknowledged",
            instance_id,
            feeder_session_id: req.feeder_session_id,
            feeder_epoch: req.feeder_epoch,
            lease_expires_in_ms: self.lease_duration.as_millis() as u64,
            status: self.sync_status,
        })
    }

    fn validate_session_and_lease(
        &mut self,
        session_id: &str,
        epoch: u64,
    ) -> Result<(), FeederError> {
        let Some(ref active_session) = self.feeder_session_id else {
            return Err(FeederError::SessionRequired);
        };
        if active_session != session_id || epoch != self.feeder_epoch {
            return Err(FeederError::FencedSession);
        }

        let now = monotonic_now();
        if let Some(deadline) = self.lease_deadline {
            if now > deadline {
                if self.sync_status == SyncStatus::Ready
                    || self.sync_status == SyncStatus::CatchingUp
                {
                    self.sync_status = SyncStatus::Stale;
                }
                self.last_expired_epoch = Some(self.feeder_epoch);
                return Err(FeederError::LeaseExpired);
            }
        } else {
            return Err(FeederError::LeaseExpired);
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fail_batch_or_error(
        &self,
        is_batch: bool,
        mode: ServerMode,
        instance_id: &str,
        req_instance_id: &str,
        idx: usize,
        seq: u64,
        event_id: &str,
        code: &'static str,
        reason: &str,
        err_code: &'static str,
        err_msg: String,
        status: axum::http::StatusCode,
        single_err: FeederError,
    ) -> Result<FeederEventsResult, FeederError> {
        if is_batch {
            Ok(FeederEventsResult::PartialFailure {
                failure: PartialBatchFailure {
                    result: "partial_failure",
                    code,
                    message: format!(
                        "batch stopped at batch index {idx} (global sequence {seq}) due to {reason}"
                    ),
                    instance_id: req_instance_id.to_string(),
                    generation: self.generation,
                    applied_delivery_sequence: self.applied_delivery_sequence,
                    applied_source_position: self.source_position.clone().unwrap_or_default(),
                    state_version: self.version_string(),
                    failed_batch_index: idx,
                    failed_delivery_sequence: seq,
                    failed_event_id: event_id.to_string(),
                    error: BatchItemError {
                        code: err_code,
                        message: err_msg,
                    },
                },
                metadata: self.response_metadata(mode, instance_id),
                status,
            })
        } else {
            Err(single_err)
        }
    }

    fn declare_barrier(
        &mut self,
        req: BarrierRequest,
        mode: ServerMode,
        instance_id: &str,
    ) -> Result<(BarrierResponse, ResponseMetadata), FeederError> {
        if self.failed {
            return Err(FeederError::Unavailable);
        }

        self.validate_session_and_lease(&req.feeder_session_id, req.feeder_epoch)?;

        if req.generation != self.generation {
            return Err(FeederError::StaleGeneration(format!(
                "request generation {} does not match current generation {}",
                req.generation, self.generation
            )));
        }

        // Decision table: B3, B4
        if self.generation == 0
            || self.sync_status == SyncStatus::Uninitialized
            || self.sync_status == SyncStatus::Loading
        {
            return Err(FeederError::GraphUninitialized);
        }

        // Decision table: B5
        if self.sync_status == SyncStatus::Desynchronized {
            return Err(FeederError::GraphDesynchronized);
        }

        // Decision table: B8 (Premature sequence)
        if req.barrier_delivery_sequence > self.applied_delivery_sequence {
            return Err(FeederError::BarrierNotReached {
                expected: self.applied_delivery_sequence,
                received: req.barrier_delivery_sequence,
            });
        }

        // Decision table: B9 (Older sequence)
        if req.barrier_delivery_sequence < self.applied_delivery_sequence {
            return Err(FeederError::SupersededBarrier {
                expected: self.applied_delivery_sequence,
                received: req.barrier_delivery_sequence,
            });
        }

        // Decision table: B7 (Position mismatch at equal sequence)
        let current_pos = self.source_position.as_deref();
        let req_pos = req.barrier_source_position.as_deref();
        if req_pos != current_pos {
            return Err(FeederError::BarrierPositionMismatch {
                expected: current_pos.unwrap_or_default().to_string(),
                received: req_pos.unwrap_or_default().to_string(),
            });
        }

        // Recovery fencing: synchronization proof after lease expiry requires
        // registering a strictly newer epoch (> last_expired_epoch).
        if self
            .last_expired_epoch
            .is_some_and(|expired| req.feeder_epoch <= expired)
        {
            return Err(FeederError::FencedSession);
        }

        // Exact match: B1 and B2
        let result_str = match self.sync_status {
            SyncStatus::Ready => "already_synchronized",
            SyncStatus::CatchingUp | SyncStatus::Stale => {
                self.sync_status = SyncStatus::Ready;
                "synchronized"
            }
            _ => unreachable!(),
        };
        self.last_expired_epoch = None;

        let metadata = self.response_metadata(mode, instance_id);
        let res = BarrierResponse {
            result: result_str,
            instance_id: req.instance_id,
            graph_id: "default",
            generation: self.generation,
            state_version: self.version_string(),
            applied_source_position: self.source_position.clone().unwrap_or_default(),
            status: "ready",
        };
        Ok((res, metadata))
    }

    fn apply_events(
        &mut self,
        req: FeederEventsRequest,
        mode: ServerMode,
        instance_id: &str,
    ) -> Result<FeederEventsResult, FeederError> {
        if req.events.is_empty() {
            return Err(FeederError::InvalidRequest(
                "events array must not be empty".into(),
            ));
        }
        if req.events.len() > crate::MAX_BATCH_OPERATIONS {
            return Err(FeederError::InvalidRequest(format!(
                "batches must contain 1 to {} operations",
                crate::MAX_BATCH_OPERATIONS
            )));
        }

        self.validate_session_and_lease(&req.feeder_session_id, req.feeder_epoch)?;

        if self.generation == 0 {
            return Err(FeederError::GraphUninitialized);
        }
        if req.generation != self.generation {
            return Err(FeederError::StaleGeneration(format!(
                "request generation {} does not match server generation {}",
                req.generation, self.generation
            )));
        }

        if self.sync_status == SyncStatus::Desynchronized {
            return Err(FeederError::GraphDesynchronized);
        }

        if let Some(ref live_source_id) = self.source_id
            && live_source_id != &req.source_id
        {
            return Err(FeederError::SourceMismatch(format!(
                "request source_id '{}' does not match server source_id '{}'",
                req.source_id, live_source_id
            )));
        }
        if let Some(ref live_source_epoch) = self.source_epoch
            && live_source_epoch != &req.source_epoch
        {
            self.sync_status = SyncStatus::Desynchronized;
            return Err(FeederError::SourceEpochMismatch(format!(
                "request source_epoch '{}' does not match server source_epoch '{}'",
                req.source_epoch, live_source_epoch
            )));
        }

        for event in &req.events {
            if event.event_id.len() > 64 {
                return Err(FeederError::InvalidRequest(
                    "event_id must not exceed 64 bytes".into(),
                ));
            }
            if event.source_position.len() > 256 {
                return Err(FeederError::InvalidRequest(
                    "source_position must not exceed 256 bytes".into(),
                ));
            }
            if event.prev_source_position.len() > 256 {
                return Err(FeederError::InvalidRequest(
                    "prev_source_position must not exceed 256 bytes".into(),
                ));
            }
            let _ = CanonicalOp::from_feeder_event(event)?;
        }

        let total_events = req.events.len();
        let mut newly_applied_count = 0usize;
        let mut last_duplicate_entry: Option<DedupEntry> = None;

        for (idx, event) in req.events.into_iter().enumerate() {
            let is_batch = newly_applied_count > 0 || total_events > 1;
            let canonical_op = CanonicalOp::from_feeder_event(&event)?;
            let seq = event.delivery_sequence;

            if seq <= self.applied_delivery_sequence {
                match self
                    .dedup_window
                    .check_sequence(seq, self.applied_delivery_sequence)
                {
                    DedupLookup::Evicted => {
                        let err_msg = format!(
                            "delivery sequence {seq} was evicted from deduplication window"
                        );
                        return self.fail_batch_or_error(
                            is_batch,
                            mode,
                            instance_id,
                            &req.instance_id,
                            idx,
                            seq,
                            &event.event_id,
                            "unverifiable_duplicate",
                            "evicted duplicate",
                            "unverifiable_duplicate",
                            err_msg.clone(),
                            axum::http::StatusCode::CONFLICT,
                            FeederError::UnverifiableDuplicate(err_msg),
                        );
                    }
                    DedupLookup::InWindow(retained) => {
                        if retained.event_id != event.event_id {
                            self.sync_status = SyncStatus::Desynchronized;
                            let err_msg = format!(
                                "retained sequence {seq} has event_id '{}', received '{}'",
                                retained.event_id, event.event_id
                            );
                            return self.fail_batch_or_error(
                                is_batch,
                                mode,
                                instance_id,
                                &req.instance_id,
                                idx,
                                seq,
                                &event.event_id,
                                "conflicting_duplicate",
                                "conflicting event ID",
                                "conflicting_duplicate",
                                err_msg.clone(),
                                axum::http::StatusCode::CONFLICT,
                                FeederError::ConflictingDuplicate(err_msg),
                            );
                        }

                        let is_canonical_match = retained.source_position == event.source_position
                            && retained.canonical_op == canonical_op;
                        if !is_canonical_match {
                            self.sync_status = SyncStatus::Desynchronized;
                            let err_msg = format!(
                                "conflicting duplicate at sequence {seq}: canonical content or source position altered"
                            );
                            return self.fail_batch_or_error(
                                is_batch,
                                mode,
                                instance_id,
                                &req.instance_id,
                                idx,
                                seq,
                                &event.event_id,
                                "conflicting_duplicate",
                                "canonical conflict",
                                "conflicting_duplicate",
                                err_msg.clone(),
                                axum::http::StatusCode::CONFLICT,
                                FeederError::ConflictingDuplicate(err_msg),
                            );
                        }

                        last_duplicate_entry = Some(retained.clone());
                        continue;
                    }
                    DedupLookup::Future => unreachable!(),
                }
            } else if let Some(expected_seq) = self.applied_delivery_sequence.checked_add(1) {
                if seq != expected_seq {
                    let expected = expected_seq;
                    return self.fail_batch_or_error(
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        "out_of_order_sequence",
                        "out-of-order sequence",
                        "out_of_order_sequence",
                        format!("expected delivery sequence {expected}, received {seq}"),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::OutOfOrderSequence {
                            expected,
                            received: seq,
                        },
                    );
                }

                if event.prev_delivery_sequence != self.applied_delivery_sequence {
                    self.sync_status = SyncStatus::Desynchronized;
                    let err_msg = format!(
                        "prev_delivery_sequence {} does not match applied sequence {}",
                        event.prev_delivery_sequence, self.applied_delivery_sequence
                    );
                    return self.fail_batch_or_error(
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        "source_predecessor_mismatch",
                        "predecessor sequence mismatch",
                        "source_predecessor_mismatch",
                        err_msg.clone(),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::SourcePredecessorMismatch(err_msg),
                    );
                }

                let live_pos = self.source_position.as_deref().unwrap_or("");
                if event.prev_source_position != live_pos {
                    self.sync_status = SyncStatus::Desynchronized;
                    let err_msg = format!(
                        "prev_source_position '{}' does not match applied source position '{}'",
                        event.prev_source_position, live_pos
                    );
                    return self.fail_batch_or_error(
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        "source_predecessor_mismatch",
                        "source predecessor mismatch",
                        "source_predecessor_mismatch",
                        err_msg.clone(),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::SourcePredecessorMismatch(err_msg),
                    );
                }

                if self.dedup_window.contains_event_id(&event.event_id) {
                    self.sync_status = SyncStatus::Desynchronized;
                    let err_msg = format!(
                        "event_id '{}' is already present in retained deduplication window",
                        event.event_id
                    );
                    return self.fail_batch_or_error(
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        "conflicting_duplicate",
                        "duplicate event ID",
                        "conflicting_duplicate",
                        err_msg.clone(),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::ConflictingDuplicate(err_msg),
                    );
                }

                let next_seq = expected_seq;

                let changed_result = match &canonical_op {
                    CanonicalOp::AddNode { node } => {
                        self.graph.add_node(*node).map_err(|err| match err {
                            GraphError::NodeLimitExceeded { limit } => (
                                "node_limit_exceeded",
                                format!("node limit {limit} exceeded"),
                            ),
                            _ => ("invalid_node", err.to_string()),
                        })
                    }
                    CanonicalOp::Link { source, target } => {
                        if source == target {
                            Err((
                                "self_loop",
                                "self-loops are forbidden in simple undirected graphs".to_string(),
                            ))
                        } else {
                            self.graph.link(*source, *target).map_err(|err| match err {
                                GraphError::EdgeLimitExceeded { limit } => (
                                    "edge_limit_exceeded",
                                    format!("edge limit {limit} exceeded"),
                                ),
                                GraphError::NodeLimitExceeded { limit } => (
                                    "node_limit_exceeded",
                                    format!("node limit {limit} exceeded"),
                                ),
                                _ => ("invalid_edge", err.to_string()),
                            })
                        }
                    }
                    CanonicalOp::Cut { source, target } => {
                        if source == target {
                            Err((
                                "self_loop",
                                "self-loops are forbidden in simple undirected graphs".to_string(),
                            ))
                        } else {
                            self.graph
                                .cut(*source, *target)
                                .map_err(|err| ("invalid_edge", err.to_string()))
                        }
                    }
                    CanonicalOp::Progress => Ok(false),
                };

                let changed = match changed_result {
                    Ok(ch) => ch,
                    Err((err_code, err_msg)) => {
                        return self.fail_batch_or_error(
                            is_batch,
                            mode,
                            instance_id,
                            &req.instance_id,
                            idx,
                            seq,
                            &event.event_id,
                            "event_failed",
                            "domain error",
                            err_code,
                            err_msg.clone(),
                            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                            FeederError::Domain(err_code, err_msg),
                        );
                    }
                };

                if changed && !matches!(canonical_op, CanonicalOp::Progress) {
                    self.increment_version();
                }
                self.applied_delivery_sequence = next_seq;
                self.source_position = Some(event.source_position.clone());
                self.dedup_window.record(DedupEntry {
                    delivery_sequence: next_seq,
                    event_id: event.event_id.clone(),
                    source_position: event.source_position.clone(),
                    canonical_op,
                    changed,
                });
                newly_applied_count += 1;
            } else {
                // Applied sequence counter is u64::MAX and cannot be incremented.
                // Note: since seq is u64, any seq > u64::MAX is impossible on the wire.
                // This branch serves as an internal defensive guard against sequence overflow.
                return Err(FeederError::SequenceExhausted);
            }
        }

        if total_events == 1 && newly_applied_count == 0 {
            let dup = last_duplicate_entry.expect("retained duplicate entry");
            let response = FeederDuplicateResponse {
                result: "duplicate",
                status: "already_applied",
                instance_id: req.instance_id,
                generation: self.generation,
                event_id: dup.event_id,
                delivery_sequence: dup.delivery_sequence,
                historical_outcome: HistoricalOutcome {
                    changed: dup.changed,
                },
                current_applied_delivery_sequence: self.applied_delivery_sequence,
                current_applied_source_position: self.source_position.clone().unwrap_or_default(),
                current_state_version: self.version_string(),
            };
            return Ok(FeederEventsResult::Duplicate {
                response,
                metadata: self.response_metadata(mode, instance_id),
            });
        }

        let response = FeederEventsResponse {
            result: "applied",
            instance_id: req.instance_id,
            generation: self.generation,
            applied_delivery_sequence: self.applied_delivery_sequence,
            applied_source_position: self.source_position.clone().unwrap_or_default(),
            state_version: self.version_string(),
            events_applied: newly_applied_count,
        };
        Ok(FeederEventsResult::Applied {
            response,
            metadata: self.response_metadata(mode, instance_id),
        })
    }

    fn declare_gap(
        &mut self,
        req: GapRequest,
        mode: ServerMode,
        instance_id: &str,
    ) -> Result<(GapResponse, ResponseMetadata), FeederError> {
        self.validate_session_and_lease(&req.feeder_session_id, req.feeder_epoch)?;

        if self.generation == 0 {
            return Err(FeederError::GraphUninitialized);
        }
        if req.generation != self.generation {
            return Err(FeederError::StaleGeneration(format!(
                "request generation {} does not match server generation {}",
                req.generation, self.generation
            )));
        }

        tracing::warn!(
            failed_delivery_sequence = req.failed_delivery_sequence,
            reason = ?req.reason,
            "feeder declared unrecoverable source gap; transitioning graph to Desynchronized"
        );
        self.sync_status = SyncStatus::Desynchronized;
        let metadata = self.response_metadata(mode, instance_id);
        let res = GapResponse {
            result: "gap_acknowledged",
            instance_id: req.instance_id,
            generation: self.generation,
            status: SyncStatus::Desynchronized,
            action_required: "full_rebuild_required",
        };
        Ok((res, metadata))
    }
}

/// Owned coherent state, serialized after releasing the graph lock.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GraphSnapshot {
    node_count: usize,
    edge_count: usize,
    max_nodes: Option<usize>,
    max_edges: Option<usize>,
    remaining_nodes: Option<usize>,
    remaining_edges: Option<usize>,
    state_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_position: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_status: Option<SyncStatus>,
}

/// Coherent response metadata captured under the graph lock alongside outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponseMetadata {
    pub(crate) instance_id: String,
    pub(crate) graph_id: &'static str,
    pub(crate) generation: u64,
    pub(crate) state_version: String,
    pub(crate) source_position: Option<String>,
    pub(crate) sync_status: Option<SyncStatus>,
}

impl ResponseMetadata {
    /// Formats the composite ETag value enclosed in double quotes:
    /// `"<generation>.<state_version>.<percent_encoded_source_position>"`
    pub(crate) fn etag(&self) -> String {
        let pos_encoded = self
            .source_position
            .as_deref()
            .map(percent_encode_position)
            .unwrap_or_default();
        format!(
            "\"{}.{}.{}\"",
            self.generation, self.state_version, pos_encoded
        )
    }
}

/// Query/mutation outcomes paired with coherent graph metadata under one lock boundary.
#[derive(Debug)]
pub(crate) struct ExecutionResult {
    pub(crate) outcomes: Vec<Outcome>,
    pub(crate) metadata: ResponseMetadata,
}

impl std::ops::Deref for ExecutionResult {
    type Target = [Outcome];
    fn deref(&self) -> &Self::Target {
        &self.outcomes
    }
}

/// Graph snapshot paired with coherent response metadata under one lock boundary.
#[derive(Debug)]
pub(crate) struct StatsResult {
    pub(crate) snapshot: GraphSnapshot,
    pub(crate) metadata: ResponseMetadata,
}

impl std::ops::Deref for StatsResult {
    type Target = GraphSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

/// Admission failure or a worker that cannot safely use the graph.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServiceError {
    Busy,
    Unavailable,
}

/// Worker-owned RAII failure guard ensuring any panic during execution immediately
/// marks the service unavailable, even if the awaiting async caller was cancelled.
struct WorkerPanicGuard {
    unavailable: Arc<AtomicBool>,
    success: bool,
}

impl Drop for WorkerPanicGuard {
    fn drop(&mut self) {
        if !self.success && std::thread::panicking() {
            self.unavailable.store(true, Ordering::Release);
        }
    }
}

impl Service {
    /// Creates the service; zero capacity intentionally rejects every request.
    #[allow(dead_code)]
    pub(crate) fn new(graph: ConnectivityGraph, capacity: usize, lifecycle: Lifecycle) -> Self {
        let config = crate::ServerConfig::default();
        Self::with_config(graph, capacity, lifecycle, &config)
    }

    /// Creates the service with validated startup settings.
    pub(crate) fn with_config(
        graph: ConnectivityGraph,
        capacity: usize,
        lifecycle: Lifecycle,
        config: &crate::ServerConfig,
    ) -> Self {
        let engine = graph.engine();
        let mode = config.mode();
        let instance_id = fresh_instance_id();
        let max_import_body_bytes = config.max_import_body_bytes();

        let (mode_str, inst_id_opt) = match mode {
            ServerMode::Standalone => (None, None),
            ServerMode::SourceManaged => (Some("source_managed"), Some(instance_id.clone())),
        };

        Self {
            info: crate::ServerInfo {
                engine: engine.as_str(),
                experimental: engine.is_experimental(),
                max_pending_jobs: capacity,
                max_body_bytes: crate::MAX_BODY_BYTES,
                max_batch_operations: crate::MAX_BATCH_OPERATIONS,
                max_nodes: graph.limits().max_nodes,
                max_edges: graph.limits().max_edges,
                mode: mode_str,
                instance_id: inst_id_opt,
            },
            graph: Arc::new(Mutex::new(GraphState::new(
                graph,
                config.dedup_window_size(),
            ))),
            permits: Arc::new(Semaphore::new(capacity)),
            import_permits: Arc::new(Semaphore::new(1)),
            unavailable: Arc::new(AtomicBool::new(false)),
            published_readiness: Arc::new(AtomicU64::new(PublishedReadiness::pack(
                SyncStatus::Uninitialized,
                0,
                0,
            ))),
            lifecycle,
            mode,
            instance_id,
            max_import_body_bytes,
        }
    }

    /// Acquires the single import permit before reading request body.
    pub(crate) fn acquire_import_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, FeederError> {
        self.import_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                tracing::warn!("import capacity exhausted; rejecting with 503 import_in_progress");
                FeederError::ImportInProgress
            })
    }

    /// Returns the maximum allowed size in bytes for a graph import payload.
    pub(crate) fn max_import_body_bytes(&self) -> usize {
        self.max_import_body_bytes
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn seed_version_for_test(&self, decimal_str: &str) {
        let mut state = self.graph.lock();
        state.version = decimal_str.bytes().rev().map(|b| b - b'0').collect();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn seed_sequence_for_test(&self, sequence: u64) {
        let mut state = self.graph.lock();
        state.applied_delivery_sequence = sequence;
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn seed_generation_for_test(&self, generation: u64) {
        let mut state = self.graph.lock();
        state.generation = generation;
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn applied_delivery_sequence(&self) -> u64 {
        self.graph.lock().applied_delivery_sequence
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn set_sync_status_for_test(&self, status: SyncStatus) {
        let mut state = self.graph.lock();
        state.sync_status = status;
        let current = self.published_readiness();
        self.published_readiness.store(
            PublishedReadiness::pack(status, current.seq.wrapping_add(1), current.deadline_millis),
            Ordering::Release,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn set_published_readiness_for_test(&self, status: SyncStatus, deadline_ms: u64) {
        let mut state = self.graph.lock();
        state.sync_status = status;
        let current = self.published_readiness();
        self.published_readiness.store(
            PublishedReadiness::pack(status, current.seq.wrapping_add(1), deadline_ms),
            Ordering::Release,
        );
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn mark_unavailable_for_test(&self) {
        self.unavailable.store(true, Ordering::Release);
    }

    /// Reports whether a worker failure permanently disabled graph work.
    pub(crate) fn is_unavailable(&self) -> bool {
        self.unavailable.load(Ordering::Acquire)
    }

    /// Returns the operating mode of this service.
    pub(crate) fn mode(&self) -> ServerMode {
        self.mode
    }

    /// Returns the process instance identifier.
    pub(crate) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Reads the packed readiness word lock-free.
    pub(crate) fn published_readiness_word(&self) -> u64 {
        self.published_readiness.load(Ordering::Acquire)
    }

    /// Reads the current published readiness state lock-free.
    pub(crate) fn published_readiness(&self) -> PublishedReadiness {
        PublishedReadiness::unpack(self.published_readiness_word())
    }

    /// Reads the current synchronization status lock-free.
    pub(crate) fn sync_status(&self) -> SyncStatus {
        self.published_readiness().status
    }

    /// Reports whether the feeder lease has expired without taking the graph lock.
    #[allow(dead_code)]
    pub(crate) fn is_lease_expired(&self) -> bool {
        let published = self.published_readiness();
        if published.deadline_millis == 0 {
            return true;
        }
        now_to_millis() > published.deadline_millis
    }

    /// Reads effective immutable settings without waiting for graph work.
    pub(crate) fn info(&self) -> crate::ServerInfo {
        self.info.clone()
    }

    /// Registers a feeder session under graph lock.
    pub(crate) async fn register_session(
        &self,
        req: SessionRequest,
    ) -> Result<SessionResponse, FeederError> {
        if self.mode != ServerMode::SourceManaged {
            return Err(FeederError::ModeMismatch);
        }
        if req.instance_id != self.instance_id {
            return Err(FeederError::StaleInstanceId(format!(
                "request instance_id '{}' does not match server instance_id '{}'",
                req.instance_id, self.instance_id
            )));
        }
        if req.lease_duration_ms < 1000 || req.lease_duration_ms > 60000 {
            return Err(FeederError::InvalidLeaseDuration);
        }
        let published_atomic = self.published_readiness.clone();
        let instance_id = self.instance_id.clone();
        self.run(move |state| {
            let res = state.register_session(req, instance_id)?;
            let word = state.current_readiness_word();
            published_atomic.store(word, Ordering::Release);
            Ok(res)
        })
        .await
        .map_err(FeederError::from)?
    }

    /// Renews the active feeder lease under graph lock.
    pub(crate) async fn heartbeat(
        &self,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse, FeederError> {
        if self.mode != ServerMode::SourceManaged {
            return Err(FeederError::ModeMismatch);
        }
        if req.instance_id != self.instance_id {
            return Err(FeederError::StaleInstanceId(format!(
                "request instance_id '{}' does not match server instance_id '{}'",
                req.instance_id, self.instance_id
            )));
        }
        let published_atomic = self.published_readiness.clone();
        let instance_id = self.instance_id.clone();
        self.run(move |state| {
            let res = state.heartbeat(req, instance_id)?;
            let word = state.current_readiness_word();
            published_atomic.store(word, Ordering::Release);
            Ok(res)
        })
        .await
        .map_err(FeederError::from)?
    }

    /// Atomically constructs and publishes a new graph topology on a blocking worker.
    pub(crate) async fn import(
        &self,
        permit: tokio::sync::OwnedSemaphorePermit,
        raw_bytes: axum::body::Bytes,
        if_match: Option<String>,
    ) -> Result<ImportResponse, FeederError> {
        let job_guard = self.lifecycle.track_job();
        let lifecycle = self.lifecycle.clone();
        let graph = self.graph.clone();
        let unavailable = self.unavailable.clone();
        let failure_flag = self.unavailable.clone();
        let published_atomic = self.published_readiness.clone();
        let engine = self.info.engine;
        let limits = knotrel_core::GraphLimits {
            max_nodes: self.info.max_nodes,
            max_edges: self.info.max_edges,
        };
        let instance_id = self.instance_id.clone();

        let is_initial = {
            let state = self.graph.lock();
            state.generation == 0
        };
        if is_initial {
            let current = PublishedReadiness::unpack(published_atomic.load(Ordering::Acquire));
            let loading_word = PublishedReadiness::pack(
                SyncStatus::Loading,
                current.seq.wrapping_add(1),
                current.deadline_millis,
            );
            published_atomic.store(loading_word, Ordering::Release);
        }

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _job_guard = job_guard;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: unavailable.clone(),
                success: false,
            };

            struct InitialLoadingGuard {
                published: Arc<AtomicU64>,
                is_initial: bool,
                success: bool,
            }
            impl Drop for InitialLoadingGuard {
                fn drop(&mut self) {
                    if self.is_initial && !self.success {
                        let current_word = self.published.load(Ordering::Acquire);
                        let current = PublishedReadiness::unpack(current_word);
                        if current.status == SyncStatus::Loading {
                            let uninit_word = PublishedReadiness::pack(
                                SyncStatus::Uninitialized,
                                current.seq.wrapping_add(1),
                                current.deadline_millis,
                            );
                            let _ = self.published.compare_exchange(
                                current_word,
                                uninit_word,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            );
                        }
                    }
                }
            }

            let mut initial_guard = InitialLoadingGuard {
                published: published_atomic.clone(),
                is_initial,
                success: false,
            };

            lifecycle.check_worker_hook();

            // 1. Off-lock JSON parsing:
            let mut buffer = raw_bytes.to_vec();
            let req: ImportRequest = simd_json::from_slice(&mut buffer).map_err(|err| {
                if err.is_data() {
                    FeederError::UnprocessableJson(crate::api::request_error_message(&err))
                } else {
                    FeederError::MalformedJson
                }
            })?;

            if req.instance_id != instance_id {
                return Err(FeederError::StaleInstanceId(format!(
                    "request instance_id '{}' does not match server instance_id '{}'",
                    req.instance_id, instance_id
                )));
            }

            if req.graph_id != "default" {
                return Err(FeederError::InvalidGraphId(format!(
                    "unsupported graph_id '{}'; only 'default' is supported",
                    req.graph_id
                )));
            }

            // 2. Off-lock candidate building
            let engine_config = match engine {
                "euler-tour" => EngineConfig::EulerTour,
                "hdt" => EngineConfig::Hdt,
                _ => EngineConfig::CompactBfs,
            };
            let mut candidate = ConnectivityGraph::with_limits(engine_config, limits);

            for node in req.nodes {
                candidate.add_node(node.0).map_err(|err| match err {
                    GraphError::NodeLimitExceeded { limit } => FeederError::Domain(
                        "node_limit_exceeded",
                        format!("node limit {limit} exceeded"),
                    ),
                    _ => FeederError::Domain("invalid_node", err.to_string()),
                })?;
            }

            for edge in req.edges {
                if edge.source.0 == edge.target.0 {
                    return Err(FeederError::Domain(
                        "self_loop",
                        "self-loops are forbidden in simple undirected graphs".into(),
                    ));
                }
                candidate
                    .link(edge.source.0, edge.target.0)
                    .map_err(|err| match err {
                        GraphError::EdgeLimitExceeded { limit } => FeederError::Domain(
                            "edge_limit_exceeded",
                            format!("edge limit {limit} exceeded"),
                        ),
                        GraphError::NodeLimitExceeded { limit } => FeederError::Domain(
                            "node_limit_exceeded",
                            format!("node limit {limit} exceeded"),
                        ),
                        _ => FeederError::Domain("invalid_edge", err.to_string()),
                    })?;
            }

            // 3. Lock acquisition & atomic precondition verification
            let mut state = graph.lock();
            if state.failed || unavailable.load(Ordering::Acquire) {
                panic_guard.unavailable.store(true, Ordering::Release);
                return Err(FeederError::Unavailable);
            }

            state.validate_session_and_lease(&req.feeder_session_id, req.feeder_epoch)?;

            match req.mode {
                ImportMode::CreateOnly => {
                    if state.generation != 0 {
                        return Err(FeederError::GraphAlreadyInitialized);
                    }
                }
                ImportMode::Replace => {
                    if state.generation == 0 {
                        return Err(FeederError::PreconditionFailed(
                            "replacement requires an initialized graph (generation > 0)".into(),
                        ));
                    }
                    let Some(ref expected_etag) = if_match else {
                        return Err(FeederError::PreconditionFailed(
                            "If-Match header is mandatory when mode is replace".into(),
                        ));
                    };
                    validate_replacement_precondition(&state, expected_etag)?;
                }
            }

            let next_generation = state
                .generation
                .checked_add(1)
                .ok_or(FeederError::GenerationExhausted)?;

            // 4. ATOMIC PUBLICATION
            state.failed = true;
            state.graph = candidate;
            state.generation = next_generation;
            state.version = vec![0];
            state.source_id = Some(req.source_id);
            state.source_epoch = Some(req.source_epoch);
            state.source_position = Some(req.source_position.clone());
            state.applied_delivery_sequence = 0;
            state.sync_status = SyncStatus::CatchingUp;
            state.dedup_window.reset();
            state.failed = false;
            state.last_expired_epoch = None;

            initial_guard.success = true;
            panic_guard.success = true;
            let word = state.current_readiness_word();
            published_atomic.store(word, Ordering::Release);

            let node_count = state.graph.node_count();
            let edge_count = state.graph.edge_count();

            Ok(ImportResponse {
                result: "published",
                instance_id: req.instance_id,
                graph_id: req.graph_id,
                generation: state.generation,
                state_version: "0".to_string(),
                node_count,
                edge_count,
                source_position: req.source_position,
                status: "catching_up",
            })
        })
        .await
        .map_err(|error| {
            tracing::error!(panicked = error.is_panic(), "import worker thread failed");
            failure_flag.store(true, Ordering::Release);
            FeederError::Unavailable
        })?
    }

    /// Executes a request on a blocking worker, preserving batch isolation.
    ///
    /// Admission is nonblocking: rejected work never enters Tokio's blocking
    /// queue. Moving the permit into the closure bounds queued plus running
    /// jobs even if the caller drops its future. The mutex is acquired only
    /// inside that closure, keeping traversal and lock contention off async
    /// executor threads. One guard spans the entire batch, so no other request
    /// can observe its intermediate states. Per-operation errors do not roll
    /// back earlier updates. A panic permanently marks the graph unavailable,
    /// including to queued jobs, until this service instance is replaced.
    pub(crate) async fn execute(
        &self,
        operations: Vec<Operation>,
    ) -> Result<ExecutionResult, ServiceError> {
        let mode = self.mode;
        let instance_id = self.instance_id.clone();
        self.run(move |state| {
            if mode == ServerMode::SourceManaged {
                if state.generation == 0 || state.sync_status == SyncStatus::Uninitialized {
                    let outcomes = operations
                        .into_iter()
                        .map(|_| Outcome::Error {
                            code: "uninitialized",
                            message: "graph is uninitialized; baseline import required".into(),
                            expected_sequence: None,
                        })
                        .collect();
                    let metadata = state.response_metadata(mode, &instance_id);
                    return ExecutionResult { outcomes, metadata };
                }
                if state.sync_status == SyncStatus::Loading {
                    let outcomes = operations
                        .into_iter()
                        .map(|_| Outcome::Error {
                            code: "loading",
                            message: "graph baseline import is in progress".into(),
                            expected_sequence: None,
                        })
                        .collect();
                    let metadata = state.response_metadata(mode, &instance_id);
                    return ExecutionResult { outcomes, metadata };
                }
                if state.sync_status == SyncStatus::Desynchronized {
                    let outcomes = operations
                        .into_iter()
                        .map(|_| Outcome::Error {
                            code: "desynchronized",
                            message: "graph is desynchronized; full rebuild required".into(),
                            expected_sequence: None,
                        })
                        .collect();
                    let metadata = state.response_metadata(mode, &instance_id);
                    return ExecutionResult { outcomes, metadata };
                }
            }
            let outcomes = operations
                .into_iter()
                .map(|operation| state.apply(operation))
                .collect();
            let metadata = state.response_metadata(mode, &instance_id);
            ExecutionResult { outcomes, metadata }
        })
        .await
    }

    /// Takes a coherent snapshot through the same bounded worker path as writes.
    pub(crate) async fn stats(&self) -> Result<StatsResult, ServiceError> {
        let mode = self.mode;
        let instance_id = self.instance_id.clone();
        let current_sync_status = self.sync_status();
        self.run(move |state| {
            let mut snapshot = state.snapshot(mode, &instance_id);
            if current_sync_status == SyncStatus::Loading && state.generation == 0 {
                snapshot.sync_status = Some(SyncStatus::Loading);
            }
            let mut metadata = state.response_metadata(mode, &instance_id);
            if current_sync_status == SyncStatus::Loading && state.generation == 0 {
                metadata.sync_status = Some(SyncStatus::Loading);
            }
            StatsResult { snapshot, metadata }
        })
        .await
    }

    async fn execute_feeder_worker<T, F>(
        &self,
        worker_name: &'static str,
        mutate: bool,
        op: F,
    ) -> Result<T, FeederError>
    where
        T: Send + 'static,
        F: FnOnce(&mut GraphState, ServerMode, &str) -> Result<T, FeederError> + Send + 'static,
    {
        let mode = self.mode;
        let instance_id = self.instance_id.clone();
        let published_atomic = self.published_readiness.clone();
        let failure_flag = self.unavailable.clone();

        if self.is_unavailable() {
            return Err(FeederError::Unavailable);
        }

        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(
                capacity = self.info.max_pending_jobs,
                "request capacity exhausted; rejecting with 503 busy"
            );
            FeederError::from(ServiceError::Busy)
        })?;
        let job_guard = self.lifecycle.track_job();
        let lifecycle = self.lifecycle.clone();
        let graph = self.graph.clone();
        let unavailable = self.unavailable.clone();

        tokio::task::spawn_blocking(move || {
            let _job_guard = job_guard;
            let _permit = permit;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: unavailable.clone(),
                success: false,
            };
            lifecycle.check_worker_hook();
            let mut state = graph.lock();
            if state.failed || unavailable.load(Ordering::Acquire) {
                panic_guard.unavailable.store(true, Ordering::Release);
                return Err(FeederError::Unavailable);
            }
            if mutate {
                state.failed = true;
            }
            let result = op(&mut state, mode, &instance_id);
            let word = state.current_readiness_word();
            published_atomic.store(word, Ordering::Release);
            if mutate {
                state.failed = false;
            }
            drop(state);
            lifecycle.check_worker_post_lock_hook();
            panic_guard.success = true;
            result
        })
        .await
        .map_err(|error| {
            tracing::error!(
                panicked = error.is_panic(),
                "{worker_name} worker thread failed"
            );
            failure_flag.store(true, Ordering::Release);
            FeederError::Unavailable
        })?
    }

    /// Applies an ordered stream of feeder events under the graph mutex.
    pub(crate) async fn apply_events(
        &self,
        req: FeederEventsRequest,
    ) -> Result<FeederEventsResult, FeederError> {
        self.execute_feeder_worker("events", true, move |state, mode, inst| {
            state.apply_events(req, mode, inst)
        })
        .await
    }

    /// Acknowledges an unrecoverable source gap, moving server to Desynchronized under mutex.
    pub(crate) async fn declare_gap(
        &self,
        req: GapRequest,
    ) -> Result<(GapResponse, ResponseMetadata), FeederError> {
        self.execute_feeder_worker("gap", true, move |state, mode, inst| {
            state.declare_gap(req, mode, inst)
        })
        .await
    }

    /// Verifies a synchronization barrier under the graph mutex.
    pub(crate) async fn declare_barrier(
        &self,
        req: BarrierRequest,
    ) -> Result<(BarrierResponse, ResponseMetadata), FeederError> {
        self.execute_feeder_worker("barrier", false, move |state, mode, inst| {
            state.declare_barrier(req, mode, inst)
        })
        .await
    }

    /// Transitions sync_status from Ready to Stale if and only if the exact observed
    /// publication has not been superseded by a newer publication.
    pub(crate) fn try_mark_stale_from_expired_lease(&self, observed_word: u64) -> bool {
        let observed = PublishedReadiness::unpack(observed_word);
        if observed.status != SyncStatus::Ready {
            return false;
        }
        let stale_word =
            PublishedReadiness::pack(SyncStatus::Stale, observed.seq, observed.deadline_millis);
        self.published_readiness
            .compare_exchange(
                observed_word,
                stale_word,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    // One admission/cancellation/poisoning policy for reads and writes. The
    // owned permit stays with the blocking closure even if its caller is gone.
    async fn run<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut GraphState) -> R + Send + 'static,
    ) -> Result<R, ServiceError> {
        if self.is_unavailable() {
            return Err(ServiceError::Unavailable);
        }

        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(
                capacity = self.info.max_pending_jobs,
                "request capacity exhausted; rejecting with 503 busy"
            );
            ServiceError::Busy
        })?;
        let job_guard = self.lifecycle.track_job();
        let lifecycle = self.lifecycle.clone();
        let graph = self.graph.clone();
        let unavailable = self.unavailable.clone();
        let failure_flag = self.unavailable.clone();
        tokio::task::spawn_blocking(move || {
            let _job_guard = job_guard;
            let _permit = permit;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: unavailable.clone(),
                success: false,
            };
            lifecycle.check_worker_hook();
            let mut graph = graph.lock();
            if graph.failed || unavailable.load(Ordering::Acquire) {
                tracing::warn!("graph worker state marked as failed; rejecting work");
                panic_guard.unavailable.store(true, Ordering::Release);
                return Err(ServiceError::Unavailable);
            }
            graph.failed = true;
            let result = work(&mut graph);
            graph.failed = false;
            drop(graph);
            lifecycle.check_worker_post_lock_hook();
            panic_guard.success = true;
            Ok(result)
        })
        .await
        .map_err(|error| {
            // Never log the error itself: a panic message may embed request data.
            tracing::error!(panicked = error.is_panic(), "graph worker thread failed");
            failure_flag.store(true, Ordering::Release);
            ServiceError::Unavailable
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::WireNode;
    use std::time::Duration;

    fn add(node: u64) -> Operation {
        Operation::AddNode {
            node: WireNode(node),
        }
    }

    fn link(source: u64, target: u64) -> Operation {
        Operation::Link {
            source: WireNode(source),
            target: WireNode(target),
        }
    }

    #[tokio::test]
    async fn saturated_service_rejects_without_mutating() {
        let service = Service::new(ConnectivityGraph::default(), 1, Lifecycle::new());
        let permit = service.permits.clone().acquire_owned().await.unwrap();
        assert!(matches!(
            service.execute(vec![add(9)]).await,
            Err(ServiceError::Busy)
        ));
        drop(permit);
        assert!(matches!(
            service.execute(vec![add(9)]).await.unwrap()[0],
            Outcome::Changed { changed: true }
        ));
    }

    #[tokio::test]
    async fn dropped_caller_does_not_release_a_running_workers_permit() {
        let service = Service::new(ConnectivityGraph::default(), 1, Lifecycle::new());
        let graph = service.graph.clone();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        // Hold the graph from a dedicated thread so the worker stays pending
        // without blocking the async test executor or depending on a large graph.
        let holder = std::thread::spawn(move || {
            let _guard = graph.lock();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        locked_rx.await.unwrap();
        let caller_service = service.clone();
        let caller = tokio::spawn(async move { caller_service.execute(vec![add(7)]).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        let _ = caller.await;
        assert_eq!(service.permits.available_permits(), 0);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(service.graph.lock().graph.connected(7, 7), Ok(true));
    }

    #[test]
    fn decimal_version_carries_without_wrapping_or_losing_precision() {
        for (before, after) in [
            ("9", "10"),
            ("99", "100"),
            ("9007199254740992", "9007199254740993"),
            ("18446744073709551615", "18446744073709551616"),
            (
                "9999999999999999999999999999999999999999",
                "10000000000000000000000000000000000000000",
            ),
        ] {
            let mut state = GraphState::new(
                ConnectivityGraph::default(),
                crate::ServerConfig::DEFAULT_DEDUP_WINDOW_SIZE,
            );
            state.version = before.bytes().rev().map(|b| b - b'0').collect();
            state.apply(add(1));
            assert_eq!(
                state.snapshot(ServerMode::Standalone, "").state_version,
                after
            );
            state.apply(add(1));
            assert_eq!(
                state.snapshot(ServerMode::Standalone, "").state_version,
                after
            );
        }
    }

    #[tokio::test]
    async fn stats_admission_does_not_block_info_and_worker_failure_fails_closed() {
        use axum::{
            Router,
            body::{Body, to_bytes},
            http::{Request, StatusCode},
            routing::get,
        };
        use tower::ServiceExt;
        let service = Service::new(ConnectivityGraph::default(), 1, Lifecycle::new());
        let app = Router::new()
            .route("/v1/stats", get(crate::api::stats))
            .route("/v1/info", get(crate::api::info))
            .with_state(service.clone());
        let permit = service.permits.clone().acquire_owned().await.unwrap();
        let response = app
            .clone()
            .oneshot(Request::get("/v1/stats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "busy");
        let info = app
            .clone()
            .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(info.status(), StatusCode::OK);
        drop(permit);
        assert_eq!(service.stats().await.unwrap().state_version, "0");
        assert!(
            service
                .run(|_| {
                    panic!("test worker panic");
                })
                .await
                .is_err()
        );
        let response = app
            .clone()
            .oneshot(Request::get("/v1/stats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "unavailable");
        let response = app
            .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(service.permits.available_permits(), 1);
        assert_eq!(service.info().engine, "compact-bfs");
    }

    #[tokio::test]
    async fn panicked_writer_rejects_queued_and_future_work() {
        let service = Service::new(ConnectivityGraph::default(), 2, Lifecycle::new());
        let writer = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            writer
                .run(move |state| {
                    state.apply(add(1));
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("panic after partial mutation");
                })
                .await
        });
        started_rx.await.unwrap();
        let reader = service.clone();
        let queued = tokio::spawn(async move { reader.stats().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Dropping the requester must not disable the worker's failure tracking.
        job.abort();
        let _ = job.await;
        release_tx.send(()).unwrap();
        assert!(matches!(
            queued.await.unwrap(),
            Err(ServiceError::Unavailable)
        ));
        assert!(matches!(
            service.execute(vec![add(2)]).await,
            Err(ServiceError::Unavailable)
        ));
        assert!(matches!(
            service.stats().await,
            Err(ServiceError::Unavailable)
        ));
        assert!(service.is_unavailable());
        assert_eq!(
            crate::lifecycle::readiness(&crate::Lifecycle::ready(), &service)
                .await
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(service.graph.lock().graph.node_count(), 1);
        assert_eq!(service.info().engine, "compact-bfs");
    }

    #[tokio::test]
    async fn worker_panic_after_caller_cancellation_marks_ready_unavailable() {
        let lifecycle = Lifecycle::ready();
        let service = Service::new(ConnectivityGraph::default(), 1, lifecycle.clone());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = service.clone();
        let job = tokio::spawn(async move {
            writer
                .run(move |state| {
                    state.apply(add(1));
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("test worker panic after caller cancel");
                })
                .await
        });
        started_rx.await.unwrap();
        // Cancel the awaiting caller before the panic occurs.
        job.abort();
        let _ = job.await;
        // Release worker so it panics on the blocking thread.
        release_tx.send(()).unwrap();
        // Wait until worker thread completes unwind and releases job guard.
        tokio::time::timeout(Duration::from_secs(3), async {
            while lifecycle.outstanding_jobs() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Without submitting any further graph jobs, /ready must become 503 unavailable!
        assert!(service.is_unavailable());
        let response = crate::lifecycle::readiness(&lifecycle, &service).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn post_lock_hook_panic_after_caller_cancellation_marks_service_unavailable() {
        let lifecycle = Lifecycle::ready();
        let service = Service::new(ConnectivityGraph::default(), 1, lifecycle.clone());

        // Install post-lock pause hook on the lifecycle
        let mut pause_handle = lifecycle.pause_next_worker_after_lock();

        let writer = service.clone();
        let job = tokio::spawn(async move { writer.execute(vec![add(1)]).await });

        // Wait until worker finishes mutation, drops graph lock, and pauses at post-lock hook
        pause_handle.wait_started().await;

        // Cancel the awaiting caller before the panic occurs in the post-lock hook
        job.abort();
        let _ = job.await;

        // Instruct the post-lock hook to panic on the blocking thread
        pause_handle.panic_on_release();

        // Wait until worker thread completes unwind and releases job guard
        tokio::time::timeout(Duration::from_secs(3), async {
            while lifecycle.outstanding_jobs() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        // Service must be permanently marked unavailable even though caller was cancelled
        assert!(service.is_unavailable());
        let response = crate::lifecycle::readiness(&lifecycle, &service).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );

        // Future requests must immediately fail closed with Unavailable
        assert!(matches!(
            service.execute(vec![add(2)]).await,
            Err(ServiceError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn admitted_stats_wait_for_the_entire_locked_job_and_retain_permit_on_cancel() {
        let service = Service::new(ConnectivityGraph::default(), 2, Lifecycle::new());
        let writer = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            writer
                .run(move |state| {
                    state.apply(add(1));
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    state.apply(add(2));
                })
                .await
        });
        started_rx.await.unwrap();
        let reader = service.clone();
        let read = tokio::spawn(async move { reader.stats().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!read.is_finished()); // The snapshot cannot observe the first change alone.
        read.abort();
        let _ = read.await;
        assert_eq!(service.permits.available_permits(), 0);
        release_tx.send(()).unwrap();
        job.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = service.stats().await.unwrap();
        assert_eq!(snapshot.node_count, 2);
        assert_eq!(snapshot.state_version, "2");
    }

    #[tokio::test]
    async fn queued_job_behind_blocked_worker_preserves_accounting_on_caller_cancellation() {
        let lifecycle = Lifecycle::ready();
        let service = Service::new(ConnectivityGraph::default(), 2, lifecycle.clone());

        let (w1_started_tx, w1_started_rx) = tokio::sync::oneshot::channel();
        let (w1_release_tx, w1_release_rx) = std::sync::mpsc::channel();
        let (w2_done_tx, w2_done_rx) = std::sync::mpsc::channel();

        let s1 = service.clone();
        let job1 = tokio::spawn(async move {
            s1.run(move |state| {
                state.apply(add(1));
                w1_started_tx.send(()).unwrap();
                w1_release_rx.recv().unwrap();
            })
            .await
        });
        w1_started_rx.await.unwrap();
        assert_eq!(lifecycle.outstanding_jobs(), 1);

        let s2 = service.clone();
        let job2 = tokio::spawn(async move {
            s2.run(move |state| {
                state.apply(add(2));
                w2_done_tx.send(()).unwrap();
            })
            .await
        });

        // Wait until job2 is admitted and running in spawn_blocking (accounting = 2).
        tokio::time::timeout(Duration::from_secs(3), async {
            while lifecycle.outstanding_jobs() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        // Cancel job2's awaiting async caller.
        job2.abort();
        let _ = job2.await;

        // Accounting must remain 2 while job2 is queued/blocked behind job1.
        assert_eq!(lifecycle.outstanding_jobs(), 2);

        // Release job1 so job2 can acquire the lock and finish.
        w1_release_tx.send(()).unwrap();
        job1.await.unwrap().unwrap();
        w2_done_rx.recv().unwrap();

        // Once job2 finishes, accounting must drop to 0.
        tokio::time::timeout(Duration::from_secs(3), async {
            while lifecycle.outstanding_jobs() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(lifecycle.outstanding_jobs(), 0);
    }

    #[tokio::test]
    async fn early_rejection_due_to_failed_graph_decrements_job_accounting_to_zero() {
        let lifecycle = Lifecycle::ready();
        let service = Service::new(ConnectivityGraph::default(), 1, lifecycle.clone());

        // Mark graph as failed directly.
        service.graph.lock().failed = true;

        // Submitting an operation must be rejected with Unavailable.
        let result = service.execute(vec![add(1)]).await;
        assert!(matches!(result, Err(ServiceError::Unavailable)));

        // Job guard must have been dropped, returning outstanding jobs to 0.
        assert_eq!(lifecycle.outstanding_jobs(), 0);
    }

    #[test]
    fn percent_encoding_and_decoding_round_trip() {
        let test_positions = [
            "pos-10",
            "0/16B3740",
            "epoch=3,pos=9912",
            "complex+cursor;part=1&part=2",
            "with spaces and \"quotes\"",
            "newline\r\nand\ttabs",
            "dots.and.more.dots",
            "special_chars~-_",
            "utf8-§-äöü-🚀",
        ];
        for original in test_positions {
            let encoded = percent_encode_position(original);
            // Must not contain unencoded dots, quotes, or whitespace
            assert!(!encoded.contains('.'));
            assert!(!encoded.contains('"'));
            assert!(!encoded.contains(' '));
            assert!(!encoded.contains('\r'));
            assert!(!encoded.contains('\n'));

            let decoded =
                percent_decode_position(&encoded).expect("failed to decode valid encoded position");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn response_metadata_etag_formatting() {
        let meta_standalone = ResponseMetadata {
            instance_id: "inst-1".into(),
            graph_id: "default",
            generation: 0,
            state_version: "42".into(),
            source_position: None,
            sync_status: None,
        };
        assert_eq!(meta_standalone.etag(), "\"0.42.\"");

        let meta_source_managed = ResponseMetadata {
            instance_id: "inst-1".into(),
            graph_id: "default",
            generation: 3,
            state_version: "0".into(),
            source_position: Some("0/16B3740".into()),
            sync_status: Some(SyncStatus::CatchingUp),
        };
        assert_eq!(meta_source_managed.etag(), "\"3.0.0%2F16B3740\"");
    }

    #[test]
    fn generation_exhaustion_arithmetic_check() {
        let max_gen = u64::MAX;
        assert_eq!(max_gen.checked_add(1), None);
    }

    #[tokio::test]
    async fn service_level_actual_generation_exhaustion_on_import_publication_path() {
        use crate::ServerConfig;
        use axum::body::Bytes;
        use knotrel_core::EngineConfig;

        let lifecycle = Lifecycle::ready();
        let config =
            ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
                .unwrap()
                .with_mode(ServerMode::SourceManaged);
        let graph = ConnectivityGraph::default();
        let service = Service::with_config(graph, 32, lifecycle, &config);

        // Seed live state: generation = u64::MAX, valid session & lease, baseline edge (1, 2)
        let session_id = "sess-exhaustion".to_string();
        let source_id = "stream-alpha".to_string();
        let source_position = "pos-initial".to_string();
        {
            let mut state = service.graph.lock();
            state.graph.link(1, 2).unwrap();
            state.generation = u64::MAX;
            state.version = "42".bytes().rev().map(|b| b - b'0').collect();
            state.source_id = Some(source_id.clone());
            state.source_epoch = Some("ep-7".to_string());
            state.source_position = Some(source_position.clone());
            state.feeder_session_id = Some(session_id.clone());
            state.feeder_epoch = 1;
            state.lease_deadline = Some(monotonic_now() + Duration::from_secs(60));
        }
        service.set_sync_status_for_test(SyncStatus::CatchingUp);

        let matching_etag = format!(
            "\"{}.42.{}\"",
            u64::MAX,
            percent_encode_position(&source_position)
        );

        let import_payload = serde_json::json!({
            "mode": "replace",
            "instance_id": service.instance_id(),
            "graph_id": "default",
            "source_id": "stream-alpha",
            "source_epoch": "ep-7",
            "source_position": "pos-new",
            "feeder_session_id": session_id,
            "feeder_epoch": 1,
            "nodes": ["10", "20"],
            "edges": [{"source": "10", "target": "20"}]
        });
        let raw_bytes = Bytes::from(serde_json::to_vec(&import_payload).unwrap());

        let permit = service
            .acquire_import_permit()
            .expect("acquire import permit");
        let result = service.import(permit, raw_bytes, Some(matching_etag)).await;

        // Must reject with GenerationExhausted
        assert!(
            matches!(result, Err(FeederError::GenerationExhausted)),
            "actual result: {:?}",
            result
        );

        // Topology preserved: 1 and 2 connected, 10 and 20 NOT published
        let state = service.graph.lock();
        assert_eq!(state.graph.connected(1, 2), Ok(true));
        assert_eq!(state.graph.node_count(), 2);
        assert_eq!(state.graph.edge_count(), 1);
        assert_eq!(state.generation, u64::MAX);
        let current_version: String = state
            .version
            .iter()
            .rev()
            .map(|d| char::from(b'0' + d))
            .collect();
        assert_eq!(current_version, "42");
        assert_eq!(state.source_id.as_deref(), Some("stream-alpha"));
        assert_eq!(state.source_epoch.as_deref(), Some("ep-7"));
        assert_eq!(state.source_position.as_deref(), Some("pos-initial"));
        assert_eq!(service.sync_status(), SyncStatus::CatchingUp);
    }

    #[tokio::test]
    async fn arbitrary_precision_revision_beyond_u64_max_with_multi_digit_carry() {
        use crate::ServerConfig;
        use knotrel_core::EngineConfig;

        let lifecycle = Lifecycle::ready();
        let config =
            ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
                .unwrap()
                .with_mode(ServerMode::Standalone);
        let graph = ConnectivityGraph::default();
        let service = Service::with_config(graph, 32, lifecycle, &config);

        // Seed revision to a 22-digit number beyond u64::MAX ending in 599
        let seed_version = "1844674407370955161599";
        {
            let mut state = service.graph.lock();
            state.version = seed_version.bytes().rev().map(|b| b - b'0').collect();
        }

        // 1. Effective mutation triggers multi-digit carry (599 -> 600)
        let res = service
            .execute(vec![link(1, 2)])
            .await
            .expect("execute link");
        assert_eq!(res.outcomes.len(), 1);
        assert!(matches!(
            res.outcomes[0],
            Outcome::Changed { changed: true }
        ));
        assert_eq!(res.metadata.state_version, "1844674407370955161600");
        assert_eq!(res.metadata.etag(), "\"0.1844674407370955161600.\"");

        // 2. Duplicate link is a no-op; revision must remain unchanged
        let res_noop = service
            .execute(vec![link(1, 2)])
            .await
            .expect("execute duplicate link");
        assert_eq!(res_noop.outcomes.len(), 1);
        assert!(matches!(
            res_noop.outcomes[0],
            Outcome::Changed { changed: false }
        ));
        assert_eq!(res_noop.metadata.state_version, "1844674407370955161600");
        assert_eq!(res_noop.metadata.etag(), "\"0.1844674407370955161600.\"");

        // 3. Rejected mutation (self-loop); error outcome; revision must remain unchanged
        let res_rejected = service
            .execute(vec![link(1, 1)])
            .await
            .expect("execute self-loop link");
        assert_eq!(res_rejected.outcomes.len(), 1);
        assert!(matches!(
            res_rejected.outcomes[0],
            Outcome::Error {
                code: "self_loop",
                ..
            }
        ));
        assert_eq!(
            res_rejected.metadata.state_version,
            "1844674407370955161600"
        );
        assert_eq!(
            res_rejected.metadata.etag(),
            "\"0.1844674407370955161600.\""
        );

        // 4. Verify snapshot reflects the exact arbitrary-precision version
        let stats = service.stats().await.expect("stats");
        assert_eq!(stats.snapshot.state_version, "1844674407370955161600");
        assert_eq!(stats.metadata.state_version, "1844674407370955161600");
        assert_eq!(stats.metadata.etag(), "\"0.1844674407370955161600.\"");
    }

    #[tokio::test]
    async fn feeder_events_sequence_exhaustion_and_arbitrary_precision_carry() {
        let config = crate::ServerConfig::default().with_mode(ServerMode::SourceManaged);
        let service =
            Service::with_config(ConnectivityGraph::default(), 2, Lifecycle::new(), &config);
        let instance_id = service.instance_id().to_string();

        let session_res = service
            .register_session(crate::api::SessionRequest {
                instance_id: instance_id.clone(),
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                lease_duration_ms: 10000,
            })
            .await
            .expect("session");
        assert_eq!(session_res.result, "session_acquired");

        let import_bytes = format!(
            r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-0","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
        );
        let permit = service.acquire_import_permit().expect("import permit");
        let _ = service
            .import(permit, axum::body::Bytes::from(import_bytes), None)
            .await
            .expect("import");

        service.seed_version_for_test("18446744073709551615");

        let ev1 = crate::api::FeederEventsRequest {
            instance_id: instance_id.clone(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            source_id: "pg-1".into(),
            source_epoch: "ep-1".into(),
            events: vec![crate::api::FeederEvent {
                event_id: "e-1".into(),
                delivery_sequence: 1,
                prev_delivery_sequence: 0,
                source_position: "pos-1".into(),
                prev_source_position: "pos-0".into(),
                op: "link".into(),
                source: Some(crate::api::WireNode(1)),
                target: Some(crate::api::WireNode(2)),
                node: None,
            }],
        };
        let res = service.apply_events(ev1).await.expect("apply events");
        match res {
            FeederEventsResult::Applied { response, metadata } => {
                assert_eq!(response.state_version, "18446744073709551616");
                assert_eq!(metadata.state_version, "18446744073709551616");
                assert_eq!(metadata.etag(), "\"1.18446744073709551616.pos-1\"");
            }
            _ => panic!("expected applied"),
        }

        // Test checked arithmetic on sequence counter:
        // Set applied_delivery_sequence to u64::MAX - 1, apply event at u64::MAX -> succeeds
        service.seed_sequence_for_test(u64::MAX - 1);
        let ev_max = crate::api::FeederEventsRequest {
            instance_id: instance_id.clone(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            source_id: "pg-1".into(),
            source_epoch: "ep-1".into(),
            events: vec![crate::api::FeederEvent {
                event_id: "e-max".into(),
                delivery_sequence: u64::MAX,
                prev_delivery_sequence: u64::MAX - 1,
                source_position: "pos-max".into(),
                prev_source_position: "pos-1".into(),
                op: "link".into(),
                source: Some(crate::api::WireNode(1)),
                target: Some(crate::api::WireNode(3)),
                node: None,
            }],
        };
        let res_max = service.apply_events(ev_max).await.expect("apply u64::MAX");
        match res_max {
            FeederEventsResult::Applied { response, .. } => {
                assert_eq!(response.applied_delivery_sequence, u64::MAX);
            }
            _ => panic!("expected applied"),
        }

        // Retained duplicate retry at u64::MAX within the deduplication window returns duplicate
        let ev_max_retry = crate::api::FeederEventsRequest {
            instance_id: instance_id.clone(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            source_id: "pg-1".into(),
            source_epoch: "ep-1".into(),
            events: vec![crate::api::FeederEvent {
                event_id: "e-max".into(),
                delivery_sequence: u64::MAX,
                prev_delivery_sequence: u64::MAX - 1,
                source_position: "pos-max".into(),
                prev_source_position: "pos-1".into(),
                op: "link".into(),
                source: Some(crate::api::WireNode(1)),
                target: Some(crate::api::WireNode(3)),
                node: None,
            }],
        };
        let res_max_retry = service
            .apply_events(ev_max_retry)
            .await
            .expect("retry u64::MAX");
        match res_max_retry {
            FeederEventsResult::Duplicate { response, .. } => {
                assert_eq!(response.current_applied_delivery_sequence, u64::MAX);
                assert_eq!(response.delivery_sequence, u64::MAX);
                assert_eq!(response.result, "duplicate");
                assert_eq!(response.status, "already_applied");
            }
            _ => panic!("expected duplicate for retained retry at u64::MAX"),
        }

        // Sequence exhaustion semantics and wire protocol boundary:
        // In the u64 wire protocol, delivery_sequence is bounded to [0, u64::MAX].
        // There is no 65-bit integer representable on the wire, so no valid wire payload
        // can supply a sequence strictly greater than u64::MAX (e.g. u64::MAX + 1).
        // Consequently, FeederError::SequenceExhausted is an internal defensive error variant
        // protecting checked arithmetic; it cannot be triggered through the u64 wire interface.
        //
        // When applied_delivery_sequence is u64::MAX, any incoming wire sequence satisfies
        // seq <= applied_delivery_sequence (since every u64 <= u64::MAX).
        // Therefore, incoming events at this boundary follow deduplication semantics:
        // - In-window matching retry (u64::MAX): succeeds as duplicate without mutation (tested above).
        // - Wrapped sequence zero (or any evicted sequence): falls outside the retained window
        //   and is rejected with UnverifiableDuplicate without mutating state.
        let ev_overflow = crate::api::FeederEventsRequest {
            instance_id: instance_id.clone(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            source_id: "pg-1".into(),
            source_epoch: "ep-1".into(),
            events: vec![crate::api::FeederEvent {
                event_id: "e-overflow".into(),
                delivery_sequence: 0,
                prev_delivery_sequence: u64::MAX,
                source_position: "pos-overflow".into(),
                prev_source_position: "pos-max".into(),
                op: "add_node".into(),
                source: None,
                target: None,
                node: Some(crate::api::WireNode(5)),
            }],
        };
        let res_overflow = service.apply_events(ev_overflow).await;
        assert!(matches!(
            res_overflow,
            Err(FeederError::UnverifiableDuplicate(_))
        ));

        // State remains strictly preserved at u64::MAX without mutating topology or sequence:
        let stats = service.stats().await.expect("stats").snapshot;
        assert_eq!(service.applied_delivery_sequence(), u64::MAX);
        assert_eq!(stats.source_position.as_deref(), Some("pos-max"));
        assert_eq!(stats.node_count, 3);
        assert_eq!(stats.sync_status, Some(SyncStatus::CatchingUp));
    }

    #[tokio::test]
    async fn feeder_events_rejected_after_desynchronization() {
        let config = crate::ServerConfig::default().with_mode(ServerMode::SourceManaged);
        let service =
            Service::with_config(ConnectivityGraph::default(), 2, Lifecycle::new(), &config);
        let instance_id = service.instance_id().to_string();

        let session_res = service
            .register_session(crate::api::SessionRequest {
                instance_id: instance_id.clone(),
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                lease_duration_ms: 10000,
            })
            .await
            .expect("session");
        assert_eq!(session_res.result, "session_acquired");

        let import_bytes = format!(
            r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-0","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
        );
        let permit = service.acquire_import_permit().expect("import permit");
        let _ = service
            .import(permit, axum::body::Bytes::from(import_bytes), None)
            .await
            .expect("import");

        // Apply event 1
        service
            .apply_events(crate::api::FeederEventsRequest {
                instance_id: instance_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                source_id: "pg-1".into(),
                source_epoch: "ep-1".into(),
                events: vec![crate::api::FeederEvent {
                    event_id: "e-1".into(),
                    delivery_sequence: 1,
                    prev_delivery_sequence: 0,
                    source_position: "pos-1".into(),
                    prev_source_position: "pos-0".into(),
                    op: "add_node".into(),
                    source: None,
                    target: None,
                    node: Some(crate::api::WireNode(3)),
                }],
            })
            .await
            .expect("event 1");

        // Transition to Desynchronized via declared gap
        service
            .declare_gap(crate::api::GapRequest {
                instance_id: instance_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                failed_delivery_sequence: 5,
                reason: Some("source gap".into()),
            })
            .await
            .expect("gap");

        assert_eq!(service.sync_status(), SyncStatus::Desynchronized);

        // Attempt next valid mutation event at D=2 -> rejected with GraphDesynchronized
        let res_mutation = service
            .apply_events(crate::api::FeederEventsRequest {
                instance_id: instance_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                source_id: "pg-1".into(),
                source_epoch: "ep-1".into(),
                events: vec![crate::api::FeederEvent {
                    event_id: "e-2".into(),
                    delivery_sequence: 2,
                    prev_delivery_sequence: 1,
                    source_position: "pos-2".into(),
                    prev_source_position: "pos-1".into(),
                    op: "add_node".into(),
                    source: None,
                    target: None,
                    node: Some(crate::api::WireNode(4)),
                }],
            })
            .await;
        assert_eq!(res_mutation.unwrap_err(), FeederError::GraphDesynchronized);

        // Attempt next valid progress event at D=2 -> rejected with GraphDesynchronized
        let res_progress = service
            .apply_events(crate::api::FeederEventsRequest {
                instance_id: instance_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "f-1".into(),
                feeder_epoch: 1,
                source_id: "pg-1".into(),
                source_epoch: "ep-1".into(),
                events: vec![crate::api::FeederEvent {
                    event_id: "e-prog".into(),
                    delivery_sequence: 2,
                    prev_delivery_sequence: 1,
                    source_position: "pos-2".into(),
                    prev_source_position: "pos-1".into(),
                    op: "progress".into(),
                    source: None,
                    target: None,
                    node: None,
                }],
            })
            .await;
        assert_eq!(res_progress.unwrap_err(), FeederError::GraphDesynchronized);

        // Verify entire state preserved:
        let stats = service.stats().await.expect("stats").snapshot;
        assert_eq!(stats.node_count, 3);
        assert_eq!(service.applied_delivery_sequence(), 1);
        assert_eq!(stats.source_position.as_deref(), Some("pos-1"));
        assert_eq!(stats.state_version, "1");
        assert_eq!(stats.sync_status, Some(SyncStatus::Desynchronized));
    }

    #[test]
    fn test_barrier_decision_table_b1_through_b9() {
        let limits = knotrel_core::GraphLimits {
            max_nodes: Some(100),
            max_edges: Some(100),
        };
        let mut state = GraphState::new(
            ConnectivityGraph::with_limits(EngineConfig::CompactBfs, limits),
            64,
        );
        let instance_id = "srv-inst-1";
        let session_id = "feed-1";

        // Setup session
        state
            .register_session(
                SessionRequest {
                    instance_id: instance_id.into(),
                    feeder_session_id: session_id.into(),
                    feeder_epoch: 1,
                    lease_duration_ms: 60_000,
                },
                instance_id.into(),
            )
            .unwrap();

        let make_req = |generation: u64, seq: u64, pos: Option<&str>| BarrierRequest {
            instance_id: instance_id.into(),
            graph_id: "default".into(),
            generation,
            feeder_session_id: session_id.into(),
            feeder_epoch: 1,
            barrier_delivery_sequence: seq,
            barrier_source_position: pos.map(Into::into),
        };

        // Case B3: Uninitialized (g = 0)
        let b3_res =
            state.declare_barrier(make_req(0, 0, None), ServerMode::SourceManaged, instance_id);
        assert!(matches!(b3_res, Err(FeederError::GraphUninitialized)));

        // Case B4: Loading (g = 0, status Loading)
        state.sync_status = SyncStatus::Loading;
        let b4_res =
            state.declare_barrier(make_req(0, 0, None), ServerMode::SourceManaged, instance_id);
        assert!(matches!(b4_res, Err(FeederError::GraphUninitialized)));

        // Publish generation 1
        state.generation = 1;
        state.sync_status = SyncStatus::CatchingUp;
        state.applied_delivery_sequence = 5;
        state.source_position = Some("pos-5".into());

        // Case B8: Premature sequence (D_B = 6 > 5)
        let b8_res = state.declare_barrier(
            make_req(1, 6, Some("pos-5")),
            ServerMode::SourceManaged,
            instance_id,
        );
        assert_eq!(
            b8_res.unwrap_err(),
            FeederError::BarrierNotReached {
                expected: 5,
                received: 6
            }
        );
        assert_eq!(state.sync_status, SyncStatus::CatchingUp);

        // Case B9: Older sequence (D_B = 4 < 5)
        let b9_res = state.declare_barrier(
            make_req(1, 4, Some("pos-5")),
            ServerMode::SourceManaged,
            instance_id,
        );
        assert_eq!(
            b9_res.unwrap_err(),
            FeederError::SupersededBarrier {
                expected: 5,
                received: 4
            }
        );
        assert_eq!(state.sync_status, SyncStatus::CatchingUp);

        // Case B7: Equal sequence with position mismatch (D_B = 5, P_B = "pos-wrong")
        let b7_res = state.declare_barrier(
            make_req(1, 5, Some("pos-wrong")),
            ServerMode::SourceManaged,
            instance_id,
        );
        assert_eq!(
            b7_res.unwrap_err(),
            FeederError::BarrierPositionMismatch {
                expected: "pos-5".into(),
                received: "pos-wrong".into()
            }
        );
        assert_eq!(state.sync_status, SyncStatus::CatchingUp);

        // Case B1: Exact match from CatchingUp (D_B = 5, P_B = "pos-5")
        let (b1_res, _) = state
            .declare_barrier(
                make_req(1, 5, Some("pos-5")),
                ServerMode::SourceManaged,
                instance_id,
            )
            .unwrap();
        assert_eq!(b1_res.result, "synchronized");
        assert_eq!(b1_res.status, "ready");
        assert_eq!(state.sync_status, SyncStatus::Ready);

        // Case B2: Exact match from Ready (idempotent)
        let (b2_res, _) = state
            .declare_barrier(
                make_req(1, 5, Some("pos-5")),
                ServerMode::SourceManaged,
                instance_id,
            )
            .unwrap();
        assert_eq!(b2_res.result, "already_synchronized");
        assert_eq!(b2_res.status, "ready");
        assert_eq!(state.sync_status, SyncStatus::Ready);

        // Case B5: Desynchronized
        state.sync_status = SyncStatus::Desynchronized;
        let b5_res = state.declare_barrier(
            make_req(1, 5, Some("pos-5")),
            ServerMode::SourceManaged,
            instance_id,
        );
        assert_eq!(b5_res.unwrap_err(), FeederError::GraphDesynchronized);
        assert_eq!(state.sync_status, SyncStatus::Desynchronized);

        // Case B6: Failed
        state.failed = true;
        let b6_res = state.declare_barrier(
            make_req(1, 5, Some("pos-5")),
            ServerMode::SourceManaged,
            instance_id,
        );
        assert_eq!(b6_res.unwrap_err(), FeederError::Unavailable);
    }

    #[tokio::test]
    async fn test_clock_advance_and_lease_expiry_staleness() {
        let _clock_guard = lock_clock_for_test().await;
        reset_clock();
        let limits = knotrel_core::GraphLimits {
            max_nodes: Some(100),
            max_edges: Some(100),
        };
        let lifecycle = Lifecycle::ready();
        let config = crate::ServerConfig::default();
        let service = Service::with_config(
            ConnectivityGraph::with_limits(EngineConfig::CompactBfs, limits),
            32,
            lifecycle.clone(),
            &config,
        );
        // Force source-managed mode for this test service
        let service = Service {
            info: crate::ServerInfo {
                engine: "compact-bfs",
                experimental: false,
                max_pending_jobs: 32,
                max_body_bytes: crate::MAX_BODY_BYTES,
                max_batch_operations: crate::MAX_BATCH_OPERATIONS,
                max_nodes: Some(100),
                max_edges: Some(100),
                mode: Some("source_managed"),
                instance_id: Some(service.instance_id.clone()),
            },
            mode: ServerMode::SourceManaged,
            ..service
        };

        let inst_id = service.instance_id().to_string();
        service
            .register_session(SessionRequest {
                instance_id: inst_id.clone(),
                feeder_session_id: "sess-1".into(),
                feeder_epoch: 1,
                lease_duration_ms: 10_000,
            })
            .await
            .unwrap();

        // Seed generation 1 in Ready
        {
            let mut state = service.graph.lock();
            state.generation = 1;
            state.sync_status = SyncStatus::Ready;
            state.applied_delivery_sequence = 10;
            state.source_position = Some("pos-10".into());
        }
        service.set_sync_status_for_test(SyncStatus::Ready);

        assert!(!service.is_lease_expired());
        assert_eq!(service.sync_status(), SyncStatus::Ready);

        // Advance mock monotonic clock past the 10s lease
        advance_clock(Duration::from_millis(10_001));

        assert!(service.is_lease_expired());
        // Lock-free mark stale using observed publication
        let word = service.published_readiness_word();
        assert!(service.try_mark_stale_from_expired_lease(word));
        assert_eq!(service.sync_status(), SyncStatus::Stale);

        // Heartbeat renewal: extends deadline but returns and preserves Stale
        let hb_res = service
            .heartbeat(HeartbeatRequest {
                instance_id: inst_id.clone(),
                feeder_session_id: "sess-1".into(),
                feeder_epoch: 1,
            })
            .await
            .unwrap();
        assert_eq!(hb_res.status, SyncStatus::Stale);
        assert_eq!(service.sync_status(), SyncStatus::Stale);
        assert!(!service.is_lease_expired());

        // Barrier with expired epoch 1 is fenced
        let barrier_err = service
            .declare_barrier(BarrierRequest {
                instance_id: inst_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "sess-1".into(),
                feeder_epoch: 1,
                barrier_delivery_sequence: 10,
                barrier_source_position: Some("pos-10".into()),
            })
            .await
            .unwrap_err();
        assert_eq!(barrier_err, FeederError::FencedSession);

        // Feeder registers new epoch 2 to recover synchronization
        let sess2 = service
            .register_session(SessionRequest {
                instance_id: inst_id.clone(),
                feeder_session_id: "sess-1".into(),
                feeder_epoch: 2,
                lease_duration_ms: 10_000,
            })
            .await
            .unwrap();
        assert_eq!(sess2.feeder_epoch, 2);

        // Exact barrier under newly registered epoch 2 restores Ready
        let (barrier_res, _) = service
            .declare_barrier(BarrierRequest {
                instance_id: inst_id.clone(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "sess-1".into(),
                feeder_epoch: 2,
                barrier_delivery_sequence: 10,
                barrier_source_position: Some("pos-10".into()),
            })
            .await
            .unwrap();
        assert_eq!(barrier_res.result, "synchronized");
        assert_eq!(barrier_res.status, "ready");
        assert_eq!(service.sync_status(), SyncStatus::Ready);

        reset_clock();
    }

    #[test]
    fn test_published_readiness_packing_roundtrip() {
        for status in [
            SyncStatus::Uninitialized,
            SyncStatus::Loading,
            SyncStatus::CatchingUp,
            SyncStatus::Ready,
            SyncStatus::Stale,
            SyncStatus::Desynchronized,
        ] {
            for seq in [0u16, 1, 42, 65535] {
                for deadline_ms in [0u64, 1, 1000, 10_000_000, 0x0FFF_FFFF_FFFF] {
                    let word = PublishedReadiness::pack(status, seq, deadline_ms);
                    let unpacked = PublishedReadiness::unpack(word);
                    assert_eq!(unpacked.status, status);
                    assert_eq!(unpacked.seq, seq);
                    assert_eq!(unpacked.deadline_millis, deadline_ms);
                }
            }
        }
    }

    #[test]
    fn test_feeder_domain_op_and_dedup_window_edge_cases() {
        use crate::api::{FeederEvent, WireNode};

        // 1. CanonicalOp::from_feeder_event error branches
        let base_event = FeederEvent {
            event_id: "evt-1".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "link".into(),
            node: None,
            source: None,
            target: None,
        };

        // Link missing source/target
        assert!(CanonicalOp::from_feeder_event(&base_event).is_err());
        // Link with node
        let mut evt = base_event.clone();
        evt.source = Some(WireNode(1));
        evt.target = Some(WireNode(2));
        evt.node = Some(WireNode(3));
        assert!(CanonicalOp::from_feeder_event(&evt).is_err());

        // Cut missing source/target
        let mut cut_evt = base_event.clone();
        cut_evt.op = "cut".into();
        assert!(CanonicalOp::from_feeder_event(&cut_evt).is_err());
        // Cut with node
        cut_evt.source = Some(WireNode(1));
        cut_evt.target = Some(WireNode(2));
        cut_evt.node = Some(WireNode(3));
        assert!(CanonicalOp::from_feeder_event(&cut_evt).is_err());

        // AddNode missing node
        let mut node_evt = base_event.clone();
        node_evt.op = "add_node".into();
        assert!(CanonicalOp::from_feeder_event(&node_evt).is_err());
        // AddNode with source
        node_evt.node = Some(WireNode(1));
        node_evt.source = Some(WireNode(1));
        assert!(CanonicalOp::from_feeder_event(&node_evt).is_err());

        // Progress with node or source
        let mut prog_evt = base_event.clone();
        prog_evt.op = "progress".into();
        assert!(CanonicalOp::from_feeder_event(&prog_evt).is_ok());
        prog_evt.node = Some(WireNode(1));
        assert!(CanonicalOp::from_feeder_event(&prog_evt).is_err());

        // Unsupported op
        let mut unk_evt = base_event.clone();
        unk_evt.op = "unsupported".into();
        assert!(CanonicalOp::from_feeder_event(&unk_evt).is_err());

        // 2. DedupWindow check_sequence and ring replacement
        let mut window = DedupWindow::new(4);
        assert!(matches!(window.check_sequence(1, 0), DedupLookup::Future));
        assert!(matches!(window.check_sequence(5, 2), DedupLookup::Future));

        // Record entries and test eviction lookup
        window.record(DedupEntry {
            event_id: "e1".into(),
            delivery_sequence: 1,
            canonical_op: CanonicalOp::AddNode { node: 1 },
            source_position: "1".into(),
            changed: true,
        });
        assert!(window.contains_event_id("e1"));
        assert!(matches!(
            window.check_sequence(1, 1),
            DedupLookup::InWindow(_)
        ));
        assert!(matches!(window.check_sequence(5, 10), DedupLookup::Evicted));

        // Overwrite slot in ring
        window.record(DedupEntry {
            event_id: "e5".into(),
            delivery_sequence: 5,
            canonical_op: CanonicalOp::AddNode { node: 5 },
            source_position: "5".into(),
            changed: true,
        });
        assert!(!window.contains_event_id("e1"));
        assert!(window.contains_event_id("e5"));

        // 3. percent_decode_position error branches
        assert_eq!(percent_decode_position("%"), None);
        assert_eq!(percent_decode_position("%A"), None);
        assert_eq!(percent_decode_position("%ZZ"), None);

        // 4. FeederError from ServiceError
        assert_eq!(
            FeederError::from(ServiceError::Busy),
            FeederError::ImportInProgress
        );
        assert_eq!(
            FeederError::from(ServiceError::Unavailable),
            FeederError::Unavailable
        );
    }

    #[tokio::test]
    async fn test_service_feeder_validation_error_paths() {
        use crate::ServerConfig;
        use crate::api::{FeederEvent, WireNode};

        let lifecycle = Lifecycle::ready();
        let config_sm = ServerConfig::default().with_mode(ServerMode::SourceManaged);
        let service_sm = Service::with_config(
            ConnectivityGraph::default(),
            2,
            lifecycle.clone(),
            &config_sm,
        );

        let config_standalone = ServerConfig::default().with_mode(ServerMode::Standalone);
        let service_standalone = Service::with_config(
            ConnectivityGraph::default(),
            2,
            lifecycle.clone(),
            &config_standalone,
        );

        // ModeMismatch on standalone
        let sess_req = SessionRequest {
            instance_id: service_standalone.instance_id().to_string(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            lease_duration_ms: 5000,
        };
        assert!(matches!(
            service_standalone.register_session(sess_req.clone()).await,
            Err(FeederError::ModeMismatch)
        ));

        let hb_req = HeartbeatRequest {
            instance_id: service_standalone.instance_id().to_string(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
        };
        assert!(matches!(
            service_standalone.heartbeat(hb_req.clone()).await,
            Err(FeederError::ModeMismatch)
        ));

        // Stale instance_id
        let mut stale_sess = sess_req.clone();
        stale_sess.instance_id = "wrong-inst".into();
        assert!(matches!(
            service_sm.register_session(stale_sess).await,
            Err(FeederError::StaleInstanceId(_))
        ));

        let mut stale_hb = hb_req.clone();
        stale_hb.instance_id = "wrong-inst".into();
        assert!(matches!(
            service_sm.heartbeat(stale_hb).await,
            Err(FeederError::StaleInstanceId(_))
        ));

        // Invalid lease duration
        let mut bad_lease = sess_req.clone();
        bad_lease.instance_id = service_sm.instance_id().to_string();
        bad_lease.lease_duration_ms = 500; // < 1000
        assert!(matches!(
            service_sm.register_session(bad_lease.clone()).await,
            Err(FeederError::InvalidLeaseDuration)
        ));
        bad_lease.lease_duration_ms = 70_000; // > 60000
        assert!(matches!(
            service_sm.register_session(bad_lease).await,
            Err(FeederError::InvalidLeaseDuration)
        ));

        // Register valid session on source_managed
        let mut valid_sess = sess_req;
        valid_sess.instance_id = service_sm.instance_id().to_string();
        assert!(service_sm.register_session(valid_sess).await.is_ok());

        // declare_gap on uninitialized graph (generation == 0)
        let gap_req = GapRequest {
            instance_id: service_sm.instance_id().to_string(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            failed_delivery_sequence: 1,
            reason: Some("gap".into()),
        };
        assert!(matches!(
            service_sm.declare_gap(gap_req.clone()).await,
            Err(FeederError::GraphUninitialized)
        ));

        // apply_events on uninitialized graph
        let make_events_req = |events: Vec<FeederEvent>| FeederEventsRequest {
            instance_id: service_sm.instance_id().to_string(),
            graph_id: "default".into(),
            generation: 1,
            source_id: "src1".into(),
            source_epoch: "ep1".into(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            events,
        };

        let evt = FeederEvent {
            event_id: "e1".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "add_node".into(),
            node: Some(WireNode(1)),
            source: None,
            target: None,
        };

        assert!(matches!(
            service_sm
                .apply_events(make_events_req(vec![evt.clone()]))
                .await,
            Err(FeederError::GraphUninitialized)
        ));

        // Seed generation = 1 so apply_events passes the generation check
        service_sm.seed_generation_for_test(1);

        // Events limits validation: oversized
        assert!(matches!(
            service_sm
                .apply_events(make_events_req(vec![
                    evt.clone();
                    crate::MAX_BATCH_OPERATIONS + 1
                ]))
                .await,
            Err(FeederError::InvalidRequest(_))
        ));

        // Long fields in event
        let mut long_pos_evt = evt.clone();
        long_pos_evt.source_position = "x".repeat(257);
        assert!(matches!(
            service_sm
                .apply_events(make_events_req(vec![long_pos_evt]))
                .await,
            Err(FeederError::InvalidRequest(_))
        ));

        let mut long_prev_evt = evt.clone();
        long_prev_evt.prev_source_position = "x".repeat(257);
        assert!(matches!(
            service_sm
                .apply_events(make_events_req(vec![long_prev_evt]))
                .await,
            Err(FeederError::InvalidRequest(_))
        ));

        let mut long_id_evt = evt.clone();
        long_id_evt.event_id = "x".repeat(65);
        assert!(matches!(
            service_sm
                .apply_events(make_events_req(vec![long_id_evt]))
                .await,
            Err(FeederError::InvalidRequest(_))
        ));

        // Unavailable service check
        service_sm.mark_unavailable_for_test();
        assert!(matches!(
            service_sm.declare_gap(gap_req).await,
            Err(FeederError::Unavailable)
        ));
        assert!(matches!(
            service_sm.apply_events(make_events_req(vec![evt])).await,
            Err(FeederError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn test_service_import_validation_and_candidate_limits() {
        use crate::ServerConfig;
        use axum::body::Bytes;
        use knotrel_core::GraphLimits;

        let lifecycle = Lifecycle::ready();
        let limits = GraphLimits {
            max_nodes: Some(5),
            max_edges: Some(1),
        };
        let config = ServerConfig::default()
            .with_mode(ServerMode::SourceManaged)
            .with_graph_limits(limits);
        let service = Service::with_config(
            ConnectivityGraph::with_limits(knotrel_core::EngineConfig::default(), limits),
            2,
            lifecycle.clone(),
            &config,
        );

        // Register session so session check passes
        let inst = service.instance_id().to_string();
        let sess_req = SessionRequest {
            instance_id: inst.clone(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            lease_duration_ms: 10000,
        };
        service.register_session(sess_req).await.unwrap();

        let base_import = |inst_id: &str, graph: &str, nodes: &str, edges: &str| {
            format!(
                r#"{{"instance_id":"{inst_id}","graph_id":"{graph}","source_id":"s1","source_epoch":"1","source_position":"0","feeder_session_id":"s1","feeder_epoch":1,"mode":"create_only","nodes":{nodes},"edges":{edges}}}"#
            )
        };

        // 1. Instance ID mismatch in import
        let permit = service.acquire_import_permit().unwrap();
        let body_mismatch = base_import("wrong", "default", "[]", "[]");
        let err = service
            .import(permit, Bytes::from(body_mismatch), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::StaleInstanceId(_)));

        // 2. Graph ID != "default"
        let permit = service.acquire_import_permit().unwrap();
        let body_graph = base_import(&inst, "other", "[]", "[]");
        let err = service
            .import(permit, Bytes::from(body_graph), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::InvalidGraphId(_)));

        // 3. Node limit exceeded in candidate
        let permit = service.acquire_import_permit().unwrap();
        let body_nodes = base_import(&inst, "default", r#"["1","2","3","4","5","6"]"#, "[]");
        let err = service
            .import(permit, Bytes::from(body_nodes), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::Domain("node_limit_exceeded", _)));

        // 4. Edge self loop in candidate
        let permit = service.acquire_import_permit().unwrap();
        let body_self_loop = base_import(
            &inst,
            "default",
            r#"["1"]"#,
            r#"[{"source":"1","target":"1"}]"#,
        );
        let err = service
            .import(permit, Bytes::from(body_self_loop), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::Domain("self_loop", _)));

        // 5. Edge limit exceeded in candidate
        let permit = service.acquire_import_permit().unwrap();
        let body_edges = base_import(
            &inst,
            "default",
            r#"["1","2","3"]"#,
            r#"[{"source":"1","target":"2"},{"source":"2","target":"3"}]"#,
        );
        let err = service
            .import(permit, Bytes::from(body_edges), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::Domain("edge_limit_exceeded", _)));
    }

    #[tokio::test]
    async fn test_service_uncovered_branches_and_edge_cases() {
        use crate::api::FeederEvent;
        use knotrel_core::ConnectivityGraph;
        let lifecycle = Lifecycle::ready();
        let config = crate::ServerConfig::default().with_mode(ServerMode::SourceManaged);
        let service =
            Service::with_config(ConnectivityGraph::default(), 2, lifecycle.clone(), &config);

        // 1. DedupWindow ring wrap slot eviction and Evicted on empty/mismatch slot
        let mut dedup = DedupWindow::new(2);
        dedup.record(DedupEntry {
            delivery_sequence: 2,
            event_id: "e2".into(),
            source_position: "2".into(),
            canonical_op: CanonicalOp::AddNode { node: 1 },
            changed: true,
        });
        assert!(matches!(dedup.check_sequence(1, 2), DedupLookup::Evicted));

        dedup.record(DedupEntry {
            delivery_sequence: 4,
            event_id: "e4".into(),
            source_position: "4".into(),
            canonical_op: CanonicalOp::AddNode { node: 2 },
            changed: true,
        });
        assert!(!dedup.contains_event_id("e2"));
        assert!(dedup.contains_event_id("e4"));

        // 2. register_session branch tests:
        let inst = service.instance_id().to_string();
        let sess_req = SessionRequest {
            instance_id: inst.clone(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 2,
            lease_duration_ms: 10000,
        };
        assert!(service.register_session(sess_req).await.is_ok());

        let fenced_req = SessionRequest {
            instance_id: inst.clone(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            lease_duration_ms: 10000,
        };
        assert!(matches!(
            service.register_session(fenced_req).await,
            Err(FeederError::FencedSession)
        ));

        let diff_fenced_req = SessionRequest {
            instance_id: inst.clone(),
            feeder_session_id: "s2".into(),
            feeder_epoch: 2,
            lease_duration_ms: 10000,
        };
        assert!(matches!(
            service.register_session(diff_fenced_req).await,
            Err(FeederError::FencedSession)
        ));

        service.set_published_readiness_for_test(
            SyncStatus::Ready,
            crate::service::now_to_millis() + 60_000,
        );
        let higher_epoch_req = SessionRequest {
            instance_id: inst.clone(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 3,
            lease_duration_ms: 10000,
        };
        assert!(service.register_session(higher_epoch_req).await.is_ok());
        assert_eq!(service.sync_status(), SyncStatus::Stale);

        // 3. Heartbeat with no session
        let fresh_service =
            Service::with_config(ConnectivityGraph::default(), 2, lifecycle.clone(), &config);
        let hb_req = HeartbeatRequest {
            instance_id: fresh_service.instance_id().to_string(),
            feeder_session_id: "none".into(),
            feeder_epoch: 1,
        };
        assert!(matches!(
            fresh_service.heartbeat(hb_req).await,
            Err(FeederError::SessionRequired)
        ));

        // 4. apply_events: generation mismatch
        service.seed_generation_for_test(1);
        let dummy_evt = FeederEvent {
            event_id: "e1".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "add_node".into(),
            node: Some(crate::api::WireNode(1)),
            source: None,
            target: None,
        };
        let make_events_req =
            |req_gen: u64, src: &str, events: Vec<FeederEvent>| FeederEventsRequest {
                instance_id: inst.clone(),
                graph_id: "default".into(),
                generation: req_gen,
                source_id: src.into(),
                source_epoch: "1".into(),
                feeder_session_id: "s1".into(),
                feeder_epoch: 3,
                events,
            };
        assert!(matches!(
            service
                .apply_events(make_events_req(2, "s1", vec![dummy_evt.clone()]))
                .await,
            Err(FeederError::StaleGeneration(_))
        ));

        // 5. execute() in SourceManaged mode under Loading and Desynchronized
        service.set_sync_status_for_test(SyncStatus::Loading);
        let exec_res = service
            .execute(vec![Operation::Connected {
                source: WireNode(1),
                target: WireNode(2),
            }])
            .await
            .unwrap();
        assert!(matches!(
            exec_res.outcomes[0],
            Outcome::Error {
                code: "loading",
                ..
            }
        ));

        service.set_sync_status_for_test(SyncStatus::Desynchronized);
        let exec_res = service
            .execute(vec![Operation::Connected {
                source: WireNode(1),
                target: WireNode(2),
            }])
            .await
            .unwrap();
        assert!(matches!(
            exec_res.outcomes[0],
            Outcome::Error {
                code: "desynchronized",
                ..
            }
        ));

        // 6. stats() while Loading
        service.seed_generation_for_test(0);
        service.set_sync_status_for_test(SyncStatus::Loading);
        let stats_res = service.stats().await.unwrap();
        assert_eq!(stats_res.snapshot.sync_status, Some(SyncStatus::Loading));

        // 7. import create_only when generation != 0 -> GraphAlreadyInitialized
        service.seed_generation_for_test(1);
        let permit = service.acquire_import_permit().unwrap();
        let body = format!(
            r#"{{"instance_id":"{inst}","graph_id":"default","source_id":"s1","source_epoch":"1","source_position":"0","feeder_session_id":"s1","feeder_epoch":3,"mode":"create_only","nodes":[],"edges":[]}}"#
        );
        let err = service
            .import(permit, axum::body::Bytes::from(body), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::GraphAlreadyInitialized));

        // 8. import replace when generation == 0 -> PreconditionFailed
        service.seed_generation_for_test(0);
        let permit = service.acquire_import_permit().unwrap();
        let body_replace = format!(
            r#"{{"instance_id":"{inst}","graph_id":"default","source_id":"s1","source_epoch":"1","source_position":"0","feeder_session_id":"s1","feeder_epoch":3,"mode":"replace","nodes":[],"edges":[]}}"#
        );
        let err = service
            .import(
                permit,
                axum::body::Bytes::from(body_replace.clone()),
                Some("\"gen:1\"".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::PreconditionFailed(_)));

        // 9. import replace when if_match is None -> PreconditionFailed
        service.seed_generation_for_test(1);
        let permit = service.acquire_import_permit().unwrap();
        let err = service
            .import(permit, axum::body::Bytes::from(body_replace), None)
            .await
            .unwrap_err();
        assert!(matches!(err, FeederError::PreconditionFailed(_)));

        // 10. Cut self-loop in apply_events
        let cut_self_loop_evt = FeederEvent {
            event_id: "e_cut_loop".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "cut".into(),
            node: None,
            source: Some(WireNode(1)),
            target: Some(WireNode(1)),
        };
        // Single event returns Err(FeederError::Domain)
        let single_err = service
            .apply_events(make_events_req(1, "s1", vec![cut_self_loop_evt.clone()]))
            .await;
        assert!(matches!(
            single_err,
            Err(FeederError::Domain("self_loop", _))
        ));

        // Batch returns PartialFailure
        let valid_evt = FeederEvent {
            event_id: "e_valid".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "add_node".into(),
            node: Some(WireNode(1)),
            source: None,
            target: None,
        };
        let mut second_evt = cut_self_loop_evt;
        second_evt.delivery_sequence = 2;
        second_evt.prev_delivery_sequence = 1;
        second_evt.prev_source_position = "1".into();
        second_evt.source_position = "2".into();
        let batch_res = service
            .apply_events(make_events_req(1, "s1", vec![valid_evt, second_evt]))
            .await
            .unwrap();
        match batch_res {
            crate::service::FeederEventsResult::PartialFailure { failure, .. } => {
                assert_eq!(failure.error.code, "self_loop");
            }
            _ => panic!("expected partial failure for self-loop cut in batch"),
        }
    }
}
