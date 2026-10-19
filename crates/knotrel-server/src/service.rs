//! Bounded CPU work and exclusive graph ownership for each request.

use crate::{
    Lifecycle, ServerMode,
    api::{
        HeartbeatRequest, HeartbeatResponse, ImportMode, ImportRequest, ImportResponse, Operation,
        Outcome, SessionRequest, SessionResponse,
    },
};
use knotrel_core::{ConnectivityGraph, EngineConfig, GraphError};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

static START_INSTANT: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

fn instant_to_millis(instant: Instant) -> u64 {
    instant.duration_since(*START_INSTANT).as_millis() as u64
}

fn now_to_millis() -> u64 {
    instant_to_millis(Instant::now())
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
    fn to_code(self) -> u8 {
        match self {
            Self::Uninitialized => 0,
            Self::Loading => 1,
            Self::CatchingUp => 2,
            Self::Ready => 3,
            Self::Stale => 4,
            Self::Desynchronized => 5,
        }
    }

    fn from_code(code: u8) -> Self {
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

/// Bounded deduplication window tracking delivery sequence and event IDs.
#[derive(Debug)]
pub(crate) struct DedupWindow {
    _capacity: usize,
    base_sequence: u64,
    entries: Vec<(u64, String, u64)>,
}

impl DedupWindow {
    pub(crate) const DEFAULT_CAPACITY: usize = 10_000;

    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            _capacity: capacity,
            base_sequence: 1,
            entries: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.base_sequence = 1;
        self.entries.clear();
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

pub(crate) fn percent_encode_position(pos: &str) -> String {
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
    PreconditionFailed(String),
    InvalidLeaseDuration,
    MalformedJson,
    UnprocessableJson(String),
    PayloadTooLarge,
    Domain(&'static str, String),
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
    sync_status: Arc<AtomicU8>,
    lease_deadline_millis: Arc<AtomicU64>,
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
    dedup_window: DedupWindow,
}

impl GraphState {
    fn new(graph: ConnectivityGraph) -> Self {
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
            dedup_window: DedupWindow::new(DedupWindow::DEFAULT_CAPACITY),
        }
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

    fn snapshot(&self, mode: ServerMode, instance_id: &str) -> GraphSnapshot {
        let node_count = self.graph.node_count();
        let edge_count = self.graph.edge_count();
        let limits = self.graph.limits();
        let state_version = self
            .version
            .iter()
            .rev()
            .map(|digit| char::from(b'0' + digit))
            .collect();
        let (instance_id, generation, source_position, sync_status) = match mode {
            ServerMode::Standalone => (None, None, None, None),
            ServerMode::SourceManaged => (
                Some(instance_id.to_string()),
                Some(self.generation),
                self.source_position.clone(),
                Some(self.sync_status),
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

    fn register_session(
        &mut self,
        req: SessionRequest,
        instance_id: String,
    ) -> Result<SessionResponse, FeederError> {
        let lease_duration = Duration::from_millis(req.lease_duration_ms);
        let now = Instant::now();

        if let Some(ref active_session) = self.feeder_session_id {
            if active_session == &req.feeder_session_id {
                if req.feeder_epoch < self.feeder_epoch {
                    return Err(FeederError::FencedSession);
                }
                self.feeder_epoch = req.feeder_epoch;
                self.lease_duration = lease_duration;
                self.lease_deadline = Some(now + lease_duration);
            } else {
                if req.feeder_epoch <= self.feeder_epoch {
                    return Err(FeederError::FencedSession);
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

        let now = Instant::now();
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
            graph: Arc::new(Mutex::new(GraphState::new(graph))),
            permits: Arc::new(Semaphore::new(capacity)),
            import_permits: Arc::new(Semaphore::new(1)),
            unavailable: Arc::new(AtomicBool::new(false)),
            sync_status: Arc::new(AtomicU8::new(SyncStatus::Uninitialized.to_code())),
            lease_deadline_millis: Arc::new(AtomicU64::new(0)),
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

    /// Reads the current synchronization status lock-free.
    pub(crate) fn sync_status(&self) -> SyncStatus {
        SyncStatus::from_code(self.sync_status.load(Ordering::Acquire))
    }

    /// Reports whether the feeder lease has expired without taking the graph lock.
    pub(crate) fn is_lease_expired(&self) -> bool {
        let deadline = self.lease_deadline_millis.load(Ordering::Acquire);
        if deadline == 0 {
            return true;
        }
        now_to_millis() > deadline
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
        let sync_status_atomic = self.sync_status.clone();
        let deadline_atomic = self.lease_deadline_millis.clone();
        let instance_id = self.instance_id.clone();
        self.run(move |state| {
            let res = state.register_session(req, instance_id)?;
            sync_status_atomic.store(res.status.to_code(), Ordering::Release);
            if let Some(dl) = state.lease_deadline {
                deadline_atomic.store(instant_to_millis(dl), Ordering::Release);
            }
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
        let sync_status_atomic = self.sync_status.clone();
        let deadline_atomic = self.lease_deadline_millis.clone();
        let instance_id = self.instance_id.clone();
        self.run(move |state| {
            let res = state.heartbeat(req, instance_id)?;
            sync_status_atomic.store(res.status.to_code(), Ordering::Release);
            if let Some(dl) = state.lease_deadline {
                deadline_atomic.store(instant_to_millis(dl), Ordering::Release);
            }
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
        let sync_status_atomic = self.sync_status.clone();
        let engine = self.info.engine;
        let limits = knotrel_core::GraphLimits {
            max_nodes: self.info.max_nodes,
            max_edges: self.info.max_edges,
        };
        let instance_id = self.instance_id.clone();

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _job_guard = job_guard;
            let mut panic_guard = WorkerPanicGuard {
                unavailable: unavailable.clone(),
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

            let Some(ref live_session_id) = state.feeder_session_id else {
                return Err(FeederError::SessionRequired);
            };
            if live_session_id != &req.feeder_session_id || req.feeder_epoch != state.feeder_epoch {
                return Err(FeederError::FencedSession);
            }

            let now = Instant::now();
            if let Some(deadline) = state.lease_deadline {
                if now > deadline {
                    return Err(FeederError::LeaseExpired);
                }
            } else {
                return Err(FeederError::LeaseExpired);
            }

            match req.mode {
                ImportMode::CreateOnly => {
                    if state.generation != 0 {
                        return Err(FeederError::GraphAlreadyInitialized);
                    }
                }
                ImportMode::Replace => {
                    let Some(ref expected_etag) = if_match else {
                        return Err(FeederError::PreconditionFailed(
                            "If-Match header is mandatory when mode is replace".into(),
                        ));
                    };
                    validate_replacement_precondition(&state, expected_etag)?;
                }
            }

            // 4. ATOMIC PUBLICATION
            state.failed = true;
            state.graph = candidate;
            state.generation += 1;
            state.version = vec![0];
            state.source_id = Some(req.source_id);
            state.source_epoch = Some(req.source_epoch);
            state.source_position = Some(req.source_position.clone());
            state.applied_delivery_sequence = 0;
            state.sync_status = SyncStatus::CatchingUp;
            state.dedup_window.reset();
            state.failed = false;

            panic_guard.success = true;
            sync_status_atomic.store(SyncStatus::CatchingUp.to_code(), Ordering::Release);

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
    ) -> Result<Vec<Outcome>, ServiceError> {
        self.run(move |state| {
            operations
                .into_iter()
                .map(|operation| state.apply(operation))
                .collect()
        })
        .await
    }

    /// Takes a coherent snapshot through the same bounded worker path as writes.
    pub(crate) async fn stats(&self) -> Result<GraphSnapshot, ServiceError> {
        let mode = self.mode;
        let instance_id = self.instance_id.clone();
        self.run(move |state| state.snapshot(mode, &instance_id))
            .await
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
            let mut state = GraphState::new(ConnectivityGraph::default());
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
            crate::lifecycle::readiness(&crate::Lifecycle::ready(), &service).status(),
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
        let response = crate::lifecycle::readiness(&lifecycle, &service);
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
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
}
