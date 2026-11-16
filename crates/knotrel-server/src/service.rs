//! Bounded CPU work and exclusive graph ownership for each request.

use crate::graph::ServiceGraph;
use crate::{
    Lifecycle, ServerMode,
    api::{
        BatchItemError, DEFAULT_GRAPH_ID, DuplicateStatus, ErrorCode, FeederDuplicateResponse,
        FeederEventsRequest, FeederEventsResponse, FeederOperation, FeederResult, GapRequest,
        GapResponse, HeartbeatRequest, HeartbeatResponse, HistoricalOutcome, ImportMode,
        ImportRequest, ImportResponse, MAX_EVENT_ID_BYTES, MAX_LEASE_DURATION_MS,
        MAX_SOURCE_POSITION_BYTES, MIN_LEASE_DURATION_MS, Operation, Outcome, PartialBatchFailure,
        RequiredAction, SessionRequest, SessionResponse, WireOp,
    },
    metrics::{
        CatchupInterval, CatchupKind, CatchupRecorder, CompletedCatchup, Desynchronization,
        FeederEventResult, FeederRejection, GraphOp, ImportObservation, JobClock, JobKind,
        PendingOutcome, Rejection, ServiceTelemetry, Uncertainty, lock_timed,
    },
};
#[cfg(test)]
use knotrel_core::ConnectivityGraph;
use knotrel_core::{EngineConfig, GraphError};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

const DEFAULT_FEEDER_LEASE_DURATION: Duration = Duration::from_secs(10);
const IMPORT_PERMIT_CAPACITY: usize = 1;

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

/// Maps a label produced by [`EngineConfig::as_str`] back to its engine.
/// An unknown label is a programming error and fails loudly instead of
/// silently substituting an engine.
fn engine_from_label(label: &str) -> EngineConfig {
    [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ]
    .into_iter()
    .find(|engine| engine.as_str() == label)
    .unwrap_or_else(|| panic!("engine label {label:?} does not come from EngineConfig::as_str"))
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
#[repr(u8)]
pub(crate) enum SyncStatus {
    Uninitialized = 0,
    Loading = 1,
    CatchingUp = 2,
    Ready = 3,
    Stale = 4,
    Desynchronized = 5,
}

impl SyncStatus {
    pub(crate) const fn as_str(self) -> &'static str {
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
        self as u8
    }

    pub(crate) const fn from_code(code: u8) -> Self {
        match code {
            code if code == Self::Loading as u8 => Self::Loading,
            code if code == Self::CatchingUp as u8 => Self::CatchingUp,
            code if code == Self::Ready as u8 => Self::Ready,
            code if code == Self::Stale as u8 => Self::Stale,
            code if code == Self::Desynchronized as u8 => Self::Desynchronized,
            _ => Self::Uninitialized,
        }
    }
}

const STATUS_BITS: u32 = 4;
const SEQUENCE_BITS: u32 = u16::BITS;
const DEADLINE_BITS: u32 = u64::BITS - STATUS_BITS - SEQUENCE_BITS;
const SEQUENCE_SHIFT: u32 = STATUS_BITS;
const DEADLINE_SHIFT: u32 = STATUS_BITS + SEQUENCE_BITS;
const STATUS_MASK: u64 = (1_u64 << STATUS_BITS) - 1;
const SEQUENCE_MASK: u64 = (1_u64 << SEQUENCE_BITS) - 1;
const DEADLINE_MASK: u64 = (1_u64 << DEADLINE_BITS) - 1;

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
        let status_bits = (status.to_code() as u64) & STATUS_MASK;
        let seq_bits = ((seq as u64) & SEQUENCE_MASK) << SEQUENCE_SHIFT;
        let deadline_bits = (deadline_millis & DEADLINE_MASK) << DEADLINE_SHIFT;
        status_bits | seq_bits | deadline_bits
    }

    pub(crate) const fn unpack(word: u64) -> Self {
        let status_code = (word & STATUS_MASK) as u8;
        let seq = ((word >> SEQUENCE_SHIFT) & SEQUENCE_MASK) as u16;
        let deadline_millis = (word >> DEADLINE_SHIFT) & DEADLINE_MASK;
        Self {
            status: SyncStatus::from_code(status_code),
            seq,
            deadline_millis,
        }
    }
}

/// Canonical operation descriptor normalized for undirected graph edges.
///
/// It is the duplicate-detection fingerprint of an event together with its
/// source position: the operation type, normalized endpoints and any
/// lifecycle flag all participate, so a retry that changes any of them is a
/// conflicting duplicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalOp {
    AddNode {
        node: u64,
    },
    Link {
        source: u64,
        target: u64,
    },
    Cut {
        source: u64,
        target: u64,
    },
    Progress,
    SetNodeActive {
        node: u64,
        active: bool,
    },
    SetEdgeEnabled {
        source: u64,
        target: u64,
        enabled: bool,
    },
    RemoveNode {
        node: u64,
    },
}

impl CanonicalOp {
    /// Validates one event's shape and resolves its canonical operation.
    ///
    /// Lifecycle operations without lifecycle support return
    /// [`FeederError::TopologyLifecycleDisabled`] before their fields are
    /// checked. `active` and `enabled` on any other operation are
    /// `invalid_request`, whatever the setting.
    pub(crate) fn from_feeder_event(
        event: &crate::api::FeederEvent,
        lifecycle: bool,
    ) -> Result<Self, FeederError> {
        let operation = match &event.op {
            WireOp::Known(operation) => *operation,
            WireOp::Unknown(name) => {
                return Err(FeederError::InvalidRequest(if lifecycle {
                    format!(
                        "unsupported operation '{name}'; must be link, cut, add_node, progress, \
                         set_node_active, set_edge_enabled, or remove_node"
                    )
                } else {
                    format!(
                        "unsupported operation '{name}'; must be link, cut, add_node, or progress"
                    )
                }));
            }
        };
        let is_lifecycle = matches!(
            operation,
            FeederOperation::SetNodeActive
                | FeederOperation::SetEdgeEnabled
                | FeederOperation::RemoveNode
        );
        if is_lifecycle && !lifecycle {
            return Err(FeederError::TopologyLifecycleDisabled);
        }
        let flags = (event.active, event.enabled);
        let endpoints = |name: &str| {
            let (Some(source), Some(target), None) = (event.source, event.target, event.node)
            else {
                return Err(FeederError::InvalidRequest(format!(
                    "{name} operation requires source and target, and must not have node"
                )));
            };
            Ok((source.0.min(target.0), source.0.max(target.0)))
        };
        let single = |name: &str| {
            let (Some(node), None, None) = (event.node, event.source, event.target) else {
                return Err(FeederError::InvalidRequest(format!(
                    "{name} operation requires node, and must not have source or target"
                )));
            };
            Ok(node.0)
        };
        let no_flags = |name: &str| {
            if flags == (None, None) {
                Ok(())
            } else {
                Err(FeederError::InvalidRequest(format!(
                    "{name} operation must not have active or enabled"
                )))
            }
        };
        match operation {
            FeederOperation::Link => {
                let (source, target) = endpoints("link")?;
                no_flags("link")?;
                Ok(Self::Link { source, target })
            }
            FeederOperation::Cut => {
                let (source, target) = endpoints("cut")?;
                no_flags("cut")?;
                Ok(Self::Cut { source, target })
            }
            FeederOperation::AddNode => {
                let node = single("add_node")?;
                no_flags("add_node")?;
                Ok(Self::AddNode { node })
            }
            FeederOperation::Progress => {
                if event.node.is_some() || event.source.is_some() || event.target.is_some() {
                    return Err(FeederError::InvalidRequest(
                        "progress operation must not have node, source, or target".into(),
                    ));
                }
                no_flags("progress")?;
                Ok(Self::Progress)
            }
            FeederOperation::SetNodeActive => {
                let node = single("set_node_active")?;
                let (Some(active), None) = flags else {
                    return Err(FeederError::InvalidRequest(
                        "set_node_active operation requires active, and must not have enabled"
                            .into(),
                    ));
                };
                Ok(Self::SetNodeActive { node, active })
            }
            FeederOperation::SetEdgeEnabled => {
                let (source, target) = endpoints("set_edge_enabled")?;
                let (None, Some(enabled)) = flags else {
                    return Err(FeederError::InvalidRequest(
                        "set_edge_enabled operation requires enabled, and must not have active"
                            .into(),
                    ));
                };
                Ok(Self::SetEdgeEnabled {
                    source,
                    target,
                    enabled,
                })
            }
            FeederOperation::RemoveNode => {
                let node = single("remove_node")?;
                no_flags("remove_node")?;
                Ok(Self::RemoveNode { node })
            }
        }
    }
}

/// Builds an import candidate off-lock from the request's nodes and edges.
///
/// Without lifecycle support, any flagged entry rejects the import with
/// `topology_lifecycle_disabled` before anything is built; otherwise the
/// legacy ordered loop runs unchanged. With lifecycle support, the complete
/// registered topology, including inactive nodes and disabled edges, is
/// built by [`LifecycleGraph::try_from_topology`](knotrel_core::LifecycleGraph::try_from_topology).
fn build_import_candidate(
    nodes: Vec<crate::api::ImportNode>,
    edges: Vec<crate::api::WireEdge>,
    engine: EngineConfig,
    limits: knotrel_core::GraphLimits,
    topology_lifecycle: bool,
) -> Result<ServiceGraph, FeederError> {
    let self_loop = || {
        FeederError::Domain(
            ErrorCode::SelfLoop,
            "self-loops are forbidden in simple undirected graphs".into(),
        )
    };
    let limit_error = |error: GraphError, fallback: ErrorCode| match error {
        GraphError::NodeLimitExceeded { limit } => FeederError::Domain(
            ErrorCode::NodeLimitExceeded,
            format!("node limit {limit} exceeded"),
        ),
        GraphError::EdgeLimitExceeded { limit } => FeederError::Domain(
            ErrorCode::EdgeLimitExceeded,
            format!("edge limit {limit} exceeded"),
        ),
        GraphError::SelfLoop { .. } => self_loop(),
        _ => FeederError::Domain(fallback, error.to_string()),
    };
    if !topology_lifecycle {
        if nodes.iter().any(|node| node.active.is_some())
            || edges.iter().any(|edge| edge.enabled.is_some())
        {
            return Err(FeederError::TopologyLifecycleDisabled);
        }
        let mut candidate = knotrel_core::ConnectivityGraph::with_limits(engine, limits);
        for node in nodes {
            candidate
                .add_node(node.node.0)
                .map_err(|err| limit_error(err, ErrorCode::InvalidNode))?;
        }
        for edge in edges {
            if edge.source.0 == edge.target.0 {
                return Err(self_loop());
            }
            candidate
                .link(edge.source.0, edge.target.0)
                .map_err(|err| limit_error(err, ErrorCode::InvalidEdge))?;
        }
        return Ok(ServiceGraph::Plain(candidate));
    }
    let node_specs: Vec<knotrel_core::NodeSpec> = nodes
        .into_iter()
        .map(|node| knotrel_core::NodeSpec {
            node: node.node.0,
            active: node.active.unwrap_or(true),
        })
        .collect();
    let edge_specs: Vec<knotrel_core::EdgeSpec> = edges
        .into_iter()
        .map(|edge| knotrel_core::EdgeSpec {
            source: edge.source.0,
            target: edge.target.0,
            enabled: edge.enabled.unwrap_or(true),
        })
        .collect();
    knotrel_core::LifecycleGraph::try_from_topology(engine, limits, &node_specs, &edge_specs)
        .map(ServiceGraph::Lifecycle)
        .map_err(|error| match error {
            knotrel_core::LifecycleError::Graph(error) => {
                limit_error(error, ErrorCode::InvalidEdge)
            }
            other => {
                let (code, message) = crate::api::lifecycle_error_code(other);
                FeederError::Domain(code, message)
            }
        })
}

/// Maps a lifecycle event's result to the event loop's domain error pair.
///
/// Prevalidation rejects lifecycle events for a plain graph, so the
/// unsupported branch is defensive and still reports an error.
fn lifecycle_event(
    result: Result<Result<bool, knotrel_core::LifecycleError>, crate::graph::LifecycleUnsupported>,
) -> Result<bool, (ErrorCode, String)> {
    match result {
        Ok(Ok(changed)) => Ok(changed),
        Ok(Err(error)) => Err(crate::api::lifecycle_error_code(error)),
        Err(_) => Err((
            ErrorCode::TopologyLifecycleDisabled,
            crate::api::LIFECYCLE_DISABLED_MESSAGE.into(),
        )),
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
    OutOfOrderSequence {
        expected: u64,
        received: u64,
    },
    SourcePredecessorMismatch(String),
    ConflictingDuplicate(String),
    UnverifiableDuplicate(String),
    SequenceExhausted,
    PreconditionFailed(String),
    InvalidLeaseDuration,
    MalformedJson,
    UnprocessableJson(String),
    PayloadTooLarge,
    Domain(ErrorCode, String),
    InvalidRequest(String),
    BarrierPositionMismatch {
        expected: String,
        received: String,
    },
    BarrierNotReached {
        expected: u64,
        received: u64,
    },
    SupersededBarrier {
        expected: u64,
        received: u64,
    },
    /// A lifecycle event or import entry without lifecycle support.
    TopologyLifecycleDisabled,
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
    pub(crate) result: FeederResult,
    pub(crate) instance_id: String,
    pub(crate) graph_id: &'static str,
    pub(crate) generation: u64,
    pub(crate) state_version: String,
    pub(crate) applied_source_position: String,
    pub(crate) status: SyncStatus,
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
///
/// Axum clones the router state for every request, so the handle is a single
/// reference-counted pointer: one atomic increment per clone instead of one
/// per shared field plus copies of the instance ID and server info strings.
#[derive(Clone)]
pub(crate) struct Service(Arc<ServiceInner>);

impl std::ops::Deref for Service {
    type Target = ServiceInner;

    fn deref(&self) -> &ServiceInner {
        &self.0
    }
}

/// State shared by every clone of a [`Service`].
pub(crate) struct ServiceInner {
    graph: Arc<Mutex<GraphState>>,
    permits: Arc<Semaphore>,
    import_permits: Arc<Semaphore>,
    // Lock-free mirror of GraphState::failed so readiness never waits for graph work.
    unavailable: Arc<AtomicBool>,
    published_readiness: Arc<AtomicU64>,
    lifecycle: Lifecycle,
    info: crate::ServerInfo,
    mode: ServerMode,
    // Shared with every worker closure without copying the string.
    instance_id: Arc<str>,
    max_import_body_bytes: usize,
    // Present only when metrics are enabled; see `metrics::ServiceTelemetry`.
    telemetry: Option<Arc<ServiceTelemetry>>,
}

/// Graph and revision share one mutex: snapshots cannot split a batch or observe
/// a mutation without its revision increment. Version starts at zero even when
/// embedding a pre-populated graph; it counts this service instance's changes.
struct GraphState {
    graph: ServiceGraph,
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
    // The open catch-up interval of the current generation: set by an import
    // publication, taken by the first `synchronized` barrier. Replacing the
    // generation drops an unfinished interval unobserved. `None` without
    // catch-up metrics, so disabled telemetry reads no clock.
    catchup: Option<CatchupInterval>,
    // Handed from `declare_barrier` to the worker, which observes it after
    // releasing the lock.
    completed_catchup: Option<CompletedCatchup>,
}

impl GraphState {
    fn new(graph: ServiceGraph, dedup_capacity: usize) -> Self {
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
            lease_duration: DEFAULT_FEEDER_LEASE_DURATION,
            lease_deadline: None,
            sync_status: SyncStatus::Uninitialized,
            publication_seq: 0,
            last_expired_epoch: None,
            dedup_window: DedupWindow::new(dedup_capacity),
            catchup: None,
            completed_catchup: None,
        }
    }

    /// Synchronization coordinates for transition logging (M6.3).
    fn sync_point(&self) -> crate::logging::SyncPoint {
        crate::logging::SyncPoint {
            generation: self.generation,
            status: self.sync_status,
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
            lifecycle: self.graph.lifecycle_counts(),
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
            graph_id: DEFAULT_GRAPH_ID,
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
            result: FeederResult::SessionAcquired,
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
            result: FeederResult::HeartbeatAcknowledged,
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
        telemetry: Option<&ServiceTelemetry>,
        is_batch: bool,
        mode: ServerMode,
        instance_id: &str,
        req_instance_id: &str,
        idx: usize,
        seq: u64,
        event_id: &str,
        code: ErrorCode,
        reason: &str,
        err_code: ErrorCode,
        err_msg: String,
        status: axum::http::StatusCode,
        single_err: FeederError,
    ) -> Result<FeederEventsResult, FeederError> {
        count_feeder_event(telemetry, FeederEventResult::Rejected);
        if is_batch {
            // The non-batch `Err` is counted where the worker's job returns.
            count_feeder_rejection(telemetry, &single_err);
            Ok(FeederEventsResult::PartialFailure {
                failure: PartialBatchFailure {
                    result: FeederResult::PartialFailure,
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
            SyncStatus::Ready => FeederResult::AlreadySynchronized,
            SyncStatus::CatchingUp | SyncStatus::Stale => {
                self.sync_status = SyncStatus::Ready;
                // The first synchronized barrier of a generation completes
                // its catch-up interval; a later recovery from stale finds
                // none. Rejected and `already_synchronized` barriers never
                // reach this arm.
                if let Some(interval) = self.catchup.take() {
                    self.completed_catchup = Some(interval.complete());
                }
                FeederResult::Synchronized
            }
            _ => unreachable!(),
        };
        self.last_expired_epoch = None;

        let metadata = self.response_metadata(mode, instance_id);
        let res = BarrierResponse {
            result: result_str,
            instance_id: req.instance_id,
            graph_id: DEFAULT_GRAPH_ID,
            generation: self.generation,
            state_version: self.version_string(),
            applied_source_position: self.source_position.clone().unwrap_or_default(),
            status: SyncStatus::Ready,
        };
        Ok((res, metadata))
    }

    /// Evaluates events in order. Each event the loop reaches is counted
    /// once in `telemetry` when its evaluation finishes; whole-request checks
    /// before the loop and events after the first failure count nothing.
    fn apply_events(
        &mut self,
        req: FeederEventsRequest,
        mode: ServerMode,
        instance_id: &str,
        telemetry: Option<&ServiceTelemetry>,
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
            desynchronize(
                &mut self.sync_status,
                Desynchronization::SourceEpochMismatch,
                telemetry,
            );
            return Err(FeederError::SourceEpochMismatch(format!(
                "request source_epoch '{}' does not match server source_epoch '{}'",
                req.source_epoch, live_source_epoch
            )));
        }

        let lifecycle = self.graph.is_lifecycle();
        for event in &req.events {
            if event.event_id.len() > MAX_EVENT_ID_BYTES {
                return Err(FeederError::InvalidRequest(format!(
                    "event_id must not exceed {MAX_EVENT_ID_BYTES} bytes"
                )));
            }
            if event.source_position.len() > MAX_SOURCE_POSITION_BYTES {
                return Err(FeederError::InvalidRequest(format!(
                    "source_position must not exceed {MAX_SOURCE_POSITION_BYTES} bytes"
                )));
            }
            if event.prev_source_position.len() > MAX_SOURCE_POSITION_BYTES {
                return Err(FeederError::InvalidRequest(format!(
                    "prev_source_position must not exceed {MAX_SOURCE_POSITION_BYTES} bytes"
                )));
            }
            let _ = CanonicalOp::from_feeder_event(event, lifecycle)?;
        }

        let total_events = req.events.len();
        let mut newly_applied_count = 0usize;
        // Stamps the last-applied instant once, on every exit path, if any
        // event was applied: one clock read per request instead of per event.
        let mut applied_stamp = AppliedStamp {
            telemetry,
            pending: false,
        };
        let mut last_duplicate_entry: Option<DedupEntry> = None;

        for (idx, event) in req.events.into_iter().enumerate() {
            let is_batch = newly_applied_count > 0 || total_events > 1;
            let canonical_op = CanonicalOp::from_feeder_event(&event, lifecycle)?;
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
                            telemetry,
                            is_batch,
                            mode,
                            instance_id,
                            &req.instance_id,
                            idx,
                            seq,
                            &event.event_id,
                            ErrorCode::UnverifiableDuplicate,
                            "evicted duplicate",
                            ErrorCode::UnverifiableDuplicate,
                            err_msg.clone(),
                            axum::http::StatusCode::CONFLICT,
                            FeederError::UnverifiableDuplicate(err_msg),
                        );
                    }
                    DedupLookup::InWindow(retained) => {
                        if retained.event_id != event.event_id {
                            desynchronize(
                                &mut self.sync_status,
                                Desynchronization::ConflictingDuplicate,
                                telemetry,
                            );
                            let err_msg = format!(
                                "retained sequence {seq} has event_id '{}', received '{}'",
                                retained.event_id, event.event_id
                            );
                            return self.fail_batch_or_error(
                                telemetry,
                                is_batch,
                                mode,
                                instance_id,
                                &req.instance_id,
                                idx,
                                seq,
                                &event.event_id,
                                ErrorCode::ConflictingDuplicate,
                                "conflicting event ID",
                                ErrorCode::ConflictingDuplicate,
                                err_msg.clone(),
                                axum::http::StatusCode::CONFLICT,
                                FeederError::ConflictingDuplicate(err_msg),
                            );
                        }

                        let is_canonical_match = retained.source_position == event.source_position
                            && retained.canonical_op == canonical_op;
                        if !is_canonical_match {
                            desynchronize(
                                &mut self.sync_status,
                                Desynchronization::ConflictingDuplicate,
                                telemetry,
                            );
                            let err_msg = format!(
                                "conflicting duplicate at sequence {seq}: canonical content or source position altered"
                            );
                            return self.fail_batch_or_error(
                                telemetry,
                                is_batch,
                                mode,
                                instance_id,
                                &req.instance_id,
                                idx,
                                seq,
                                &event.event_id,
                                ErrorCode::ConflictingDuplicate,
                                "canonical conflict",
                                ErrorCode::ConflictingDuplicate,
                                err_msg.clone(),
                                axum::http::StatusCode::CONFLICT,
                                FeederError::ConflictingDuplicate(err_msg),
                            );
                        }

                        last_duplicate_entry = Some(retained.clone());
                        count_feeder_event(telemetry, FeederEventResult::Duplicate);
                        continue;
                    }
                    DedupLookup::Future => unreachable!(),
                }
            } else if let Some(expected_seq) = self.applied_delivery_sequence.checked_add(1) {
                if seq != expected_seq {
                    let expected = expected_seq;
                    return self.fail_batch_or_error(
                        telemetry,
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        ErrorCode::OutOfOrderSequence,
                        "out-of-order sequence",
                        ErrorCode::OutOfOrderSequence,
                        format!("expected delivery sequence {expected}, received {seq}"),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::OutOfOrderSequence {
                            expected,
                            received: seq,
                        },
                    );
                }

                if event.prev_delivery_sequence != self.applied_delivery_sequence {
                    desynchronize(
                        &mut self.sync_status,
                        Desynchronization::SourcePredecessorMismatch,
                        telemetry,
                    );
                    let err_msg = format!(
                        "prev_delivery_sequence {} does not match applied sequence {}",
                        event.prev_delivery_sequence, self.applied_delivery_sequence
                    );
                    return self.fail_batch_or_error(
                        telemetry,
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        ErrorCode::SourcePredecessorMismatch,
                        "predecessor sequence mismatch",
                        ErrorCode::SourcePredecessorMismatch,
                        err_msg.clone(),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::SourcePredecessorMismatch(err_msg),
                    );
                }

                let live_pos = self.source_position.as_deref().unwrap_or("");
                if event.prev_source_position != live_pos {
                    desynchronize(
                        &mut self.sync_status,
                        Desynchronization::SourcePredecessorMismatch,
                        telemetry,
                    );
                    let err_msg = format!(
                        "prev_source_position '{}' does not match applied source position '{}'",
                        event.prev_source_position, live_pos
                    );
                    return self.fail_batch_or_error(
                        telemetry,
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        ErrorCode::SourcePredecessorMismatch,
                        "source predecessor mismatch",
                        ErrorCode::SourcePredecessorMismatch,
                        err_msg.clone(),
                        axum::http::StatusCode::CONFLICT,
                        FeederError::SourcePredecessorMismatch(err_msg),
                    );
                }

                if self.dedup_window.contains_event_id(&event.event_id) {
                    desynchronize(
                        &mut self.sync_status,
                        Desynchronization::ConflictingDuplicate,
                        telemetry,
                    );
                    let err_msg = format!(
                        "event_id '{}' is already present in retained deduplication window",
                        event.event_id
                    );
                    return self.fail_batch_or_error(
                        telemetry,
                        is_batch,
                        mode,
                        instance_id,
                        &req.instance_id,
                        idx,
                        seq,
                        &event.event_id,
                        ErrorCode::ConflictingDuplicate,
                        "duplicate event ID",
                        ErrorCode::ConflictingDuplicate,
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
                                ErrorCode::NodeLimitExceeded,
                                format!("node limit {limit} exceeded"),
                            ),
                            _ => (ErrorCode::InvalidNode, err.to_string()),
                        })
                    }
                    CanonicalOp::Link { source, target } => {
                        if source == target {
                            Err((
                                ErrorCode::SelfLoop,
                                "self-loops are forbidden in simple undirected graphs".to_string(),
                            ))
                        } else {
                            self.graph.link(*source, *target).map_err(|err| match err {
                                GraphError::EdgeLimitExceeded { limit } => (
                                    ErrorCode::EdgeLimitExceeded,
                                    format!("edge limit {limit} exceeded"),
                                ),
                                GraphError::NodeLimitExceeded { limit } => (
                                    ErrorCode::NodeLimitExceeded,
                                    format!("node limit {limit} exceeded"),
                                ),
                                _ => (ErrorCode::InvalidEdge, err.to_string()),
                            })
                        }
                    }
                    CanonicalOp::Cut { source, target } => {
                        if source == target {
                            Err((
                                ErrorCode::SelfLoop,
                                "self-loops are forbidden in simple undirected graphs".to_string(),
                            ))
                        } else {
                            self.graph
                                .cut(*source, *target)
                                .map_err(|err| (ErrorCode::InvalidEdge, err.to_string()))
                        }
                    }
                    CanonicalOp::Progress => Ok(false),
                    CanonicalOp::SetNodeActive { node, active } => lifecycle_event(
                        self.graph
                            .lifecycle_mut()
                            .map(|graph| graph.set_node_active(*node, *active)),
                    ),
                    // The fingerprint is normalized (min, max); the facade
                    // gets the supplied order so that error precedence and
                    // messages match direct operations (`u` before `v`).
                    CanonicalOp::SetEdgeEnabled { enabled, .. } => {
                        let (Some(source), Some(target)) = (event.source, event.target) else {
                            unreachable!("prevalidated set_edge_enabled has both endpoints")
                        };
                        lifecycle_event(
                            self.graph
                                .lifecycle_mut()
                                .map(|graph| graph.set_edge_enabled(source.0, target.0, *enabled)),
                        )
                    }
                    CanonicalOp::RemoveNode { node } => lifecycle_event(
                        self.graph
                            .lifecycle_mut()
                            .map(|graph| Ok(graph.remove_node(*node))),
                    ),
                };

                let changed = match changed_result {
                    Ok(ch) => ch,
                    Err((err_code, err_msg)) => {
                        return self.fail_batch_or_error(
                            telemetry,
                            is_batch,
                            mode,
                            instance_id,
                            &req.instance_id,
                            idx,
                            seq,
                            &event.event_id,
                            ErrorCode::EventFailed,
                            "domain error",
                            err_code,
                            err_msg.clone(),
                            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                            FeederError::Domain(err_code, err_msg),
                        );
                    }
                };

                let result = if changed && !matches!(canonical_op, CanonicalOp::Progress) {
                    FeederEventResult::Changed
                } else {
                    FeederEventResult::Unchanged
                };
                if changed && !matches!(canonical_op, CanonicalOp::Progress) {
                    self.increment_version();
                }
                self.applied_delivery_sequence = next_seq;
                // `clone_from` reuses the retained position's capacity; the
                // owned event's strings then move into the window entry.
                self.source_position
                    .get_or_insert_default()
                    .clone_from(&event.source_position);
                self.dedup_window.record(DedupEntry {
                    delivery_sequence: next_seq,
                    event_id: event.event_id,
                    source_position: event.source_position,
                    canonical_op,
                    changed,
                });
                newly_applied_count += 1;
                count_feeder_event(telemetry, result);
                applied_stamp.pending = true;
            } else {
                // Applied sequence counter is u64::MAX and cannot be incremented.
                // Note: since seq is u64, any seq > u64::MAX is impossible on the wire.
                // This branch serves as an internal defensive guard against sequence overflow.
                count_feeder_event(telemetry, FeederEventResult::Rejected);
                return Err(FeederError::SequenceExhausted);
            }
        }

        if total_events == 1 && newly_applied_count == 0 {
            let dup = last_duplicate_entry.expect("retained duplicate entry");
            let response = FeederDuplicateResponse {
                result: FeederResult::Duplicate,
                status: DuplicateStatus::AlreadyApplied,
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
            result: FeederResult::Applied,
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
        telemetry: Option<&ServiceTelemetry>,
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
            generation = self.generation,
            reason = ?req.reason.as_deref().map(crate::logging::sanitize),
            "feeder declared unrecoverable source gap; transitioning graph to Desynchronized"
        );
        desynchronize(
            &mut self.sync_status,
            Desynchronization::GapDeclared,
            telemetry,
        );
        let metadata = self.response_metadata(mode, instance_id);
        let res = GapResponse {
            result: FeederResult::GapAcknowledged,
            instance_id: req.instance_id,
            generation: self.generation,
            status: SyncStatus::Desynchronized,
            action_required: RequiredAction::FullRebuildRequired,
        };
        Ok((res, metadata))
    }
}

/// Owned coherent state, serialized after releasing the graph lock.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GraphSnapshot {
    node_count: usize,
    edge_count: usize,
    /// Present only with lifecycle support; flattened into the object.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    lifecycle: Option<crate::graph::LifecycleCounts>,
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
    #[cfg(test)]
    pub(crate) fn etag(&self) -> String {
        let pos_encoded = self
            .source_position
            .as_deref()
            .map(percent_encode_position)
            .unwrap_or_default();
        self.etag_with_encoded_position(&pos_encoded)
    }

    /// Formats the same composite ETag from an already percent-encoded
    /// source position, so response construction encodes it only once.
    pub(crate) fn etag_with_encoded_position(&self, pos_encoded: &str) -> String {
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

/// Whether a graph job can change state. Only state-changing jobs produce
/// uncertain-outcome observations when abandoned or panicking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobEffect {
    ReadOnly,
    MayChangeState,
}

/// The single import permit, owned by the HTTP handler while it reads the
/// body and then by the import worker.
///
/// Carries the import's [`ImportObservation`] when import metrics exist.
/// Fields drop in declaration order, so the observation records the import
/// and clears `in_progress` before the semaphore permit is released.
pub(crate) struct ImportPermit {
    observation: Option<ImportObservation>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl ImportPermit {
    /// Ends the `receive` phase; `complete` is false when the body read failed.
    pub(crate) fn received(&mut self, complete: bool) {
        if let Some(observation) = &mut self.observation {
            observation.received(complete);
        }
    }

    fn observe(&mut self, step: fn(&mut ImportObservation)) {
        if let Some(observation) = &mut self.observation {
            step(observation);
        }
    }
}

/// Everything the import worker closure owns for accounting.
///
/// Fields drop in declaration order, whether the closure runs or is dropped
/// unrun: the import is recorded and its permit released before the job
/// count falls, so shutdown cannot report the jobs drained while an import
/// is still being recorded or still holds its permit.
struct ImportJob {
    import: ImportPermit,
    _job_guard: crate::lifecycle::JobGuard,
}

/// Worker-owned RAII failure guard ensuring any panic during execution immediately
/// marks the service unavailable, even if the awaiting async caller was cancelled.
///
/// It also records `worker_panic` for state-changing jobs. Counting here,
/// on the worker, rather than in the caller's `JoinError` mapping keeps the
/// observation when the caller is gone and avoids counting it twice.
struct WorkerPanicGuard {
    unavailable: Arc<AtomicBool>,
    // Set only for state-changing jobs when metrics are enabled.
    panic_telemetry: Option<Arc<ServiceTelemetry>>,
    success: bool,
}

impl Drop for WorkerPanicGuard {
    fn drop(&mut self) {
        if !self.success && std::thread::panicking() {
            self.unavailable.store(true, Ordering::Release);
            if let Some(telemetry) = &self.panic_telemetry {
                telemetry.uncertain(Uncertainty::WorkerPanic);
            }
        }
    }
}

/// Counts one evaluated feeder event when telemetry is enabled.
fn count_feeder_event(telemetry: Option<&ServiceTelemetry>, result: FeederEventResult) {
    if let Some(telemetry) = telemetry {
        telemetry.count_feeder_event(result);
    }
}

/// Stamps the last-applied instant when dropped after at least one applied,
/// non-duplicate event.
///
/// Feeder batches apply up to `MAX_BATCH_OPERATIONS` events under the graph
/// lock; reading the clock once per request rather than once per event keeps
/// that work out of the lock hold. The guard is dropped while the lock is
/// still held, on success, partial failure, early error and unwinding alike,
/// so stamps stay ordered. The gauge has millisecond resolution, so the
/// later read changes no observable ordering between requests.
struct AppliedStamp<'a> {
    telemetry: Option<&'a ServiceTelemetry>,
    pending: bool,
}

impl Drop for AppliedStamp<'_> {
    fn drop(&mut self) {
        if self.pending
            && let Some(telemetry) = self.telemetry
        {
            telemetry.mark_applied();
        }
    }
}

/// Moves the graph into `desynchronized`, counting the transition only when
/// it was not already there. Takes the status field alone so callers may
/// still borrow other graph-state fields.
fn desynchronize(
    status: &mut SyncStatus,
    cause: Desynchronization,
    telemetry: Option<&ServiceTelemetry>,
) {
    if *status != SyncStatus::Desynchronized
        && let Some(telemetry) = telemetry
    {
        telemetry.count_desynchronization(cause);
    }
    *status = SyncStatus::Desynchronized;
}

/// Counts one rejected feeder request when telemetry is enabled and the
/// error is a feeder-protocol rejection.
/// Logs (always) and counts (with metrics) a feeder rejection by its code.
fn count_feeder_rejection(telemetry: Option<&ServiceTelemetry>, error: &FeederError) {
    if let Some(code) = FeederRejection::of(error) {
        crate::logging::log_feeder_rejection(code.label());
        if let Some(telemetry) = telemetry {
            telemetry.count_feeder_rejection(code);
        }
    }
}

/// Publishes the live graph's counts while the caller holds its lock.
fn publish_graph_counts(telemetry: Option<&ServiceTelemetry>, state: &GraphState) {
    if let Some(telemetry) = telemetry {
        telemetry.publish_graph_counts(&state.graph);
    }
}

impl Service {
    /// Creates the service; zero capacity intentionally rejects every request.
    #[allow(dead_code)]
    pub(crate) fn new(
        graph: impl Into<ServiceGraph>,
        capacity: usize,
        lifecycle: Lifecycle,
    ) -> Self {
        let config = crate::ServerConfig::default();
        Self::with_config(graph, capacity, lifecycle, &config)
    }

    /// Creates the service with validated startup settings.
    pub(crate) fn with_config(
        graph: impl Into<ServiceGraph>,
        capacity: usize,
        lifecycle: Lifecycle,
        config: &crate::ServerConfig,
    ) -> Self {
        let graph = graph.into();
        let engine = graph.engine();
        let topology_lifecycle = graph.is_lifecycle();
        let mode = config.mode();
        let instance_id = fresh_instance_id();
        let max_import_body_bytes = config.max_import_body_bytes();
        let telemetry = config
            .metrics_enabled()
            .then(|| ServiceTelemetry::new(mode, topology_lifecycle));
        if let Some(telemetry) = &telemetry {
            telemetry.publish_graph_counts(&graph);
        }

        let (mode_str, inst_id_opt) = match mode {
            ServerMode::Standalone => (None, None),
            ServerMode::SourceManaged => (Some(mode.as_str()), Some(instance_id.clone())),
        };

        Self(Arc::new(ServiceInner {
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
                topology_lifecycle,
            },
            graph: Arc::new(Mutex::new(GraphState::new(
                graph,
                config.dedup_window_size(),
            ))),
            permits: Arc::new(Semaphore::new(capacity)),
            import_permits: Arc::new(Semaphore::new(IMPORT_PERMIT_CAPACITY)),
            unavailable: Arc::new(AtomicBool::new(false)),
            published_readiness: Arc::new(AtomicU64::new(PublishedReadiness::pack(
                SyncStatus::Uninitialized,
                0,
                0,
            ))),
            lifecycle,
            mode,
            instance_id: instance_id.into(),
            max_import_body_bytes,
            telemetry,
        }))
    }

    /// Rejection and uncertain-outcome counters, present when metrics are enabled.
    pub(crate) fn telemetry(&self) -> Option<&Arc<ServiceTelemetry>> {
        self.telemetry.as_ref()
    }

    /// Counts a request refused before its graph job or import ran.
    pub(crate) fn count_rejection(&self, reason: Rejection) {
        if let Some(telemetry) = &self.telemetry {
            telemetry.reject(reason);
        }
    }

    fn telemetry_for(&self, effect: JobEffect) -> Option<Arc<ServiceTelemetry>> {
        self.telemetry
            .as_ref()
            .filter(|_| effect == JobEffect::MayChangeState)
            .cloned()
    }

    /// Acquires the single import permit before reading request body.
    pub(crate) fn acquire_import_permit(&self) -> Result<ImportPermit, FeederError> {
        let permit = self
            .import_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                tracing::warn!("import capacity exhausted; rejecting with 503 import_in_progress");
                self.count_rejection(Rejection::ImportCapacity);
                FeederError::ImportInProgress
            })?;
        Ok(ImportPermit {
            observation: ImportObservation::acquired(self.telemetry.as_ref()),
            _permit: permit,
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
    pub(crate) fn topology_lifecycle(&self) -> bool {
        self.info.topology_lifecycle
    }

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
        if *req.instance_id != *self.instance_id {
            return Err(FeederError::StaleInstanceId(format!(
                "request instance_id '{}' does not match server instance_id '{}'",
                req.instance_id, self.instance_id
            )));
        }
        if !(MIN_LEASE_DURATION_MS..=MAX_LEASE_DURATION_MS).contains(&req.lease_duration_ms) {
            let error = FeederError::InvalidLeaseDuration;
            count_feeder_rejection(self.telemetry.as_deref(), &error);
            return Err(error);
        }
        let published_atomic = self.published_readiness.clone();
        let instance_id = Arc::clone(&self.instance_id);
        let telemetry = self.telemetry.clone();
        self.run(
            JobKind::FeederSession,
            JobEffect::MayChangeState,
            move |state| {
                let res = state
                    .register_session(req, instance_id.to_string())
                    .inspect_err(|error| count_feeder_rejection(telemetry.as_deref(), error))?;
                let word = state.current_readiness_word();
                published_atomic.store(word, Ordering::Release);
                Ok(res)
            },
        )
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
        if *req.instance_id != *self.instance_id {
            return Err(FeederError::StaleInstanceId(format!(
                "request instance_id '{}' does not match server instance_id '{}'",
                req.instance_id, self.instance_id
            )));
        }
        let published_atomic = self.published_readiness.clone();
        let instance_id = Arc::clone(&self.instance_id);
        let telemetry = self.telemetry.clone();
        self.run(
            JobKind::FeederHeartbeat,
            JobEffect::MayChangeState,
            move |state| {
                let res = state
                    .heartbeat(req, instance_id.to_string())
                    .inspect_err(|error| count_feeder_rejection(telemetry.as_deref(), error))?;
                let word = state.current_readiness_word();
                published_atomic.store(word, Ordering::Release);
                Ok(res)
            },
        )
        .await
        .map_err(FeederError::from)?
    }

    /// Atomically constructs and publishes a new graph topology on a blocking worker.
    pub(crate) async fn import(
        &self,
        permit: ImportPermit,
        raw_bytes: axum::body::Bytes,
        if_match: Option<String>,
    ) -> Result<ImportResponse, FeederError> {
        let job_guard = self.lifecycle.track_job();
        let lifecycle = self.lifecycle.clone();
        let graph = self.graph.clone();
        let unavailable = self.unavailable.clone();
        let failure_flag = self.unavailable.clone();
        let published_atomic = self.published_readiness.clone();
        let telemetry = self.telemetry.clone();
        let panic_telemetry = self.telemetry_for(JobEffect::MayChangeState);
        let engine = self.info.engine;
        let topology_lifecycle = self.info.topology_lifecycle;
        let limits = knotrel_core::GraphLimits {
            max_nodes: self.info.max_nodes,
            max_edges: self.info.max_edges,
        };
        let instance_id = Arc::clone(&self.instance_id);

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

        let job = ImportJob {
            import: permit,
            _job_guard: job_guard,
        };
        let clock = JobClock::submit(self.telemetry.as_ref(), JobKind::ImportPublish);
        // The worker logs inside the submitting request's span (M6.3).
        let span = tracing::Span::current();
        let handle = tokio::task::spawn_blocking(move || {
            let _span = span.enter();
            JobClock::start(clock.as_ref());
            // Dropped last, after the lock and the request state, so the
            // `publish` phase and the terminal result include them. Bound as
            // a whole so the closure captures it as one value.
            let mut job = job;
            job.import.observe(ImportObservation::enter_worker);
            let mut panic_guard = WorkerPanicGuard {
                unavailable: unavailable.clone(),
                panic_telemetry,
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
            job.import.observe(ImportObservation::begin_prepare);

            // 1. Off-lock JSON parsing with a streaming deserializer (see
            // `parse_import_body`), reading the body in place. The body is
            // released before candidate building, so peak memory does not
            // hold it alongside the parsed request and the candidate graph.
            let req: ImportRequest = {
                let parsed = crate::api::parse_import_body(&raw_bytes);
                drop(raw_bytes);
                parsed.map_err(|message| match message {
                    Some(message) => FeederError::UnprocessableJson(message),
                    None => FeederError::MalformedJson,
                })?
            };

            if *req.instance_id != *instance_id {
                return Err(FeederError::StaleInstanceId(format!(
                    "request instance_id '{}' does not match server instance_id '{}'",
                    req.instance_id, instance_id
                )));
            }

            if req.graph_id != DEFAULT_GRAPH_ID {
                return Err(FeederError::InvalidGraphId(format!(
                    "unsupported graph_id '{}'; only '{DEFAULT_GRAPH_ID}' is supported",
                    req.graph_id,
                )));
            }

            // 2. Off-lock candidate building
            let engine_config = engine_from_label(engine);
            let candidate = build_import_candidate(
                req.nodes,
                req.edges,
                engine_config,
                limits,
                topology_lifecycle,
            )?;

            job.import.observe(ImportObservation::begin_publish);
            let mut state = lock_timed(&graph, clock.as_ref(), &lifecycle);
            if state.failed || unavailable.load(Ordering::Acquire) {
                panic_guard.unavailable.store(true, Ordering::Release);
                if let Some(telemetry) = &telemetry {
                    telemetry.reject(Rejection::Unavailable);
                }
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
            let before = state.sync_point();
            state.failed = true;
            // The replaced graph is destroyed after the lock is released (one
            // free per adjacency and incident list), not in the critical
            // section; queued requests already see the new generation.
            let replaced = std::mem::replace(&mut state.graph, candidate);
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
            // Starts this generation's catch-up interval, superseding any
            // unfinished one of the replaced generation.
            let kind = match req.mode {
                ImportMode::CreateOnly => CatchupKind::Initial,
                ImportMode::Replace => CatchupKind::Replacement,
            };
            state.catchup = CatchupInterval::start(telemetry.as_deref(), kind);
            state.completed_catchup = None;

            initial_guard.success = true;
            panic_guard.success = true;
            job.import.observe(ImportObservation::published);
            let word = state.current_readiness_word();
            published_atomic.store(word, Ordering::Release);

            let node_count = state.graph.node_count();
            let edge_count = state.graph.edge_count();
            if let Some(telemetry) = &telemetry {
                telemetry.publish_graph_counts(&state.graph);
                telemetry.mark_applied();
            }
            tracing::info!(
                mode = match req.mode {
                    ImportMode::CreateOnly => "create_only",
                    ImportMode::Replace => "replace",
                },
                from_generation = before.generation,
                generation = state.generation,
                node_count,
                edge_count,
                source_id = %crate::logging::sanitize(state.source_id.as_deref().unwrap_or("")),
                source_position = %crate::logging::sanitize(&req.source_position),
                "graph published by import"
            );
            before.log_transition(
                state.sync_point(),
                "import_publish",
                Some(&req.source_position),
            );

            let response = ImportResponse {
                result: FeederResult::Published,
                instance_id: req.instance_id,
                graph_id: req.graph_id,
                generation: state.generation,
                state_version: "0".to_string(),
                node_count,
                edge_count,
                lifecycle: state.graph.lifecycle_counts(),
                source_position: req.source_position,
                status: SyncStatus::CatchingUp,
            };
            drop(state);
            lifecycle.check_worker_post_lock_hook();
            // Still inside the worker: job accounting and drain include it.
            drop(replaced);
            Ok(response)
        });
        let pending = PendingOutcome::arm(self.telemetry_for(JobEffect::MayChangeState));
        let joined = handle.await;
        pending.disarm();
        joined.map_err(|error| {
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
        self.execute_with(JobKind::Batch, operations).await
    }

    /// Executes one `/v1/operations` request; see [`Service::execute`].
    pub(crate) async fn execute_operation(
        &self,
        operation: Operation,
    ) -> Result<ExecutionResult, ServiceError> {
        self.execute_with(JobKind::Operation, vec![operation]).await
    }

    async fn execute_with(
        &self,
        kind: JobKind,
        operations: Vec<Operation>,
    ) -> Result<ExecutionResult, ServiceError> {
        let mode = self.mode;
        let instance_id = Arc::clone(&self.instance_id);
        let telemetry = self.telemetry.clone();
        let effect = if operations.iter().all(Operation::is_query) {
            JobEffect::ReadOnly
        } else {
            JobEffect::MayChangeState
        };
        self.run(kind, effect, move |state| {
            if mode == ServerMode::SourceManaged {
                if state.generation == 0 || state.sync_status == SyncStatus::Uninitialized {
                    let outcomes = operations
                        .into_iter()
                        .map(|_| Outcome::Error {
                            code: ErrorCode::Uninitialized,
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
                            code: ErrorCode::Loading,
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
                            code: ErrorCode::Desynchronized,
                            message: "graph is desynchronized; full rebuild required".into(),
                            expected_sequence: None,
                        })
                        .collect();
                    let metadata = state.response_metadata(mode, &instance_id);
                    return ExecutionResult { outcomes, metadata };
                }
            }
            // Each operation is counted once when it returns, so a panic keeps
            // earlier counts and invents no outcome for the interrupted one.
            let outcomes = operations
                .into_iter()
                .map(|operation| match telemetry.as_deref() {
                    None => state.apply(operation),
                    Some(telemetry) => {
                        let op = GraphOp::of(&operation);
                        let outcome = state.apply(operation);
                        telemetry.count_operation(op, &outcome);
                        outcome
                    }
                })
                .collect();
            let metadata = state.response_metadata(mode, &instance_id);
            ExecutionResult { outcomes, metadata }
        })
        .await
    }

    /// Takes a coherent snapshot through the same bounded worker path as writes.
    pub(crate) async fn stats(&self) -> Result<StatsResult, ServiceError> {
        let mode = self.mode;
        let instance_id = Arc::clone(&self.instance_id);
        let current_sync_status = self.sync_status();
        self.run(JobKind::Stats, JobEffect::ReadOnly, move |state| {
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
        kind: JobKind,
        mutate: bool,
        op: F,
    ) -> Result<T, FeederError>
    where
        T: Send + 'static,
        F: FnOnce(&mut GraphState, ServerMode, &str) -> Result<T, FeederError> + Send + 'static,
    {
        let mode = self.mode;

        if self.is_unavailable() {
            self.count_rejection(Rejection::Unavailable);
            return Err(FeederError::Unavailable);
        }

        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(
                capacity = self.info.max_pending_jobs,
                "request capacity exhausted; rejecting with 503 busy"
            );
            // Counted as `busy`; the HTTP mapping keeps `import_in_progress`.
            self.count_rejection(Rejection::Busy);
            FeederError::from(ServiceError::Busy)
        })?;
        let job_guard = self.lifecycle.track_job();
        // One shared handle gives the worker every field it needs with a
        // single reference-count increment.
        let service = self.clone();
        let panic_telemetry = self.telemetry_for(JobEffect::MayChangeState);
        let clock = JobClock::submit(self.telemetry.as_ref(), kind);
        // The worker logs inside the submitting request's span (M6.3).
        let span = tracing::Span::current();

        let handle = tokio::task::spawn_blocking(move || {
            let _span = span.enter();
            JobClock::start(clock.as_ref());
            let _job_guard = job_guard;
            let _permit = permit;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: Arc::clone(&service.unavailable),
                panic_telemetry,
                success: false,
            };
            let lifecycle = &service.lifecycle;
            let telemetry = &service.telemetry;
            lifecycle.check_worker_hook();
            // Declared before the lock, so it observes after the lock is
            // released, also when unwinding.
            let mut catchup = CatchupRecorder::new(telemetry.clone());
            let mut state = lock_timed(&service.graph, clock.as_ref(), lifecycle);
            if state.failed || service.unavailable.load(Ordering::Acquire) {
                panic_guard.unavailable.store(true, Ordering::Release);
                if let Some(telemetry) = telemetry {
                    telemetry.reject(Rejection::Unavailable);
                }
                return Err(FeederError::Unavailable);
            }
            if mutate {
                state.failed = true;
            }
            let before = state.sync_point();
            let result = op(&mut state, mode, &service.instance_id);
            before.log_transition(
                state.sync_point(),
                worker_name,
                state.source_position.as_deref(),
            );
            catchup.record(state.completed_catchup.take());
            if let Err(error) = &result {
                count_feeder_rejection(telemetry.as_deref(), error);
            }
            let word = state.current_readiness_word();
            service.published_readiness.store(word, Ordering::Release);
            if mutate {
                state.failed = false;
                // Also after a partial failure: its applied prefix is live.
                publish_graph_counts(telemetry.as_deref(), &state);
            }
            drop(state);
            lifecycle.check_worker_post_lock_hook();
            panic_guard.success = true;
            result
        });
        let pending = PendingOutcome::arm(self.telemetry_for(JobEffect::MayChangeState));
        let joined = handle.await;
        pending.disarm();
        joined.map_err(|error| {
            tracing::error!(
                panicked = error.is_panic(),
                "{worker_name} worker thread failed"
            );
            self.unavailable.store(true, Ordering::Release);
            FeederError::Unavailable
        })?
    }

    /// Applies an ordered stream of feeder events under the graph mutex.
    pub(crate) async fn apply_events(
        &self,
        req: FeederEventsRequest,
    ) -> Result<FeederEventsResult, FeederError> {
        let telemetry = self.telemetry.clone();
        self.execute_feeder_worker(
            "events",
            JobKind::FeederEvents,
            true,
            move |state, mode, inst| state.apply_events(req, mode, inst, telemetry.as_deref()),
        )
        .await
    }

    /// Acknowledges an unrecoverable source gap, moving server to Desynchronized under mutex.
    pub(crate) async fn declare_gap(
        &self,
        req: GapRequest,
    ) -> Result<(GapResponse, ResponseMetadata), FeederError> {
        let telemetry = self.telemetry.clone();
        self.execute_feeder_worker("gap", JobKind::FeederGap, true, move |state, mode, inst| {
            state.declare_gap(req, mode, inst, telemetry.as_deref())
        })
        .await
    }

    /// Verifies a synchronization barrier under the graph mutex.
    pub(crate) async fn declare_barrier(
        &self,
        req: BarrierRequest,
    ) -> Result<(BarrierResponse, ResponseMetadata), FeederError> {
        self.execute_feeder_worker(
            "barrier",
            JobKind::FeederBarrier,
            false,
            move |state, mode, inst| state.declare_barrier(req, mode, inst),
        )
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
        let marked = self
            .published_readiness
            .compare_exchange(
                observed_word,
                stale_word,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if marked {
            tracing::warn!(
                from = SyncStatus::Ready.as_str(),
                to = SyncStatus::Stale.as_str(),
                "feeder lease expired; readiness is stale"
            );
        }
        marked
    }

    // One admission/cancellation/poisoning policy for reads and writes. The
    // owned permit stays with the blocking closure even if its caller is gone.
    async fn run<R: Send + 'static>(
        &self,
        kind: JobKind,
        effect: JobEffect,
        work: impl FnOnce(&mut GraphState) -> R + Send + 'static,
    ) -> Result<R, ServiceError> {
        if self.is_unavailable() {
            self.count_rejection(Rejection::Unavailable);
            return Err(ServiceError::Unavailable);
        }

        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(
                capacity = self.info.max_pending_jobs,
                "request capacity exhausted; rejecting with 503 busy"
            );
            self.count_rejection(Rejection::Busy);
            ServiceError::Busy
        })?;
        let job_guard = self.lifecycle.track_job();
        // One shared handle gives the worker the graph, lifecycle, failure
        // flag and telemetry with a single reference-count increment.
        let service = self.clone();
        let panic_telemetry = self.telemetry_for(effect);
        let clock = JobClock::submit(self.telemetry.as_ref(), kind);
        // The worker logs inside the submitting request's span (M6.3).
        let span = tracing::Span::current();
        let handle = tokio::task::spawn_blocking(move || {
            let _span = span.enter();
            JobClock::start(clock.as_ref());
            let _job_guard = job_guard;
            let _permit = permit;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: Arc::clone(&service.unavailable),
                panic_telemetry,
                success: false,
            };
            let lifecycle = &service.lifecycle;
            let telemetry = service.telemetry.as_deref();
            lifecycle.check_worker_hook();
            let mut graph = lock_timed(&service.graph, clock.as_ref(), lifecycle);
            if graph.failed || service.unavailable.load(Ordering::Acquire) {
                tracing::warn!("graph worker state marked as failed; rejecting work");
                panic_guard.unavailable.store(true, Ordering::Release);
                if let Some(telemetry) = telemetry {
                    telemetry.reject(Rejection::Unavailable);
                }
                return Err(ServiceError::Unavailable);
            }
            graph.failed = true;
            let before = graph.sync_point();
            let result = work(&mut graph);
            graph.failed = false;
            if effect == JobEffect::MayChangeState {
                before.log_transition(
                    graph.sync_point(),
                    kind.label(),
                    graph.source_position.as_deref(),
                );
                publish_graph_counts(telemetry, &graph);
            }
            drop(graph);
            lifecycle.check_worker_post_lock_hook();
            panic_guard.success = true;
            Ok(result)
        });
        let pending = PendingOutcome::arm(self.telemetry_for(effect));
        let joined = handle.await;
        pending.disarm();
        joined.map_err(|error| {
            // Never log the error itself: a panic message may embed request data.
            tracing::error!(panicked = error.is_panic(), "graph worker thread failed");
            self.unavailable.store(true, Ordering::Release);
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
                ConnectivityGraph::default().into(),
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
                .run(JobKind::Operation, JobEffect::MayChangeState, |_| {
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
    async fn panic_during_a_lifecycle_mutation_fails_closed() {
        let graph = ServiceGraph::empty(
            EngineConfig::CompactBfs,
            knotrel_core::GraphLimits::default(),
            true,
        );
        let service = Service::new(graph, 2, Lifecycle::new());
        assert!(service.execute(vec![link(1, 2), link(2, 3)]).await.is_ok());
        let writer = service.clone();
        let result = writer
            .run(
                JobKind::Operation,
                JobEffect::MayChangeState,
                move |state| {
                    let outcome = state.apply(Operation::SetNodeActive {
                        node: crate::api::WireNode(2),
                        active: false,
                    });
                    assert!(matches!(outcome, Outcome::Changed { changed: true }));
                    panic!("panic after a lifecycle mutation");
                },
            )
            .await;
        assert!(matches!(result, Err(ServiceError::Unavailable)));
        assert!(service.is_unavailable());
        assert!(matches!(
            service
                .execute(vec![Operation::RemoveNode {
                    node: crate::api::WireNode(1)
                }])
                .await,
            Err(ServiceError::Unavailable)
        ));
        assert!(matches!(
            service.stats().await,
            Err(ServiceError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn panicked_writer_rejects_queued_and_future_work() {
        let service = Service::new(ConnectivityGraph::default(), 2, Lifecycle::new());
        let writer = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            writer
                .run(
                    JobKind::Operation,
                    JobEffect::MayChangeState,
                    move |state| {
                        state.apply(add(1));
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        panic!("panic after partial mutation");
                    },
                )
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
                .run(
                    JobKind::Operation,
                    JobEffect::MayChangeState,
                    move |state| {
                        state.apply(add(1));
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        panic!("test worker panic after caller cancel");
                    },
                )
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
                .run(
                    JobKind::Operation,
                    JobEffect::MayChangeState,
                    move |state| {
                        state.apply(add(1));
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        state.apply(add(2));
                    },
                )
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
            s1.run(
                JobKind::Operation,
                JobEffect::MayChangeState,
                move |state| {
                    state.apply(add(1));
                    w1_started_tx.send(()).unwrap();
                    w1_release_rx.recv().unwrap();
                },
            )
            .await
        });
        w1_started_rx.await.unwrap();
        assert_eq!(lifecycle.outstanding_jobs(), 1);

        let s2 = service.clone();
        let job2 = tokio::spawn(async move {
            s2.run(
                JobKind::Operation,
                JobEffect::MayChangeState,
                move |state| {
                    state.apply(add(2));
                    w2_done_tx.send(()).unwrap();
                },
            )
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

    #[tokio::test]
    async fn in_worker_unavailable_refusal_is_counted_once_at_its_boundary() {
        let lifecycle = Lifecycle::ready();
        let config = crate::ServerConfig::default().with_metrics_enabled(true);
        let service = Service::with_config(ConnectivityGraph::default(), 1, lifecycle, &config);
        let telemetry = Arc::clone(service.telemetry().unwrap());

        // Passes the entry check; the worker refuses under the graph lock.
        service.graph.lock().failed = true;
        let result = service.execute(vec![add(1)]).await;
        assert!(matches!(result, Err(ServiceError::Unavailable)));
        assert_eq!(telemetry.rejected_count(Rejection::Unavailable), 1);

        // The worker published the flag, so the entry check refuses next.
        let result = service.stats().await;
        assert!(matches!(result, Err(ServiceError::Unavailable)));
        assert_eq!(telemetry.rejected_count(Rejection::Unavailable), 2);
        for reason in [
            Rejection::Busy,
            Rejection::ImportCapacity,
            Rejection::ShuttingDown,
        ] {
            assert_eq!(telemetry.rejected_count(reason), 0);
        }
        for cause in [
            Uncertainty::RequestTimeout,
            Uncertainty::Cancelled,
            Uncertainty::WorkerPanic,
        ] {
            assert_eq!(telemetry.uncertain_count(cause), 0);
        }
    }

    #[tokio::test]
    async fn disabled_metrics_create_no_service_telemetry() {
        let service = Service::new(ConnectivityGraph::default(), 1, Lifecycle::ready());
        assert!(service.telemetry().is_none());
        service.graph.lock().failed = true;
        let result = service.execute(vec![add(1)]).await;
        assert!(matches!(result, Err(ServiceError::Unavailable)));
        assert!(service.telemetry().is_none());

        let config = crate::ServerConfig::default();
        let service =
            Service::with_config(ConnectivityGraph::default(), 1, Lifecycle::ready(), &config);
        assert!(service.telemetry().is_none());
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
                code: ErrorCode::SelfLoop,
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
        let _clock_guard = lock_clock_for_test().await;
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
        assert_eq!(session_res.result, FeederResult::SessionAcquired);

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
                active: None,
                enabled: None,
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
                active: None,
                enabled: None,
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
                active: None,
                enabled: None,
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
                assert_eq!(response.result, FeederResult::Duplicate);
                assert_eq!(response.status, DuplicateStatus::AlreadyApplied);
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
                active: None,
                enabled: None,
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
        let _clock_guard = lock_clock_for_test().await;
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
        assert_eq!(session_res.result, FeederResult::SessionAcquired);

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
                    active: None,
                    enabled: None,
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
                    active: None,
                    enabled: None,
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
                    active: None,
                    enabled: None,
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
        let _clock_guard = CLOCK_TEST_MUTEX.blocking_lock();
        let limits = knotrel_core::GraphLimits {
            max_nodes: Some(100),
            max_edges: Some(100),
        };
        let mut state = GraphState::new(
            ConnectivityGraph::with_limits(EngineConfig::CompactBfs, limits).into(),
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
        assert_eq!(b1_res.result, FeederResult::Synchronized);
        assert_eq!(b1_res.status, SyncStatus::Ready);
        assert_eq!(state.sync_status, SyncStatus::Ready);

        // Case B2: Exact match from Ready (idempotent)
        let (b2_res, _) = state
            .declare_barrier(
                make_req(1, 5, Some("pos-5")),
                ServerMode::SourceManaged,
                instance_id,
            )
            .unwrap();
        assert_eq!(b2_res.result, FeederResult::AlreadySynchronized);
        assert_eq!(b2_res.status, SyncStatus::Ready);
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
        let config = crate::ServerConfig::default().with_mode(ServerMode::SourceManaged);
        let service = Service::with_config(
            ConnectivityGraph::with_limits(EngineConfig::CompactBfs, limits),
            32,
            lifecycle.clone(),
            &config,
        );
        assert_eq!(service.info().mode, Some("source_managed"));

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
        assert_eq!(barrier_res.result, FeederResult::Synchronized);
        assert_eq!(barrier_res.status, SyncStatus::Ready);
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
    fn published_readiness_bit_fields_cover_the_word_without_overlap() {
        assert_eq!(STATUS_BITS + SEQUENCE_BITS + DEADLINE_BITS, u64::BITS);
        assert_eq!(SEQUENCE_SHIFT, STATUS_BITS);
        assert_eq!(DEADLINE_SHIFT, STATUS_BITS + SEQUENCE_BITS);
        let packed = PublishedReadiness::pack(SyncStatus::Desynchronized, u16::MAX, DEADLINE_MASK);
        assert_eq!(
            PublishedReadiness::unpack(packed),
            PublishedReadiness {
                status: SyncStatus::Desynchronized,
                seq: u16::MAX,
                deadline_millis: DEADLINE_MASK,
            }
        );
    }

    /// An engine label that does not come from `EngineConfig::as_str` is a
    /// programming error and fails loudly instead of substituting an engine.
    #[test]
    #[should_panic(expected = "does not come from EngineConfig::as_str")]
    fn unknown_engine_labels_are_never_substituted() {
        let _ = engine_from_label("not-an-engine");
    }

    /// The defensive branch of a lifecycle event on a plain graph still
    /// reports `topology_lifecycle_disabled`.
    #[test]
    fn lifecycle_event_on_a_plain_graph_reports_lifecycle_disabled() {
        let result = lifecycle_event(Err(crate::graph::LifecycleUnsupported));
        assert_eq!(
            result,
            Err((
                ErrorCode::TopologyLifecycleDisabled,
                crate::api::LIFECYCLE_DISABLED_MESSAGE.to_owned()
            ))
        );
    }

    #[test]
    fn engine_labels_round_trip_for_import_candidates() {
        for engine in [
            EngineConfig::CompactBfs,
            EngineConfig::EulerTour,
            EngineConfig::Hdt,
        ] {
            assert_eq!(engine_from_label(engine.as_str()), engine);
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
            active: None,
            enabled: None,
            node: None,
            source: None,
            target: None,
        };

        // Link missing source/target
        assert!(CanonicalOp::from_feeder_event(&base_event, false).is_err());
        // Link with node
        let mut evt = base_event.clone();
        evt.source = Some(WireNode(1));
        evt.target = Some(WireNode(2));
        evt.node = Some(WireNode(3));
        assert!(CanonicalOp::from_feeder_event(&evt, false).is_err());

        // Cut missing source/target
        let mut cut_evt = base_event.clone();
        cut_evt.op = "cut".into();
        assert!(CanonicalOp::from_feeder_event(&cut_evt, false).is_err());
        // Cut with node
        cut_evt.source = Some(WireNode(1));
        cut_evt.target = Some(WireNode(2));
        cut_evt.node = Some(WireNode(3));
        assert!(CanonicalOp::from_feeder_event(&cut_evt, false).is_err());

        // AddNode missing node
        let mut node_evt = base_event.clone();
        node_evt.op = "add_node".into();
        assert!(CanonicalOp::from_feeder_event(&node_evt, false).is_err());
        // AddNode with source
        node_evt.node = Some(WireNode(1));
        node_evt.source = Some(WireNode(1));
        assert!(CanonicalOp::from_feeder_event(&node_evt, false).is_err());

        // Progress with node or source
        let mut prog_evt = base_event.clone();
        prog_evt.op = "progress".into();
        assert!(CanonicalOp::from_feeder_event(&prog_evt, false).is_ok());
        prog_evt.node = Some(WireNode(1));
        assert!(CanonicalOp::from_feeder_event(&prog_evt, false).is_err());

        // Unsupported op
        let mut unk_evt = base_event.clone();
        unk_evt.op = "unsupported".into();
        assert!(CanonicalOp::from_feeder_event(&unk_evt, false).is_err());

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
        let _clock_guard = lock_clock_for_test().await;
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
            active: None,
            enabled: None,
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
        let _clock_guard = lock_clock_for_test().await;
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
        assert!(matches!(
            err,
            FeederError::Domain(ErrorCode::NodeLimitExceeded, _)
        ));

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
        assert!(matches!(err, FeederError::Domain(ErrorCode::SelfLoop, _)));

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
        assert!(matches!(
            err,
            FeederError::Domain(ErrorCode::EdgeLimitExceeded, _)
        ));
    }

    #[tokio::test]
    async fn test_service_uncovered_branches_and_edge_cases() {
        let _clock_guard = lock_clock_for_test().await;
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
            active: None,
            enabled: None,
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
                code: ErrorCode::Loading,
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
                code: ErrorCode::Desynchronized,
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
            active: None,
            enabled: None,
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
            Err(FeederError::Domain(ErrorCode::SelfLoop, _))
        ));

        // Batch returns PartialFailure
        let valid_evt = FeederEvent {
            event_id: "e_valid".into(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "1".into(),
            prev_source_position: "".into(),
            op: "add_node".into(),
            active: None,
            enabled: None,
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
                assert_eq!(failure.error.code, ErrorCode::SelfLoop);
            }
            _ => panic!("expected partial failure for self-loop cut in batch"),
        }
    }

    mod graph_counts {
        use super::*;
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt;

        fn metrics_config() -> crate::ServerConfig {
            crate::ServerConfig::default().with_metrics_enabled(true)
        }

        fn metrics_router(service: &Service) -> axum::Router {
            crate::build_router_from_service(service.clone(), metrics_config(), Lifecycle::ready())
        }

        async fn scrape_counts(app: &axum::Router) -> (String, String) {
            let response = app
                .clone()
                .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            let sample = |name: &str| {
                text.lines()
                    .find_map(|line| line.strip_prefix(&format!("{name} ")))
                    .unwrap_or_else(|| panic!("missing {name} in {text}"))
                    .to_owned()
            };
            (sample("knotrel_graph_nodes"), sample("knotrel_graph_edges"))
        }

        fn counts(nodes: &str, edges: &str) -> (String, String) {
            (nodes.to_owned(), edges.to_owned())
        }

        #[tokio::test]
        async fn embedded_nonempty_graph_is_reported_at_construction() {
            let mut graph = ConnectivityGraph::default();
            graph.link(1, 2).unwrap();
            graph.link(2, 3).unwrap();
            graph.add_node(9).unwrap();
            let service = Service::with_config(graph, 2, Lifecycle::ready(), &metrics_config());
            assert_eq!(
                scrape_counts(&metrics_router(&service)).await,
                counts("4", "2")
            );
        }

        #[tokio::test]
        async fn disabled_telemetry_publishes_nothing() {
            let service = Service::with_config(
                ConnectivityGraph::default(),
                2,
                Lifecycle::ready(),
                &crate::ServerConfig::default(),
            );
            service.execute(vec![link(1, 2)]).await.unwrap();
            assert!(service.telemetry().is_none());
        }

        /// Without telemetry the counting branch is skipped and every entry,
        /// including those after a domain error, keeps its outcome.
        #[tokio::test]
        async fn disabled_telemetry_counts_no_outcomes() {
            let service = Service::with_config(
                ConnectivityGraph::default(),
                2,
                Lifecycle::ready(),
                &crate::ServerConfig::default(),
            );
            let result = service
                .execute(vec![link(1, 2), link(3, 3), link(1, 2)])
                .await
                .unwrap();
            assert!(matches!(
                result.outcomes[..],
                [
                    Outcome::Changed { changed: true },
                    Outcome::Error {
                        code: ErrorCode::SelfLoop,
                        ..
                    },
                    Outcome::Changed { changed: false },
                ]
            ));
            assert!(service.telemetry().is_none());
        }

        /// The worker below is a real `run` job paused inside its locked
        /// region; `try_lock` proves the lock is held while the scrape completes.
        #[tokio::test]
        async fn scrape_completes_while_a_worker_holds_the_graph_lock() {
            let service = Service::with_config(
                ConnectivityGraph::default(),
                2,
                Lifecycle::ready(),
                &metrics_config(),
            );
            let app = metrics_router(&service);
            let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let worker = tokio::spawn({
                let service = service.clone();
                async move {
                    service
                        .run(
                            JobKind::Operation,
                            JobEffect::MayChangeState,
                            move |state| {
                                state.apply(link(1, 2));
                                locked_tx.send(()).unwrap();
                                let _ = release_rx.recv();
                            },
                        )
                        .await
                }
            });
            locked_rx.await.unwrap();
            assert!(service.graph.try_lock().is_none());

            let scraped = tokio::time::timeout(Duration::from_secs(5), scrape_counts(&app))
                .await
                .expect("scrape must not wait for the graph lock");
            assert_eq!(scraped, counts("0", "0"));
            assert!(service.graph.try_lock().is_none());

            release_tx.send(()).unwrap();
            worker.await.unwrap().unwrap();
            assert_eq!(scrape_counts(&app).await, counts("2", "1"));
        }
    }

    /// Rejection and desynchronization accounting reached only through
    /// direct state set-up: an expired lease and disabled telemetry.
    mod feeder_rejection_counts {
        use super::*;
        use crate::metrics::{Desynchronization, FeederRejection};

        fn seeded(metrics: bool, lease: Option<Instant>) -> Service {
            let config = crate::ServerConfig::default()
                .with_mode(ServerMode::SourceManaged)
                .with_metrics_enabled(metrics);
            let service =
                Service::with_config(ConnectivityGraph::default(), 2, Lifecycle::ready(), &config);
            {
                let mut state = service.graph.lock();
                state.generation = 1;
                state.source_id = Some("pg-1".into());
                state.source_epoch = Some("ep-1".into());
                state.source_position = Some("p-0".into());
                state.feeder_session_id = Some("s".into());
                state.feeder_epoch = 1;
                state.lease_deadline = lease;
            }
            service.set_sync_status_for_test(SyncStatus::CatchingUp);
            service
        }

        fn gap(service: &Service) -> crate::api::GapRequest {
            crate::api::GapRequest {
                instance_id: service.instance_id().to_owned(),
                graph_id: "default".into(),
                generation: 1,
                feeder_session_id: "s".into(),
                feeder_epoch: 1,
                failed_delivery_sequence: 1,
                reason: None,
            }
        }

        #[tokio::test]
        async fn expired_lease_is_one_rejection_without_a_transition() {
            let _clock = lock_clock_for_test().await;
            let expired = monotonic_now()
                .checked_sub(Duration::from_millis(1))
                .unwrap();
            let service = seeded(true, Some(expired));
            let result = service.declare_gap(gap(&service)).await;
            assert!(matches!(result, Err(FeederError::LeaseExpired)));
            let telemetry = service.telemetry().unwrap();
            for code in FeederRejection::ALL {
                let expected = u64::from(code == FeederRejection::LeaseExpired);
                assert_eq!(telemetry.feeder_rejection_count(code), expected, "{code:?}");
            }
            for cause in Desynchronization::ALL {
                assert_eq!(telemetry.desynchronization_count(cause), 0, "{cause:?}");
            }
        }

        /// Without telemetry nothing is counted and the gap still transitions.
        #[tokio::test]
        async fn disabled_telemetry_keeps_rejections_and_transitions() {
            let lease = monotonic_now() + Duration::from_secs(60);
            let service = seeded(false, Some(lease));
            assert!(service.telemetry().is_none());
            service.declare_gap(gap(&service)).await.unwrap();
            assert_eq!(service.graph.lock().sync_status, SyncStatus::Desynchronized);
            let result = service.declare_gap(crate::api::GapRequest {
                generation: 7,
                ..gap(&service)
            });
            assert!(matches!(result.await, Err(FeederError::StaleGeneration(_))));
            assert!(service.telemetry().is_none());
        }
    }

    /// Timing-boundary proofs. Each test advances the `cfg(test)` timing
    /// clock by `STEP` at one controlled point and checks which phase absorbs
    /// it; real elapsed time in these tests is far below `STEP`.
    mod job_timing {
        use super::*;
        use crate::metrics::{JobKind, advance_timing};
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt;

        /// Serializes tests that move the shared timing clock.
        pub(super) static CLOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        const STEP: Duration = Duration::from_secs(5);
        const PHASES: [&str; 3] = [
            "knotrel_graph_job_queue_seconds",
            "knotrel_graph_lock_wait_seconds",
            "knotrel_graph_job_execution_seconds",
        ];

        fn metrics_config() -> crate::ServerConfig {
            crate::ServerConfig::default().with_metrics_enabled(true)
        }

        fn timed_service(lifecycle: Lifecycle) -> Service {
            Service::with_config(
                ConnectivityGraph::default(),
                4,
                lifecycle,
                &metrics_config(),
            )
        }

        /// `(count, sum)` of the queue, lock-wait and execution histograms for `kind`.
        async fn phases(service: &Service, kind: &str) -> [(u64, f64); 3] {
            let app = crate::build_router_from_service(
                service.clone(),
                metrics_config(),
                Lifecycle::ready(),
            );
            let response = app
                .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            let read = |name: String| -> f64 {
                let prefix = format!("{name}{{kind=\"{kind}\"}} ");
                text.lines()
                    .find_map(|line| line.strip_prefix(&prefix))
                    .unwrap_or_else(|| panic!("missing {prefix} in {text}"))
                    .parse()
                    .unwrap()
            };
            PHASES.map(|phase| {
                (
                    read(format!("{phase}_count")) as u64,
                    read(format!("{phase}_sum")),
                )
            })
        }

        fn absorbed(phase: (u64, f64)) -> bool {
            phase.0 == 1 && phase.1 >= STEP.as_secs_f64()
        }

        fn excluded(phase: (u64, f64)) -> bool {
            phase.0 == 1 && phase.1 < STEP.as_secs_f64()
        }

        #[tokio::test]
        async fn execution_spans_work_under_the_graph_lock() {
            let _clock = CLOCK.lock().await;
            let service = timed_service(Lifecycle::ready());
            service
                .run(JobKind::Operation, JobEffect::MayChangeState, |_| {
                    advance_timing(STEP)
                })
                .await
                .unwrap();
            let [queue, lock_wait, execution] = phases(&service, "operation").await;
            assert!(excluded(queue), "{queue:?}");
            assert!(excluded(lock_wait), "{lock_wait:?}");
            assert!(absorbed(execution), "{execution:?}");
        }

        #[tokio::test]
        async fn panicking_work_records_execution_until_unwinding_releases_the_lock() {
            let _clock = CLOCK.lock().await;
            let service = timed_service(Lifecycle::ready());
            let result = service
                .run(JobKind::Batch, JobEffect::MayChangeState, |_| {
                    advance_timing(STEP);
                    panic!("injected worker panic");
                })
                .await;
            assert!(matches!(result, Err(ServiceError::Unavailable)));
            let [queue, lock_wait, execution] = phases(&service, "batch").await;
            assert!(excluded(queue), "{queue:?}");
            assert!(excluded(lock_wait), "{lock_wait:?}");
            assert!(absorbed(execution), "{execution:?}");
        }

        #[tokio::test]
        async fn worker_rejection_under_the_lock_is_timed_and_admission_rejection_is_not() {
            let service = timed_service(Lifecycle::ready());
            service.graph.lock().failed = true;
            assert!(matches!(
                service.execute(vec![add(1)]).await,
                Err(ServiceError::Unavailable)
            ));
            assert!(matches!(
                service.execute(vec![add(1)]).await,
                Err(ServiceError::Unavailable)
            ));
            let observed = phases(&service, "batch").await.map(|phase| phase.0);
            assert_eq!(observed, [1, 1, 1]);
        }

        /// The worker holds the graph lock inside `work` while the scrape
        /// completes. Queue time is already recorded; lock wait and execution
        /// are recorded only after the lock is released.
        #[tokio::test]
        async fn scrape_completes_while_a_timed_worker_holds_the_graph_lock() {
            let service = timed_service(Lifecycle::ready());
            let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let worker = tokio::spawn({
                let service = service.clone();
                async move {
                    service
                        .run(JobKind::Stats, JobEffect::ReadOnly, move |_| {
                            locked_tx.send(()).unwrap();
                            let _ = release_rx.recv();
                        })
                        .await
                }
            });
            locked_rx.await.unwrap();
            assert!(service.graph.try_lock().is_none());
            let observed = tokio::time::timeout(Duration::from_secs(5), phases(&service, "stats"))
                .await
                .expect("scrape must not wait for the graph lock")
                .map(|phase| phase.0);
            assert_eq!(observed, [1, 0, 0]);
            assert!(service.graph.try_lock().is_none());

            release_tx.send(()).unwrap();
            worker.await.unwrap().unwrap();
            let observed = phases(&service, "stats").await.map(|phase| phase.0);
            assert_eq!(observed, [1, 1, 1]);
        }

        #[cfg(feature = "test-support")]
        #[tokio::test]
        async fn execution_ends_when_the_graph_lock_is_released() {
            let _clock = CLOCK.lock().await;
            let lifecycle = Lifecycle::ready();
            let service = timed_service(lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker_after_lock();
            let worker = tokio::spawn({
                let service = service.clone();
                async move { service.execute_operation(add(1)).await }
            });
            pause.wait_started().await;
            advance_timing(STEP);
            pause.release();
            worker.await.unwrap().unwrap();
            for phase in phases(&service, "operation").await {
                assert!(excluded(phase), "{phase:?}");
            }
        }

        #[cfg(feature = "test-support")]
        #[tokio::test]
        async fn queue_ends_and_lock_wait_starts_around_the_pre_lock_hook() {
            let _clock = CLOCK.lock().await;
            let lifecycle = Lifecycle::ready();
            let service = timed_service(lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let worker = tokio::spawn({
                let service = service.clone();
                async move { service.stats().await }
            });
            pause.wait_started().await;
            advance_timing(STEP);
            pause.release();
            worker.await.unwrap().unwrap();
            for phase in phases(&service, "stats").await {
                assert!(excluded(phase), "{phase:?}");
            }
        }

        #[cfg(feature = "test-support")]
        #[tokio::test]
        async fn lock_wait_spans_the_lock_request_until_acquisition() {
            let _clock = CLOCK.lock().await;
            let lifecycle = Lifecycle::ready();
            let service = timed_service(lifecycle.clone());
            let graph = service.graph.clone();
            let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let holder = std::thread::spawn(move || {
                let _guard = graph.lock();
                locked_tx.send(()).unwrap();
                let _ = release_rx.recv();
            });
            locked_rx.await.unwrap();
            let mut pause = lifecycle.pause_next_lock_request();
            let worker = tokio::spawn({
                let service = service.clone();
                async move { service.execute_operation(add(1)).await }
            });
            pause.wait_started().await;
            advance_timing(STEP);
            pause.release();
            release_tx.send(()).unwrap();
            holder.join().unwrap();
            worker.await.unwrap().unwrap();
            let [queue, lock_wait, execution] = phases(&service, "operation").await;
            assert!(excluded(queue), "{queue:?}");
            assert!(absorbed(lock_wait), "{lock_wait:?}");
            assert!(excluded(execution), "{execution:?}");
        }

        /// One blocking thread: the second job waits in Tokio's blocking
        /// queue until the first, paused before its lock request, finishes.
        #[cfg(feature = "test-support")]
        #[test]
        fn queue_spans_submission_until_the_worker_starts() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let _clock = CLOCK.lock().await;
                let lifecycle = Lifecycle::ready();
                let service = timed_service(lifecycle.clone());
                let mut pause = lifecycle.pause_next_worker();
                let first = tokio::spawn({
                    let service = service.clone();
                    async move { service.stats().await }
                });
                pause.wait_started().await;
                let second = tokio::spawn({
                    let service = service.clone();
                    async move { service.execute_operation(add(1)).await }
                });
                // Single-threaded runtime: once the permit is taken, the
                // second caller has submitted its job and is awaiting it.
                while service.permits.available_permits() != 2 {
                    tokio::task::yield_now().await;
                }
                advance_timing(STEP);
                pause.release();
                first.await.unwrap().unwrap();
                second.await.unwrap().unwrap();
                for phase in phases(&service, "stats").await {
                    assert!(excluded(phase), "{phase:?}");
                }
                let [queue, lock_wait, execution] = phases(&service, "operation").await;
                assert!(absorbed(queue), "{queue:?}");
                assert!(excluded(lock_wait), "{lock_wait:?}");
                assert!(excluded(execution), "{execution:?}");
            });
        }
    }

    /// Lease-remaining and last-applied-age gauges, driven through the
    /// production router. Tests that move the shared lease clock hold its lock
    /// and reset it on entry and on drop, including after a failed assertion.
    mod feeder_activity {
        use super::*;
        use axum::body::{Body, to_bytes};
        use axum::http::{Request, StatusCode, header};
        use serde_json::{Value, json};
        use tower::ServiceExt;

        const LEASE: &str = "knotrel_feeder_lease_remaining_seconds";
        const AGE: &str = "knotrel_feeder_last_applied_age_seconds";
        const STEP: Duration = Duration::from_secs(10);

        struct Clock(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

        impl Clock {
            async fn lock() -> Self {
                let guard = lock_clock_for_test().await;
                reset_clock();
                Self(guard)
            }
        }

        impl Drop for Clock {
            fn drop(&mut self) {
                reset_clock();
            }
        }

        struct App {
            service: Service,
            lifecycle: Lifecycle,
            router: axum::Router,
            instance: String,
        }

        fn app_with(config: crate::ServerConfig) -> App {
            let lifecycle = Lifecycle::ready();
            let service =
                Service::with_config(ConnectivityGraph::default(), 4, lifecycle.clone(), &config);
            let router =
                crate::build_router_from_service(service.clone(), config, lifecycle.clone());
            let instance = service.instance_id().to_owned();
            App {
                service,
                lifecycle,
                router,
                instance,
            }
        }

        fn source_managed() -> crate::ServerConfig {
            crate::ServerConfig::default()
                .with_mode(ServerMode::SourceManaged)
                .with_metrics_enabled(true)
        }

        impl App {
            async fn post(&self, path: &str, if_match: Option<&str>, body: Value) -> StatusCode {
                let mut request = Request::post(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-knotrel-instance-id", &self.instance);
                if let Some(etag) = if_match {
                    request = request.header(header::IF_MATCH, etag);
                }
                self.router
                    .clone()
                    .oneshot(request.body(Body::from(body.to_string())).unwrap())
                    .await
                    .unwrap()
                    .status()
            }

            async fn get(&self, path: &str) -> (StatusCode, String) {
                let response = self
                    .router
                    .clone()
                    .oneshot(Request::get(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                let status = response.status();
                let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
                (status, String::from_utf8(bytes.to_vec()).unwrap())
            }

            /// `(lease remaining, last-applied age)` from one scrape.
            async fn gauges(&self) -> (Option<f64>, Option<f64>) {
                let (status, text) = self.get("/metrics").await;
                assert_eq!(status, StatusCode::OK);
                let sample = |name: &str| {
                    text.lines()
                        .find_map(|line| line.strip_prefix(&format!("{name} ")))
                        .map(|value| value.parse().unwrap())
                };
                (sample(LEASE), sample(AGE))
            }

            async fn age(&self) -> f64 {
                self.gauges().await.1.expect("age series present")
            }

            async fn lease(&self) -> f64 {
                self.gauges().await.0.expect("lease series present")
            }

            async fn session(&self, id: &str, epoch: u64, lease_ms: u64) -> StatusCode {
                let body = json!({
                    "instance_id": self.instance,
                    "feeder_session_id": id,
                    "feeder_epoch": epoch,
                    "lease_duration_ms": lease_ms,
                });
                self.post("/v1/feeder/session", None, body).await
            }

            async fn heartbeat(&self) -> StatusCode {
                let body = json!({
                    "instance_id": self.instance,
                    "feeder_session_id": "s",
                    "feeder_epoch": 1,
                });
                self.post("/v1/feeder/heartbeat", None, body).await
            }

            async fn import(&self, mode: &str, if_match: Option<&str>) -> StatusCode {
                let body = json!({
                    "mode": mode,
                    "instance_id": self.instance,
                    "graph_id": "default",
                    "source_id": "pg-1",
                    "source_epoch": "ep-1",
                    "source_position": "p-0",
                    "feeder_session_id": "s",
                    "feeder_epoch": 1,
                    "nodes": ["1", "2"],
                    "edges": [],
                });
                self.post("/v1/feeder/import", if_match, body).await
            }

            async fn events(&self, events: Value) -> StatusCode {
                let body = json!({
                    "instance_id": self.instance,
                    "graph_id": "default",
                    "generation": 1,
                    "feeder_session_id": "s",
                    "feeder_epoch": 1,
                    "source_id": "pg-1",
                    "source_epoch": "ep-1",
                    "events": events,
                });
                self.post("/v1/feeder/events", None, body).await
            }

            async fn bootstrap(&self) {
                assert_eq!(self.session("s", 1, 60_000).await, StatusCode::OK);
                assert_eq!(self.import("create_only", None).await, StatusCode::CREATED);
            }

            /// Renews the lease, then lets `STEP` pass.
            async fn idle(&self) {
                assert_eq!(self.heartbeat().await, StatusCode::OK);
                advance_clock(STEP);
            }

            async fn wait_idle(&self) {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while self.lifecycle.outstanding_jobs() != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }

            /// Starts a real read-only job that holds the graph lock until
            /// the returned sender is used or dropped.
            async fn hold_graph_lock(
                &self,
            ) -> (
                std::sync::mpsc::Sender<()>,
                tokio::task::JoinHandle<Result<(), ServiceError>>,
            ) {
                let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let holder = tokio::spawn({
                    let service = self.service.clone();
                    async move {
                        service
                            .run(JobKind::Stats, JobEffect::ReadOnly, move |_| {
                                locked_tx.send(()).unwrap();
                                let _ = release_rx.recv();
                            })
                            .await
                    }
                });
                locked_rx.await.unwrap();
                assert!(self.service.graph.try_lock().is_none());
                (release_tx, holder)
            }
        }

        fn event(seq: u64, fields: Value) -> Value {
            let mut event = json!({
                "event_id": format!("e-{seq}"),
                "delivery_sequence": seq,
                "prev_delivery_sequence": seq - 1,
                "source_position": format!("p-{seq}"),
                "prev_source_position": format!("p-{}", seq - 1),
            });
            event
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            event
        }

        fn link(seq: u64, source: &str, target: &str) -> Value {
            event(
                seq,
                json!({"op": "link", "source": source, "target": target}),
            )
        }

        fn reset(age: f64) -> bool {
            age < STEP.as_secs_f64()
        }

        fn kept(age: f64) -> bool {
            age >= STEP.as_secs_f64()
        }

        /// Publishes `deadline` through the production readiness word, as
        /// a feeder worker does under the graph lock, with status `Ready`
        /// and an active session `s`/1.
        fn publish_deadline(service: &Service, deadline: Instant) {
            let mut state = service.graph.lock();
            state.feeder_session_id = Some("s".into());
            state.feeder_epoch = 1;
            state.lease_deadline = Some(deadline);
            state.sync_status = SyncStatus::Ready;
            let word = state.current_readiness_word();
            service.published_readiness.store(word, Ordering::Release);
        }

        /// Outside the deadline millisecond the gauge's sign agrees with
        /// every expiry check: `/ready`, `Service::is_lease_expired` (both
        /// on published whole milliseconds) and the worker's unrounded
        /// `is_lease_expired_internal` and `validate_session_and_lease`.
        #[tokio::test]
        async fn lease_sign_matches_every_expiry_check_outside_the_deadline_millisecond() {
            let _clock = Clock::lock().await;
            let app = app_with(source_managed());

            publish_deadline(&app.service, monotonic_now() + Duration::from_secs(60));
            let lease = app.lease().await;
            assert!(lease > 59.0 && lease <= 60.0, "{lease}");
            assert_eq!(app.get("/ready").await.0, StatusCode::OK);
            assert!(!app.service.is_lease_expired());
            {
                let mut state = app.service.graph.lock();
                assert!(!state.is_lease_expired_internal());
                assert!(state.validate_session_and_lease("s", 1).is_ok());
            }

            publish_deadline(&app.service, monotonic_now());
            // Whole-millisecond steps: `now_millis` is at least
            // `deadline_millis + 2` and the unrounded now is past the deadline.
            advance_clock(Duration::from_millis(2));
            let lease = app.lease().await;
            assert!(lease <= -0.002 && lease > -1.0, "{lease}");
            assert_eq!(app.get("/ready").await.0, StatusCode::SERVICE_UNAVAILABLE);
            assert!(app.service.is_lease_expired());
            let mut state = app.service.graph.lock();
            assert!(state.is_lease_expired_internal());
            assert!(matches!(
                state.validate_session_and_lease("s", 1),
                Err(FeederError::LeaseExpired)
            ));
        }

        /// The gauge reads `0` for the whole deadline millisecond
        /// `[D, D + 1 ms)`, where `D` is the deadline truncated as
        /// `current_readiness_word` publishes it. Published readiness still
        /// treats the lease as live there; worker validation, which compares
        /// unrounded instants (`monotonic_now() > deadline`), already
        /// expires it after the exact deadline inside that millisecond.
        #[test]
        fn lease_reads_zero_throughout_the_deadline_millisecond() {
            use crate::metrics::lease_remaining_seconds;
            let at = |micros: u64| *START_INSTANT + Duration::from_micros(micros);
            let deadline = at(5_000_600);
            let deadline_ms = instant_to_millis(deadline);
            assert_eq!(deadline_ms, 5_000);
            assert_eq!(lease_remaining_seconds(0, 5_000), None);
            // (now, gauge, published expiry, worker expiry)
            let cases = [
                (at(4_999_900), 0.001, false, false),
                (at(5_000_000), 0.0, false, false),
                (at(5_000_200), 0.0, false, false),
                (at(5_000_800), 0.0, false, true),
                (at(5_001_000), -0.001, true, true),
                (at(65_000_000), -60.0, true, true),
            ];
            for (now, gauge, published, worker) in cases {
                let now_ms = instant_to_millis(now);
                assert_eq!(lease_remaining_seconds(deadline_ms, now_ms), Some(gauge));
                assert_eq!(now_ms > deadline_ms, published, "{now_ms}");
                assert_eq!(now > deadline, worker, "{now_ms}");
            }
        }

        #[tokio::test]
        async fn standalone_mode_exposes_neither_gauge() {
            let app = app_with(crate::ServerConfig::default().with_metrics_enabled(true));
            let (_, text) = app.get("/metrics").await;
            assert!(!text.contains(LEASE), "{text}");
            assert!(!text.contains(AGE), "{text}");
        }

        #[tokio::test]
        async fn lease_follows_session_heartbeat_takeover_and_expiry_without_renewal() {
            let _clock = Clock::lock().await;
            let app = app_with(source_managed());
            assert_eq!(app.gauges().await, (None, None));

            assert_eq!(app.session("s", 1, 10_000).await, StatusCode::OK);
            let lease = app.lease().await;
            assert!(lease > 9.0 && lease <= 10.0, "{lease}");
            assert_eq!(app.gauges().await.1, None, "a session applies nothing");

            advance_clock(Duration::from_secs(4));
            let lease = app.lease().await;
            assert!(lease > 5.0 && lease <= 6.0, "{lease}");
            assert_eq!(app.heartbeat().await, StatusCode::OK);
            let lease = app.lease().await;
            assert!(lease > 9.0 && lease <= 10.0, "{lease}");

            assert_eq!(app.session("t", 2, 30_000).await, StatusCode::OK);
            let lease = app.lease().await;
            assert!(lease > 29.0 && lease <= 30.0, "{lease}");
            // A fenced takeover leaves the published deadline alone.
            assert_eq!(app.session("u", 1, 1_000).await, StatusCode::CONFLICT);
            let lease = app.lease().await;
            assert!(lease > 29.0 && lease <= 30.0, "{lease}");

            advance_clock(Duration::from_secs(40));
            let lease = app.lease().await;
            assert!(lease > -11.0 && lease <= -10.0, "{lease}");
            // Scraping neither renews the lease nor publishes staleness.
            let again = app.lease().await;
            assert!(again <= lease, "{again} > {lease}");
        }

        #[tokio::test]
        async fn applied_events_and_publication_reset_age_and_nothing_else_does() {
            let _clock = Clock::lock().await;
            let app = app_with(source_managed());
            assert_eq!(app.session("s", 1, 60_000).await, StatusCode::OK);
            assert_eq!(app.gauges().await.1, None);
            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            assert!(reset(app.age().await));

            app.idle().await;
            assert_eq!(app.events(json!([link(1, "1", "2")])).await, StatusCode::OK);
            assert!(reset(app.age().await), "effective event");

            app.idle().await;
            assert_eq!(app.events(json!([link(1, "1", "2")])).await, StatusCode::OK);
            assert!(kept(app.age().await), "exact duplicate");
            assert_eq!(
                app.events(json!([link(3, "1", "2")])).await,
                StatusCode::CONFLICT
            );
            assert!(kept(app.age().await), "out-of-order event");
            assert_eq!(
                app.events(json!([link(2, "1", "1")])).await,
                StatusCode::UNPROCESSABLE_ENTITY
            );
            assert!(kept(app.age().await), "domain-rejected event");
            let connected = r#"{"op":"connected","source":"1","target":"2"}"#;
            let query = Request::post("/v1/operations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(connected))
                .unwrap();
            let response = app.router.clone().oneshot(query).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(app.get("/v1/stats").await.0, StatusCode::OK);
            assert_eq!(app.session("s", 1, 60_000).await, StatusCode::OK);
            let barrier = json!({
                "instance_id": app.instance,
                "graph_id": "default",
                "generation": 1,
                "feeder_session_id": "s",
                "feeder_epoch": 1,
                "barrier_delivery_sequence": 1,
                "barrier_source_position": "p-1",
            });
            assert_eq!(
                app.post("/v1/feeder/barrier", None, barrier).await,
                StatusCode::OK
            );
            assert!(kept(app.age().await), "query, stats, session, barrier");

            assert_eq!(app.events(json!([link(2, "2", "1")])).await, StatusCode::OK);
            assert!(reset(app.age().await), "no-op mutation");

            app.idle().await;
            let progress = event(3, json!({"op": "progress"}));
            assert_eq!(app.events(json!([progress])).await, StatusCode::OK);
            assert!(reset(app.age().await), "progress event");

            app.idle().await;
            let partial = json!([link(4, "3", "4"), link(5, "5", "5"), link(6, "6", "7")]);
            assert_eq!(app.events(partial).await, StatusCode::UNPROCESSABLE_ENTITY);
            assert!(
                reset(app.age().await),
                "applied prefix of a partial failure"
            );

            app.idle().await;
            let gap = json!({
                "instance_id": app.instance,
                "graph_id": "default",
                "generation": 1,
                "feeder_session_id": "s",
                "feeder_epoch": 1,
                "failed_delivery_sequence": 5,
            });
            assert_eq!(app.post("/v1/feeder/gap", None, gap).await, StatusCode::OK);
            assert!(kept(app.age().await), "declared gap");
        }

        #[tokio::test]
        async fn only_a_published_import_resets_age() {
            let _clock = Clock::lock().await;
            let app = app_with(source_managed());
            app.bootstrap().await;

            app.idle().await;
            assert_eq!(
                app.import("create_only", None).await,
                StatusCode::CONFLICT,
                "already initialized"
            );
            assert_eq!(
                app.import("replace", Some(r#""1.9.p-0""#)).await,
                StatusCode::PRECONDITION_FAILED
            );
            assert!(kept(app.age().await), "rejected imports");
            let before = app.lease().await;

            assert_eq!(
                app.import("replace", Some(r#""1.0.p-0""#)).await,
                StatusCode::CREATED
            );
            assert!(reset(app.age().await), "published replacement");
            let after = app.lease().await;
            assert!(after <= before, "import does not renew the lease");
        }

        #[tokio::test]
        async fn worker_application_after_caller_cancellation_resets_age() {
            let _clock = Clock::lock().await;
            let app = app_with(source_managed());
            app.bootstrap().await;
            app.idle().await;

            let (release, holder) = app.hold_graph_lock().await;
            let caller = tokio::spawn({
                let router = app.router.clone();
                let request = Request::post("/v1/feeder/events")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-knotrel-instance-id", &app.instance)
                    .body(Body::from(
                        json!({
                            "instance_id": app.instance,
                            "graph_id": "default",
                            "generation": 1,
                            "feeder_session_id": "s",
                            "feeder_epoch": 1,
                            "source_id": "pg-1",
                            "source_epoch": "ep-1",
                            "events": [link(1, "1", "2")],
                        })
                        .to_string(),
                    ))
                    .unwrap();
                async move { router.oneshot(request).await }
            });
            // Holder plus the submitted events job.
            while app.lifecycle.outstanding_jobs() != 2 {
                tokio::task::yield_now().await;
            }
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert!(kept(app.age().await), "scrape while the lock is held");

            release.send(()).unwrap();
            holder.await.unwrap().unwrap();
            app.wait_idle().await;
            assert!(reset(app.age().await), "worker applied after cancellation");
        }

        #[tokio::test]
        async fn worker_application_after_caller_timeout_resets_age() {
            let _clock = Clock::lock().await;
            let app = app_with(
                source_managed()
                    .with_request_timeout(Duration::from_millis(50))
                    .unwrap(),
            );
            app.bootstrap().await;
            app.idle().await;

            let (release, holder) = app.hold_graph_lock().await;
            assert_eq!(
                app.events(json!([link(1, "1", "2")])).await,
                StatusCode::SERVICE_UNAVAILABLE
            );
            assert_eq!(app.lifecycle.outstanding_jobs(), 2);
            assert!(kept(app.age().await));

            release.send(()).unwrap();
            holder.await.unwrap().unwrap();
            app.wait_idle().await;
            assert!(reset(app.age().await), "worker applied after timeout");
        }

        #[tokio::test]
        async fn scrape_reads_both_gauges_while_a_worker_holds_the_graph_lock() {
            let app = app_with(source_managed());
            app.bootstrap().await;
            let (release, holder) = app.hold_graph_lock().await;
            let gauges = tokio::time::timeout(Duration::from_secs(5), app.gauges())
                .await
                .expect("scrape must not wait for the graph lock");
            assert!(matches!(gauges, (Some(_), Some(_))), "{gauges:?}");
            assert!(app.service.graph.try_lock().is_none());
            release.send(()).unwrap();
            holder.await.unwrap().unwrap();
        }

        #[tokio::test]
        async fn disabled_telemetry_keeps_feeder_behavior() {
            let app = app_with(crate::ServerConfig::default().with_mode(ServerMode::SourceManaged));
            assert!(app.service.telemetry().is_none());
            app.bootstrap().await;
            assert_eq!(app.events(json!([link(1, "1", "2")])).await, StatusCode::OK);
            assert_eq!(app.get("/metrics").await.0, StatusCode::NOT_FOUND);
        }
    }

    mod import_metrics {
        use super::*;
        use crate::metrics::{ImportResult, advance_timing};
        use axum::body::{Body, Bytes, to_bytes};
        use axum::http::{Request, StatusCode, header};
        use serde_json::{Value, json};
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tower::ServiceExt;

        const IMPORT: &str = "/v1/feeder/import";
        const STEP: Duration = Duration::from_secs(5);
        const PHASES: [&str; 3] = ["receive", "prepare", "publish"];
        const RESULTS: [&str; 5] = [
            "published",
            "payload_rejected",
            "publication_rejected",
            "worker_failed",
            "abandoned",
        ];

        /// Serializes the lease clock (so leases cannot expire mid-test) and
        /// the job-phase clock (so phase sums are not disturbed).
        struct Clocks {
            _lease: tokio::sync::MutexGuard<'static, ()>,
            _timing: tokio::sync::MutexGuard<'static, ()>,
        }

        impl Clocks {
            async fn lock() -> Self {
                let lease = lock_clock_for_test().await;
                reset_clock();
                let timing = super::job_timing::CLOCK.lock().await;
                Self {
                    _lease: lease,
                    _timing: timing,
                }
            }
        }

        impl Drop for Clocks {
            fn drop(&mut self) {
                reset_clock();
            }
        }

        /// Request body fed by the test, holding the handler in body receipt.
        struct ChannelBody(tokio::sync::mpsc::UnboundedReceiver<Bytes>);

        impl hyper::body::Body for ChannelBody {
            type Data = Bytes;
            type Error = std::convert::Infallible;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
                self.0
                    .poll_recv(cx)
                    .map(|chunk| chunk.map(|bytes| Ok(hyper::body::Frame::data(bytes))))
            }
        }

        struct Scrape(String);

        impl Scrape {
            fn sample(&self, series: &str) -> Option<f64> {
                self.0
                    .lines()
                    .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
                    .map(|value| value.parse().unwrap())
            }

            fn in_progress(&self) -> Option<f64> {
                self.sample("knotrel_import_in_progress")
            }

            fn results(&self) -> [u64; 5] {
                RESULTS.map(|result| {
                    let series = format!("knotrel_imports_total{{result=\"{result}\"}}");
                    self.sample(&series)
                        .unwrap_or_else(|| panic!("missing {series} in {}", self.0))
                        as u64
                })
            }

            /// `(count, sum)` of one import-duration phase.
            fn phase(&self, phase: &str) -> (u64, f64) {
                let read = |suffix: &str| {
                    let series =
                        format!("knotrel_import_duration_seconds_{suffix}{{phase=\"{phase}\"}}");
                    self.sample(&series)
                        .unwrap_or_else(|| panic!("missing {series} in {}", self.0))
                };
                (read("count") as u64, read("sum"))
            }

            fn counts(&self) -> [u64; 3] {
                PHASES.map(|phase| self.phase(phase).0)
            }
        }

        fn absorbed((count, sum): (u64, f64)) -> bool {
            count == 1 && sum >= STEP.as_secs_f64()
        }

        fn excluded((count, sum): (u64, f64)) -> bool {
            count == 1 && sum < STEP.as_secs_f64()
        }

        struct App {
            service: Service,
            router: axum::Router,
            instance: String,
        }

        fn app_with(config: crate::ServerConfig) -> App {
            let lifecycle = Lifecycle::ready();
            let service =
                Service::with_config(ConnectivityGraph::default(), 4, lifecycle.clone(), &config);
            let router =
                crate::build_router_from_service(service.clone(), config, lifecycle.clone());
            let instance = service.instance_id().to_owned();
            App {
                service,
                router,
                instance,
            }
        }

        fn source_managed() -> crate::ServerConfig {
            crate::ServerConfig::default()
                .with_mode(ServerMode::SourceManaged)
                .with_metrics_enabled(true)
        }

        impl App {
            fn request(
                &self,
                path: &str,
                if_match: Option<&str>,
                body: Body,
            ) -> impl Future<Output = StatusCode> + Send + 'static {
                let mut request = Request::post(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-knotrel-instance-id", &self.instance);
                if let Some(etag) = if_match {
                    request = request.header(header::IF_MATCH, etag);
                }
                let request = request.body(body).unwrap();
                let router = self.router.clone();
                async move { router.oneshot(request).await.unwrap().status() }
            }

            fn import_body(&self, mode: &str) -> Value {
                json!({
                    "mode": mode,
                    "instance_id": self.instance,
                    "graph_id": "default",
                    "source_id": "pg-1",
                    "source_epoch": "ep-1",
                    "source_position": "p-0",
                    "feeder_session_id": "s",
                    "feeder_epoch": 1,
                    "nodes": ["1", "2"],
                    "edges": [],
                })
            }

            async fn send(&self, if_match: Option<&str>, body: Value) -> StatusCode {
                self.request(IMPORT, if_match, Body::from(body.to_string()))
                    .await
            }

            async fn import(&self, mode: &str, if_match: Option<&str>) -> StatusCode {
                self.send(if_match, self.import_body(mode)).await
            }

            /// Starts an import whose body the test feeds through the sender.
            fn spawn_streamed_import(
                &self,
            ) -> (
                tokio::sync::mpsc::UnboundedSender<Bytes>,
                tokio::task::JoinHandle<StatusCode>,
            ) {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let body = Body::new(ChannelBody(rx));
                (tx, tokio::spawn(self.request(IMPORT, None, body)))
            }

            async fn session(&self) {
                let body = json!({
                    "instance_id": self.instance,
                    "feeder_session_id": "s",
                    "feeder_epoch": 1,
                    "lease_duration_ms": 60_000,
                });
                let status = self
                    .request("/v1/feeder/session", None, Body::from(body.to_string()))
                    .await;
                assert_eq!(status, StatusCode::OK);
            }

            async fn scrape(&self) -> Scrape {
                let response = self
                    .router
                    .clone()
                    .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
                Scrape(String::from_utf8(bytes.to_vec()).unwrap())
            }

            async fn wait_for_in_progress(&self, value: f64) {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while self.scrape().await.in_progress() != Some(value) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("import_in_progress never reached {value}"));
            }
        }

        #[tokio::test]
        async fn published_imports_count_once_and_time_every_phase() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            let before = app.scrape().await;
            assert_eq!(before.in_progress(), Some(0.0));
            assert_eq!(before.results(), [0; 5]);
            assert_eq!(before.counts(), [0; 3]);

            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            assert_eq!(
                app.import("replace", Some(r#""1.0.p-0""#)).await,
                StatusCode::CREATED
            );
            let after = app.scrape().await;
            assert_eq!(after.in_progress(), Some(0.0));
            assert_eq!(after.results(), [2, 0, 0, 0, 0]);
            assert_eq!(after.counts(), [2, 2, 2]);
        }

        #[tokio::test]
        async fn payload_rejections_end_in_prepare_without_publish() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;

            let malformed = app.request(IMPORT, None, Body::from("{\"mode\":")).await;
            assert_eq!(malformed, StatusCode::BAD_REQUEST);
            let mut self_loop = app.import_body("create_only");
            self_loop["edges"] = json!([{"source": "1", "target": "1"}]);
            assert_eq!(
                app.send(None, self_loop).await,
                StatusCode::UNPROCESSABLE_ENTITY
            );
            let mut stale = app.import_body("create_only");
            stale["instance_id"] = json!("other-instance");
            assert!(app.send(None, stale).await.is_client_error());

            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [0, 3, 0, 0, 0]);
            assert_eq!(scrape.counts(), [3, 3, 0]);
        }

        #[tokio::test]
        async fn failed_body_read_is_a_payload_rejection_without_prepare() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed().with_max_import_body_bytes(16).unwrap());
            app.session().await;
            assert_eq!(
                app.import("create_only", None).await,
                StatusCode::PAYLOAD_TOO_LARGE
            );
            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [0, 1, 0, 0, 0]);
            assert_eq!(scrape.counts(), [1, 0, 0]);
        }

        #[tokio::test]
        async fn publication_rejections_are_timed_through_publish() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);

            assert_eq!(app.import("create_only", None).await, StatusCode::CONFLICT);
            assert_eq!(
                app.import("replace", Some(r#""1.9.p-0""#)).await,
                StatusCode::PRECONDITION_FAILED
            );
            let mut fenced = app.import_body("replace");
            fenced["feeder_session_id"] = json!("other-session");
            assert!(
                app.send(Some(r#""1.0.p-0""#), fenced)
                    .await
                    .is_client_error()
            );

            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [1, 0, 3, 0, 0]);
            assert_eq!(scrape.counts(), [4, 4, 4]);
        }

        #[tokio::test]
        async fn capacity_refusal_is_not_an_import() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            let held = app.service.acquire_import_permit().unwrap();
            assert_eq!(app.scrape().await.in_progress(), Some(1.0));

            assert_eq!(
                app.import("create_only", None).await,
                StatusCode::SERVICE_UNAVAILABLE
            );
            let telemetry = app.service.telemetry().unwrap();
            assert_eq!(telemetry.rejected_count(Rejection::ImportCapacity), 1);
            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(1.0));
            assert_eq!(scrape.results(), [0; 5]);

            drop(held);
            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [0, 0, 0, 0, 1], "a permit never used");
            assert_eq!(scrape.counts(), [0; 3]);
        }

        #[tokio::test]
        async fn receive_spans_the_body_read() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            let (body, caller) = app.spawn_streamed_import();
            app.wait_for_in_progress(1.0).await;

            advance_timing(STEP);
            body.send(Bytes::from(app.import_body("create_only").to_string()))
                .unwrap();
            drop(body);
            assert_eq!(caller.await.unwrap(), StatusCode::CREATED);

            let scrape = app.scrape().await;
            assert_eq!(scrape.results(), [1, 0, 0, 0, 0]);
            assert!(absorbed(scrape.phase("receive")), "{}", scrape.0);
            assert!(excluded(scrape.phase("prepare")), "{}", scrape.0);
            assert!(excluded(scrape.phase("publish")), "{}", scrape.0);
        }

        #[tokio::test]
        async fn abandoned_body_receipt_is_abandoned_without_phases() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            let (body, caller) = app.spawn_streamed_import();
            app.wait_for_in_progress(1.0).await;
            body.send(Bytes::from_static(b"{\"mode\":")).unwrap();

            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [0, 0, 0, 0, 1]);
            assert_eq!(scrape.counts(), [0; 3], "an unfinished receive");

            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            assert_eq!(app.scrape().await.results(), [1, 0, 0, 0, 1]);
        }

        /// Tests that pause the worker through the test-support hooks.
        #[cfg(feature = "test-support")]
        mod paused {
            use super::*;

            impl App {
                fn spawn_import(&self) -> tokio::task::JoinHandle<StatusCode> {
                    let body = Body::from(self.import_body("create_only").to_string());
                    tokio::spawn(self.request(IMPORT, None, body))
                }
            }

            #[tokio::test]
            async fn in_progress_covers_a_paused_worker_whose_start_hook_is_untimed() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_worker();
                let caller = app.spawn_import();
                pause.wait_started().await;
                assert_eq!(app.scrape().await.in_progress(), Some(1.0));
                assert_eq!(app.scrape().await.results(), [0; 5]);

                advance_timing(STEP);
                pause.release();
                assert_eq!(caller.await.unwrap(), StatusCode::CREATED);
                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(0.0));
                assert_eq!(scrape.results(), [1, 0, 0, 0, 0]);
                for phase in PHASES {
                    assert!(excluded(scrape.phase(phase)), "{phase}: {}", scrape.0);
                }
            }

            #[tokio::test]
            async fn publish_starts_at_the_graph_lock_request() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_lock_request();
                let caller = app.spawn_import();
                pause.wait_started().await;

                advance_timing(STEP);
                pause.release();
                assert_eq!(caller.await.unwrap(), StatusCode::CREATED);
                let scrape = app.scrape().await;
                assert!(excluded(scrape.phase("receive")), "{}", scrape.0);
                assert!(excluded(scrape.phase("prepare")), "{}", scrape.0);
                assert!(absorbed(scrape.phase("publish")), "{}", scrape.0);
            }

            #[tokio::test]
            async fn worker_panic_at_start_is_worker_failed_without_worker_phases() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_worker();
                let caller = app.spawn_import();
                pause.wait_started().await;
                pause.panic_on_release();
                assert_eq!(caller.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);

                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(0.0));
                assert_eq!(scrape.results(), [0, 0, 0, 1, 0]);
                assert_eq!(scrape.counts(), [1, 0, 0]);
            }

            #[tokio::test]
            async fn worker_panic_at_the_lock_request_records_prepare_only() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_lock_request();
                let caller = app.spawn_import();
                pause.wait_started().await;
                pause.panic_on_release();
                assert_eq!(caller.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);

                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(0.0));
                assert_eq!(scrape.results(), [0, 0, 0, 1, 0]);
                assert_eq!(scrape.counts(), [1, 1, 0], "an unwound publish");
            }

            #[tokio::test]
            async fn publication_after_caller_cancellation_counts_as_published() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_lock_request();
                let caller = app.spawn_import();
                pause.wait_started().await;

                caller.abort();
                assert!(caller.await.unwrap_err().is_cancelled());
                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(1.0), "the worker owns it");
                assert_eq!(scrape.results(), [0; 5]);

                pause.release();
                app.wait_for_in_progress(0.0).await;
                let scrape = app.scrape().await;
                assert_eq!(scrape.results(), [1, 0, 0, 0, 0]);
                assert_eq!(scrape.counts(), [1, 1, 1]);
            }

            #[tokio::test]
            async fn publication_after_caller_timeout_counts_as_published() {
                let _clocks = Clocks::lock().await;
                let app = app_with(
                    source_managed()
                        .with_request_timeout(Duration::from_millis(50))
                        .unwrap(),
                );
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_lock_request();
                assert_eq!(
                    app.import("create_only", None).await,
                    StatusCode::SERVICE_UNAVAILABLE
                );
                pause.wait_started().await;
                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(1.0));
                assert_eq!(scrape.results(), [0; 5]);

                pause.release();
                app.wait_for_in_progress(0.0).await;
                assert_eq!(app.scrape().await.results(), [1, 0, 0, 0, 0]);
            }

            #[tokio::test]
            async fn worker_panic_after_publication_stays_published() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let mut pause = app.service.lifecycle.pause_next_worker_after_lock();
                let caller = app.spawn_import();
                pause.wait_started().await;
                pause.panic_on_release();
                assert_eq!(caller.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);

                let scrape = app.scrape().await;
                assert_eq!(scrape.in_progress(), Some(0.0));
                assert_eq!(scrape.results(), [1, 0, 0, 0, 0]);
                assert_eq!(scrape.counts(), [1, 1, 0], "an unwound publish");
            }

            /// The job count falls only after the import is recorded and its
            /// permit released, so a drain cannot finish before them.
            #[tokio::test]
            async fn import_is_recorded_before_its_job_ends() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let telemetry = app.service.telemetry().unwrap().clone();
                let lifecycle = app.service.lifecycle.clone();
                let mut pause = lifecycle.pause_next_lock_request();
                let caller = app.spawn_import();
                pause.wait_started().await;

                // Holding job releases blocks the worker's job-guard update.
                let held = lifecycle.hold_job_releases();
                pause.release();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let recorded = loop {
                    let done = telemetry.import_in_progress() == 0
                        && telemetry.import_result_count(ImportResult::Published) == 1
                        && app.service.import_permits.available_permits() == 1;
                    if done || std::time::Instant::now() >= deadline {
                        break done;
                    }
                    std::thread::yield_now();
                };
                let outstanding = lifecycle.outstanding_jobs();
                drop(held);
                assert!(recorded, "import not recorded while its job was counted");
                assert_eq!(outstanding, 1);
                assert_eq!(caller.await.unwrap(), StatusCode::CREATED);
                assert_eq!(lifecycle.outstanding_jobs(), 0);
            }
        }

        #[tokio::test]
        async fn standalone_mode_has_no_import_accounting() {
            let app = app_with(crate::ServerConfig::default().with_metrics_enabled(true));
            assert!(!app.scrape().await.0.contains("knotrel_import"));
            let permit = app.service.acquire_import_permit().unwrap();
            assert!(permit.observation.is_none());
        }

        #[tokio::test]
        async fn disabled_telemetry_keeps_import_behavior() {
            let _clocks = Clocks::lock().await;
            let app = app_with(crate::ServerConfig::default().with_mode(ServerMode::SourceManaged));
            assert!(app.service.telemetry().is_none());
            assert!(
                app.service
                    .acquire_import_permit()
                    .unwrap()
                    .observation
                    .is_none()
            );
            app.session().await;
            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            let response = app
                .router
                .clone()
                .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn every_permitted_import_has_exactly_one_result() {
            let _clocks = Clocks::lock().await;
            let app = app_with(source_managed());
            app.session().await;
            assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            assert_eq!(app.import("create_only", None).await, StatusCode::CONFLICT);
            let malformed = app.request(IMPORT, None, Body::from("{")).await;
            assert_eq!(malformed, StatusCode::BAD_REQUEST);
            let (body, caller) = app.spawn_streamed_import();
            app.wait_for_in_progress(1.0).await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            drop(body);
            let held = app.service.acquire_import_permit().unwrap();
            assert_eq!(
                app.import("create_only", None).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "refused before the permit"
            );
            drop(held);

            let scrape = app.scrape().await;
            assert_eq!(scrape.in_progress(), Some(0.0));
            assert_eq!(scrape.results(), [1, 1, 1, 0, 2]);
            assert_eq!(scrape.results().iter().sum::<u64>(), 5);
        }

        /// A worker closure dropped without running, here because the
        /// blocking pool has shut down, leaves its import abandoned.
        #[test]
        fn queued_import_dropped_before_running_is_abandoned() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (_clocks, app, mut permit) = runtime.block_on(async {
                let clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                app.session().await;
                let permit = app.service.acquire_import_permit().unwrap();
                (clocks, app, permit)
            });
            permit.received(true);
            let handle = runtime.handle().clone();
            runtime.shutdown_background();

            let body = Bytes::from(app.import_body("create_only").to_string());
            let result = handle.block_on(app.service.import(permit, body, None));
            assert!(
                matches!(result, Err(FeederError::Unavailable)),
                "{result:?}"
            );

            let telemetry = app.service.telemetry().unwrap();
            assert_eq!(telemetry.import_in_progress(), 0);
            assert_eq!(telemetry.import_result_count(ImportResult::Abandoned), 1);
            assert_eq!(telemetry.import_result_count(ImportResult::WorkerFailed), 0);
            assert_eq!(app.service.import_permits.available_permits(), 1);
            assert_eq!(app.service.lifecycle.outstanding_jobs(), 0);
        }

        /// Catch-up duration: `knotrel_catchup_duration_seconds`.
        mod catchup {
            use super::*;

            const KINDS: [&str; 2] = ["initial", "replacement"];

            impl Scrape {
                /// `(count, sum)` of one catch-up kind.
                fn catchup(&self, kind: &str) -> (u64, f64) {
                    let read = |suffix: &str| {
                        let series =
                            format!("knotrel_catchup_duration_seconds_{suffix}{{kind=\"{kind}\"}}");
                        self.sample(&series)
                            .unwrap_or_else(|| panic!("missing {series} in {}", self.0))
                    };
                    (read("count") as u64, read("sum"))
                }

                fn catchup_counts(&self) -> [u64; 2] {
                    KINDS.map(|kind| self.catchup(kind).0)
                }

                fn catchup_series(&self) -> usize {
                    self.0
                        .lines()
                        .filter(|line| line.starts_with("knotrel_catchup_duration_seconds_"))
                        .count()
                }
            }

            /// An elapsed time of `steps` timing steps plus negligible real time.
            fn steps(sum: f64, steps: u32) -> bool {
                let expected = STEP.as_secs_f64() * f64::from(steps);
                sum >= expected && sum < expected + 1.0
            }

            impl App {
                async fn post_json(&self, path: &str, body: Value) -> (StatusCode, Value) {
                    let request = Request::post(path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-knotrel-instance-id", &self.instance)
                        .body(Body::from(body.to_string()))
                        .unwrap();
                    let response = self.router.clone().oneshot(request).await.unwrap();
                    let status = response.status();
                    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
                    (
                        status,
                        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                    )
                }

                async fn session_epoch(&self, epoch: u64) -> StatusCode {
                    let body = json!({
                        "instance_id": self.instance,
                        "feeder_session_id": "s",
                        "feeder_epoch": epoch,
                        "lease_duration_ms": 60_000,
                    });
                    self.post_json("/v1/feeder/session", body).await.0
                }

                async fn heartbeat(&self) -> StatusCode {
                    let body = json!({
                        "instance_id": self.instance,
                        "feeder_session_id": "s",
                        "feeder_epoch": 1,
                    });
                    self.post_json("/v1/feeder/heartbeat", body).await.0
                }

                /// A barrier at `sequence` and position `p-<sequence>`.
                async fn barrier(
                    &self,
                    generation: u64,
                    epoch: u64,
                    sequence: u64,
                ) -> (StatusCode, Value) {
                    let body = json!({
                        "instance_id": self.instance,
                        "graph_id": "default",
                        "generation": generation,
                        "feeder_session_id": "s",
                        "feeder_epoch": epoch,
                        "barrier_delivery_sequence": sequence,
                        "barrier_source_position": format!("p-{sequence}"),
                    });
                    self.post_json("/v1/feeder/barrier", body).await
                }

                /// Links nodes 1 and 2 as delivery sequence 1.
                async fn link_event(&self, generation: u64) -> StatusCode {
                    let body = json!({
                        "instance_id": self.instance,
                        "graph_id": "default",
                        "generation": generation,
                        "feeder_session_id": "s",
                        "feeder_epoch": 1,
                        "source_id": "pg-1",
                        "source_epoch": "ep-1",
                        "events": [{
                            "event_id": "e-1",
                            "delivery_sequence": 1,
                            "prev_delivery_sequence": 0,
                            "source_position": "p-1",
                            "prev_source_position": "p-0",
                            "op": "link",
                            "source": "1",
                            "target": "2",
                        }],
                    });
                    self.post_json("/v1/feeder/events", body).await.0
                }

                async fn gap(&self, generation: u64) -> StatusCode {
                    let body = json!({
                        "instance_id": self.instance,
                        "graph_id": "default",
                        "generation": generation,
                        "feeder_session_id": "s",
                        "feeder_epoch": 1,
                        "failed_delivery_sequence": 1,
                    });
                    self.post_json("/v1/feeder/gap", body).await.0
                }

                #[cfg(feature = "test-support")]
                async fn wait_for_no_jobs(&self) {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while self.service.lifecycle.outstanding_jobs() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("worker never finished");
                }
            }

            async fn started(app: &App) {
                app.session().await;
                assert_eq!(app.import("create_only", None).await, StatusCode::CREATED);
            }

            #[tokio::test]
            async fn series_exist_at_zero_with_the_calculated_count() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [0, 0]);
                assert_eq!(scrape.catchup("initial").1, 0.0);
                // 2 kinds x (13 buckets + `+Inf` + `_sum` + `_count`).
                assert_eq!(scrape.catchup_series(), 2 * (13 + 1 + 2), "{}", scrape.0);
                assert!(
                    scrape
                        .0
                        .contains("# TYPE knotrel_catchup_duration_seconds histogram")
                );
            }

            #[tokio::test]
            async fn initial_import_to_the_first_barrier_is_observed_once() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;

                // Nothing completes the interval before a successful barrier:
                // an event, a heartbeat, the passage of time and every
                // rejected barrier leave it open.
                assert_eq!(app.link_event(1).await, StatusCode::OK);
                advance_timing(STEP);
                assert_eq!(app.heartbeat().await, StatusCode::OK);
                let (status, body) = app.barrier(1, 1, 5).await;
                assert_eq!(status, StatusCode::CONFLICT, "{body}");
                assert_eq!(body["code"], "barrier_not_reached", "{body}");
                let (status, _) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::CONFLICT, "older sequence");
                let (status, _) = app.barrier(7, 1, 1).await;
                assert_eq!(status, StatusCode::CONFLICT, "stale generation");
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                advance_timing(STEP);
                let (status, body) = app.barrier(1, 1, 1).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                assert_eq!(body["result"], "synchronized");
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [1, 0]);
                let (count, sum) = scrape.catchup("initial");
                assert_eq!(count, 1);
                assert!(steps(sum, 2), "{sum}");

                // A repeated barrier is `already_synchronized` and adds nothing.
                advance_timing(STEP);
                let (status, body) = app.barrier(1, 1, 1).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body["result"], "already_synchronized");
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [1, 0]);
                assert!(steps(scrape.catchup("initial").1, 2));
            }

            #[tokio::test]
            async fn replacement_starts_a_new_interval_after_a_completed_one() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                advance_timing(STEP);
                assert_eq!(app.barrier(1, 1, 0).await.0, StatusCode::OK);
                assert_eq!(app.scrape().await.catchup_counts(), [1, 0]);

                advance_timing(STEP * 4);
                assert_eq!(
                    app.import("replace", Some(r#""1.0.p-0""#)).await,
                    StatusCode::CREATED
                );
                assert_eq!(app.scrape().await.catchup_counts(), [1, 0], "still open");
                advance_timing(STEP * 2);
                let (status, body) = app.barrier(2, 1, 0).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                assert_eq!(body["result"], "synchronized");

                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [1, 1]);
                assert!(steps(scrape.catchup("initial").1, 1));
                // Measured from the replacement, not from the first import.
                assert!(steps(scrape.catchup("replacement").1, 2));
            }

            #[tokio::test]
            async fn a_replaced_interval_is_not_observed_and_not_completed_late() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                advance_timing(STEP);
                assert_eq!(
                    app.import("replace", Some(r#""1.0.p-0""#)).await,
                    StatusCode::CREATED
                );
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                // The obsolete generation cannot complete: its barrier is
                // fenced by the generation check.
                let (status, _) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::CONFLICT);
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                advance_timing(STEP * 2);
                assert_eq!(app.barrier(2, 1, 0).await.0, StatusCode::OK);
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [0, 1]);
                assert!(steps(scrape.catchup("replacement").1, 2));
            }

            #[tokio::test]
            async fn a_desynchronized_interval_is_abandoned_until_rebuilt() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                advance_timing(STEP);
                assert_eq!(app.gap(1).await, StatusCode::OK);
                let (status, body) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::CONFLICT, "{body}");
                assert_eq!(body["code"], "graph_desynchronized", "{body}");
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                advance_timing(STEP);
                assert_eq!(
                    app.import("replace", Some(r#""1.0.p-0""#)).await,
                    StatusCode::CREATED
                );
                advance_timing(STEP);
                assert_eq!(app.barrier(2, 1, 0).await.0, StatusCode::OK);
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [0, 1]);
                assert!(steps(scrape.catchup("replacement").1, 1));
            }

            #[tokio::test]
            async fn lease_expiry_and_takeover_do_not_interrupt_the_interval() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                advance_timing(STEP);
                advance_clock(Duration::from_secs(61));

                // The expired lease rejects the barrier and the old epoch stays fenced.
                let (status, body) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::CONFLICT, "{body}");
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                assert_eq!(app.session_epoch(2).await, StatusCode::OK);
                let (status, _) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::CONFLICT, "fenced epoch");
                assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                advance_timing(STEP);
                let (status, body) = app.barrier(1, 2, 0).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                assert_eq!(body["result"], "synchronized");
                let scrape = app.scrape().await;
                assert_eq!(scrape.catchup_counts(), [1, 0]);
                // The interval includes the time the generation was stale.
                assert!(steps(scrape.catchup("initial").1, 2));
            }

            #[tokio::test]
            async fn recovery_from_stale_after_completion_is_not_a_catchup() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                assert_eq!(app.barrier(1, 1, 0).await.0, StatusCode::OK);
                assert_eq!(app.scrape().await.catchup_counts(), [1, 0]);

                advance_clock(Duration::from_secs(61));
                assert_eq!(app.barrier(1, 1, 0).await.0, StatusCode::CONFLICT);
                assert_eq!(app.session_epoch(2).await, StatusCode::OK);
                let (status, body) = app.barrier(1, 2, 0).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                assert_eq!(body["result"], "synchronized", "recovered from stale");
                assert_eq!(app.scrape().await.catchup_counts(), [1, 0]);
            }

            #[tokio::test]
            async fn standalone_mode_has_no_catchup_series() {
                let app = app_with(crate::ServerConfig::default().with_metrics_enabled(true));
                assert!(!app.scrape().await.0.contains("knotrel_catchup"));
            }

            #[tokio::test]
            async fn disabled_telemetry_tracks_no_interval_and_keeps_behavior() {
                let _clocks = Clocks::lock().await;
                let app =
                    app_with(crate::ServerConfig::default().with_mode(ServerMode::SourceManaged));
                assert!(app.service.telemetry().is_none());
                started(&app).await;
                {
                    let state = app.service.graph.lock();
                    assert!(state.catchup.is_none(), "no clock read, no interval");
                }
                let (status, body) = app.barrier(1, 1, 0).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                assert_eq!(body["result"], "synchronized");
                let state = app.service.graph.lock();
                assert!(state.catchup.is_none() && state.completed_catchup.is_none());
            }

            #[tokio::test]
            async fn instances_count_independently() {
                let _clocks = Clocks::lock().await;
                let first = app_with(source_managed());
                let second = app_with(source_managed());
                started(&first).await;
                started(&second).await;
                advance_timing(STEP);
                assert_eq!(first.barrier(1, 1, 0).await.0, StatusCode::OK);
                assert_eq!(first.scrape().await.catchup_counts(), [1, 0]);
                assert_eq!(second.scrape().await.catchup_counts(), [0, 0]);
                assert_eq!(second.barrier(1, 1, 0).await.0, StatusCode::OK);
                assert_eq!(second.scrape().await.catchup_counts(), [1, 0]);
                assert_eq!(first.scrape().await.catchup_counts(), [1, 0]);
            }

            #[tokio::test]
            async fn scrape_reads_catchup_while_the_graph_lock_is_held() {
                let _clocks = Clocks::lock().await;
                let app = app_with(source_managed());
                started(&app).await;
                let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let holder = tokio::spawn({
                    let service = app.service.clone();
                    async move {
                        service
                            .run(JobKind::Stats, JobEffect::ReadOnly, move |_| {
                                locked_tx.send(()).unwrap();
                                let _ = release_rx.recv();
                            })
                            .await
                    }
                });
                locked_rx.await.unwrap();
                assert!(app.service.graph.try_lock().is_none());
                let scrape = tokio::time::timeout(Duration::from_secs(5), app.scrape())
                    .await
                    .expect("scrape must not wait for the graph lock");
                assert_eq!(scrape.catchup_counts(), [0, 0]);
                release_tx.send(()).unwrap();
                holder.await.unwrap().unwrap();
            }

            /// Tests that pause the barrier worker through the test-support hooks.
            #[cfg(feature = "test-support")]
            mod paused {
                use super::*;

                /// Releases a paused worker even when an assertion fails.
                struct Release(Option<crate::WorkerPauseHandle>);

                impl Release {
                    fn now(&mut self) {
                        if let Some(handle) = self.0.take() {
                            handle.release();
                        }
                    }
                }

                impl Drop for Release {
                    fn drop(&mut self) {
                        self.now();
                    }
                }

                #[tokio::test]
                async fn caller_cancellation_leaves_the_completion_to_the_worker() {
                    let _clocks = Clocks::lock().await;
                    let app = app_with(source_managed());
                    started(&app).await;
                    let mut pause = app.service.lifecycle.pause_next_worker();
                    let caller = tokio::spawn({
                        let router = app.router.clone();
                        let instance = app.instance.clone();
                        let body = json!({
                            "instance_id": app.instance,
                            "graph_id": "default",
                            "generation": 1,
                            "feeder_session_id": "s",
                            "feeder_epoch": 1,
                            "barrier_delivery_sequence": 0,
                            "barrier_source_position": "p-0",
                        });
                        async move {
                            let request = Request::post("/v1/feeder/barrier")
                                .header(header::CONTENT_TYPE, "application/json")
                                .header("x-knotrel-instance-id", instance)
                                .body(Body::from(body.to_string()))
                                .unwrap();
                            router.oneshot(request).await.unwrap().status()
                        }
                    });
                    pause.wait_started().await;
                    let mut release = Release(Some(pause));

                    caller.abort();
                    assert!(caller.await.unwrap_err().is_cancelled());
                    advance_timing(STEP);
                    assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                    release.now();
                    app.wait_for_no_jobs().await;
                    let scrape = app.scrape().await;
                    assert_eq!(scrape.catchup_counts(), [1, 0]);
                    assert!(steps(scrape.catchup("initial").1, 1));
                }

                #[tokio::test]
                async fn caller_timeout_leaves_the_completion_to_the_worker() {
                    let _clocks = Clocks::lock().await;
                    let app = app_with(
                        source_managed()
                            .with_request_timeout(Duration::from_millis(50))
                            .unwrap(),
                    );
                    started(&app).await;
                    let mut pause = app.service.lifecycle.pause_next_worker();
                    let (status, _) = app.barrier(1, 1, 0).await;
                    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                    pause.wait_started().await;
                    let mut release = Release(Some(pause));
                    advance_timing(STEP);
                    assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);

                    release.now();
                    app.wait_for_no_jobs().await;
                    let scrape = app.scrape().await;
                    assert_eq!(scrape.catchup_counts(), [1, 0]);
                    assert!(steps(scrape.catchup("initial").1, 1));
                }

                #[tokio::test]
                async fn a_worker_panic_before_the_barrier_observes_nothing() {
                    let _clocks = Clocks::lock().await;
                    let app = app_with(source_managed());
                    started(&app).await;
                    let mut pause = app.service.lifecycle.pause_next_worker();
                    let barrier = app.barrier(1, 1, 0);
                    let (status, _) = tokio::join!(barrier, async {
                        pause.wait_started().await;
                        pause.panic_on_release();
                    })
                    .0;
                    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                    assert_eq!(app.scrape().await.catchup_counts(), [0, 0]);
                }

                #[tokio::test]
                async fn a_panic_after_the_barrier_keeps_the_completed_interval() {
                    let _clocks = Clocks::lock().await;
                    let app = app_with(source_managed());
                    started(&app).await;
                    advance_timing(STEP);
                    let mut pause = app.service.lifecycle.pause_next_worker_after_lock();
                    let barrier = app.barrier(1, 1, 0);
                    let (status, _) = tokio::join!(barrier, async {
                        pause.wait_started().await;
                        pause.panic_on_release();
                    })
                    .0;
                    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                    let scrape = app.scrape().await;
                    assert_eq!(scrape.catchup_counts(), [1, 0]);
                    assert!(steps(scrape.catchup("initial").1, 1));
                }
            }
        }
    }
}
