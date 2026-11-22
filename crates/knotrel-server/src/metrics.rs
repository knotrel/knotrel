//! Opt-in Prometheus exposition (`GET /metrics`), owned per router instance.
//!
//! HTTP accounting lives in [`track_http`]; service-health gauges are read at
//! scrape time by [`HealthCollector`]. Rejection and uncertain-outcome
//! counters, the worker-published graph-size gauges and the worker-phase
//! histograms live in [`ServiceTelemetry`], owned by the `Service` they
//! observe, as do the instant of the last feeder application and the import
//! metrics, which an [`ImportObservation`] carried with the import permit
//! records. Nothing here acquires the graph mutex, a graph-job admission
//! permit or an import permit; graph counts are published by workers that
//! already hold the graph lock, and phase timings are recorded by workers
//! after they release it. Scrapes and request
//! accounting do use synchronization internal to the `prometheus-client`
//! registry (metric-family maps and histogram state). Semantics are specified
//! in `docs/observability-contract.md`.

use crate::{
    Lifecycle, ServerMode,
    api::{ErrorCode, Operation, Outcome, Route},
    lifecycle::{self, ReadinessState},
    service::{self, FeederError, Lane, Service},
};
use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use prometheus_client::{
    collector::Collector,
    encoding::{DescriptorEncoder, EncodeMetric, prometheus_text},
    metrics::{
        MetricType,
        counter::Counter,
        family::Family,
        gauge::{ConstGauge, Gauge},
        histogram::Histogram,
    },
    registry::Registry,
};
use std::{
    ops::{Deref, DerefMut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MILLISECONDS_PER_SECOND: f64 = 1_000.0;
const PRESENT_STAMP_OFFSET: u64 = 1;

/// Upper bounds, in seconds, of the HTTP duration histogram (plus `+Inf`).
const DURATION_BUCKETS: [f64; 15] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Upper bounds, in seconds, of the worker-phase histograms (plus `+Inf`).
///
/// Two steps per decade from 10 µs, below the HTTP floor because in-memory
/// lock holds and uncontended acquisitions are typically microseconds, up to
/// the 30 s default request timeout. They resolve orders of magnitude; they
/// are not service-level objectives.
const JOB_PHASE_BUCKETS: [f64; 14] = [
    0.00001, 0.00005, 0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0,
];

/// Upper bounds, in seconds, of the catch-up histogram (plus `+Inf`).
///
/// Catch-up spans a feeder replaying source history, so it ranges from
/// milliseconds to many minutes: the buckets run from 10 ms to one hour. They
/// resolve orders of magnitude; they are not service-level objectives, and an
/// interval beyond one hour lands only in `+Inf`.
const CATCHUP_BUCKETS: [f64; 13] = [
    0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0,
];

const REQUEST_LABEL_COUNT: usize = 3;
const ROUTE_LABEL_COUNT: usize = 1;
const OUTCOME_LABEL_COUNT: usize = 1;
const OPERATION_LABEL_COUNT: usize = 2;
const REJECTION_LABEL_COUNT: usize = 2;

type RequestLabels = [(&'static str, &'static str); REQUEST_LABEL_COUNT];
type RouteLabels = [(&'static str, &'static str); ROUTE_LABEL_COUNT];
type OutcomeLabels = [(&'static str, &'static str); OUTCOME_LABEL_COUNT];
type RejectionLabels = [(&'static str, &'static str); REJECTION_LABEL_COUNT];
type OperationLabels = [(&'static str, &'static str); OPERATION_LABEL_COUNT];
type DurationFamily = Family<RouteLabels, Histogram, fn() -> Histogram>;
type PhaseFamily = Family<OutcomeLabels, Histogram, fn() -> Histogram>;

/// Registry and HTTP metric families for one router instance.
#[derive(Debug)]
pub(crate) struct Metrics {
    registry: Registry,
    requests: Family<RequestLabels, Counter>,
    durations: DurationFamily,
}

impl Metrics {
    pub(crate) fn new(service: Service, lifecycle: Lifecycle) -> Arc<Self> {
        let mut registry = Registry::default();
        let requests = Family::<RequestLabels, Counter>::default();
        let durations: DurationFamily =
            Family::new_with_constructor(|| Histogram::new(DURATION_BUCKETS));
        registry.register(
            "knotrel_http_requests",
            "HTTP requests finished or cancelled, by matched route template, method and status class",
            requests.clone(),
        );
        registry.register(
            "knotrel_http_request_duration_seconds",
            "Server-side time from middleware entry to response construction for requests that produced a response; excludes body transmission",
            durations.clone(),
        );
        if let Some(telemetry) = service.telemetry() {
            telemetry.register(&mut registry);
        }
        registry.register_collector(Box::new(HealthCollector { service, lifecycle }));
        Arc::new(Self {
            registry,
            requests,
            durations,
        })
    }
}

/// Why a request was refused before any graph job or import ran for it,
/// and on which admission lane: one variant per `(reason, lane)` series of
/// `knotrel_requests_rejected_total`.
///
/// `busy` is split by the lane whose permits were exhausted; the import
/// lane's refusal is `import_capacity`; `shutting_down` and `unavailable`
/// are refused before or regardless of a lane and carry `lane="none"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    BusyShared,
    BusyFeederData,
    BusyFeederControl,
    ImportCapacity,
    ShuttingDown,
    Unavailable,
}

/// `lane` label value of rejections not tied to an admission lane.
const NO_LANE: &str = "none";

impl Rejection {
    const ALL: [Self; 6] = [
        Self::BusyShared,
        Self::BusyFeederData,
        Self::BusyFeederControl,
        Self::ImportCapacity,
        Self::ShuttingDown,
        Self::Unavailable,
    ];

    /// The refusal of a full `lane`: `busy` on that lane, or
    /// `import_capacity` for the import lane.
    pub(crate) fn busy(lane: Lane) -> Self {
        match lane {
            Lane::Shared => Self::BusyShared,
            Lane::FeederData => Self::BusyFeederData,
            Lane::FeederControl => Self::BusyFeederControl,
            Lane::Import => Self::ImportCapacity,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::BusyShared | Self::BusyFeederData | Self::BusyFeederControl => "busy",
            Self::ImportCapacity => "import_capacity",
            Self::ShuttingDown => "shutting_down",
            Self::Unavailable => "unavailable",
        }
    }

    /// The admission lane, if the refusal belongs to one.
    fn lane(self) -> Option<Lane> {
        match self {
            Self::BusyShared => Some(Lane::Shared),
            Self::BusyFeederData => Some(Lane::FeederData),
            Self::BusyFeederControl => Some(Lane::FeederControl),
            Self::ImportCapacity => Some(Lane::Import),
            Self::ShuttingDown | Self::Unavailable => None,
        }
    }

    fn lane_label(self) -> &'static str {
        self.lane().map_or(NO_LANE, Lane::label)
    }

    /// Whether the series exists in `mode`. `busy` on a feeder lane exists
    /// only in source-managed mode; every other series exists in both
    /// modes, including `import_capacity`, which predates the `lane` label
    /// and is kept at zero in standalone mode.
    fn exists_in(self, mode: ServerMode) -> bool {
        match self {
            Self::BusyFeederData | Self::BusyFeederControl => mode == ServerMode::SourceManaged,
            _ => true,
        }
    }
}

/// Why the effect of an admitted state-changing job may be unknown to its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Uncertainty {
    RequestTimeout,
    Cancelled,
    WorkerPanic,
}

impl Uncertainty {
    const ALL: [Self; 3] = [Self::RequestTimeout, Self::Cancelled, Self::WorkerPanic];

    fn label(self) -> &'static str {
        match self {
            Self::RequestTimeout => "request_timeout",
            Self::Cancelled => "cancelled",
            Self::WorkerPanic => "worker_panic",
        }
    }
}

/// Graph job type, the `kind` label of the worker-phase histograms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    Operation,
    Batch,
    Stats,
    FeederSession,
    FeederHeartbeat,
    FeederEvents,
    FeederGap,
    FeederBarrier,
    ImportPublish,
    Inspect,
}

impl JobKind {
    const ALL: [Self; 10] = [
        Self::Operation,
        Self::Batch,
        Self::Stats,
        Self::FeederSession,
        Self::FeederHeartbeat,
        Self::FeederEvents,
        Self::FeederGap,
        Self::FeederBarrier,
        Self::ImportPublish,
        Self::Inspect,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::Batch => "batch",
            Self::Stats => "stats",
            Self::FeederSession => "feeder_session",
            Self::FeederHeartbeat => "feeder_heartbeat",
            Self::FeederEvents => "feeder_events",
            Self::FeederGap => "feeder_gap",
            Self::FeederBarrier => "feeder_barrier",
            Self::ImportPublish => "import_publish",
            Self::Inspect => "inspect",
        }
    }

    /// Whether HTTP can create this job in `mode`; feeder and import
    /// endpoints answer `mode_conflict` in standalone mode before admission,
    /// and inspection routes answer `inspection_disabled` before admission
    /// unless `inspection` is enabled.
    fn reachable_in(self, mode: ServerMode, inspection: bool) -> bool {
        match self {
            Self::Inspect => inspection,
            Self::Operation | Self::Batch | Self::Stats => true,
            _ => mode == ServerMode::SourceManaged,
        }
    }
}

/// Direct-client graph operation, the `op` label of the operation counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GraphOp {
    AddNode,
    Link,
    Cut,
    Connected,
    SetNodeActive,
    SetEdgeEnabled,
    RemoveNode,
    NodeState,
    EdgeState,
}

impl GraphOp {
    const ALL: [Self; 9] = [
        Self::AddNode,
        Self::Link,
        Self::Cut,
        Self::Connected,
        Self::SetNodeActive,
        Self::SetEdgeEnabled,
        Self::RemoveNode,
        Self::NodeState,
        Self::EdgeState,
    ];

    pub(crate) fn of(operation: &Operation) -> Self {
        match operation {
            Operation::AddNode { .. } => Self::AddNode,
            Operation::Link { .. } => Self::Link,
            Operation::Cut { .. } => Self::Cut,
            Operation::Connected { .. } => Self::Connected,
            Operation::SetNodeActive { .. } => Self::SetNodeActive,
            Operation::SetEdgeEnabled { .. } => Self::SetEdgeEnabled,
            Operation::RemoveNode { .. } => Self::RemoveNode,
            Operation::NodeState { .. } => Self::NodeState,
            Operation::EdgeState { .. } => Self::EdgeState,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AddNode => "add_node",
            Self::Link => "link",
            Self::Cut => "cut",
            Self::Connected => "connected",
            Self::SetNodeActive => "set_node_active",
            Self::SetEdgeEnabled => "set_edge_enabled",
            Self::RemoveNode => "remove_node",
            Self::NodeState => "node_state",
            Self::EdgeState => "edge_state",
        }
    }

    /// `outcome` labels, indexed by [`outcome_index`].
    fn outcomes(self) -> [&'static str; OperationOutcome::COUNT] {
        match self {
            Self::Connected => ["true", "false", "error"],
            Self::NodeState | Self::EdgeState => ["registered", "unregistered", "error"],
            _ => ["changed", "unchanged", "error"],
        }
    }

    fn is_query(self) -> bool {
        matches!(self, Self::Connected | Self::NodeState | Self::EdgeState)
    }

    fn requires_lifecycle(self) -> bool {
        !matches!(
            self,
            Self::AddNode | Self::Link | Self::Cut | Self::Connected
        )
    }

    /// Whether HTTP can execute this operation in `mode` with the given
    /// lifecycle support: source-managed mode refuses direct mutations with
    /// `direct_mutation_disabled`, and lifecycle operations are refused with
    /// `topology_lifecycle_disabled` without lifecycle support, both before
    /// admission.
    fn reachable_in(self, mode: ServerMode, lifecycle: bool) -> bool {
        (mode == ServerMode::Standalone || self.is_query())
            && (lifecycle || !self.requires_lifecycle())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum OperationOutcome {
    Positive = 0,
    Negative = 1,
    Error = 2,
}

impl OperationOutcome {
    const COUNT: usize = Self::Error as usize + 1;
}

/// Index into [`GraphOp::outcomes`]: changed or `true`, unchanged or `false`, error.
fn outcome_index(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Changed { changed: true }
        | Outcome::Connected { connected: true }
        | Outcome::NodeState {
            registered: true, ..
        }
        | Outcome::EdgeState {
            registered: true, ..
        } => OperationOutcome::Positive as usize,
        Outcome::Changed { changed: false }
        | Outcome::Connected { connected: false }
        | Outcome::NodeState {
            registered: false, ..
        }
        | Outcome::EdgeState {
            registered: false, ..
        } => OperationOutcome::Negative as usize,
        Outcome::Error { .. } => OperationOutcome::Error as usize,
    }
}

/// Lifecycle count gauges, registered only with lifecycle support.
#[derive(Debug, Default)]
struct LifecycleGauges {
    active_nodes: Gauge,
    enabled_edges: Gauge,
    effective_edges: Gauge,
}

/// How the event loop finished evaluating one feeder event, the `result`
/// label of the feeder-event counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeederEventResult {
    Changed,
    /// An applied no-op mutation or `progress` event.
    Unchanged,
    Duplicate,
    Rejected,
}

impl FeederEventResult {
    const ALL: [Self; 4] = [
        Self::Changed,
        Self::Unchanged,
        Self::Duplicate,
        Self::Rejected,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
            Self::Duplicate => "duplicate",
            Self::Rejected => "rejected",
        }
    }
}

/// Pre-created feeder-event counters; present only in source-managed mode.
#[derive(Debug)]
struct FeederEventCounters {
    family: Family<OutcomeLabels, Counter>,
    results: [Counter; FeederEventResult::ALL.len()],
}

/// Why the service rejected a feeder session, heartbeat, events, gap or
/// barrier request: the `code` label of the feeder-rejection counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeederRejection {
    SessionRequired,
    FencedSession,
    LeaseExpired,
    InvalidLeaseDuration,
    GraphUninitialized,
    GraphDesynchronized,
    StaleGeneration,
    SourceMismatch,
    SourceEpochMismatch,
    OutOfOrderSequence,
    SourcePredecessorMismatch,
    ConflictingDuplicate,
    UnverifiableDuplicate,
    SequenceExhausted,
    InvalidRequest,
    /// A lifecycle event without lifecycle support.
    TopologyLifecycleDisabled,
    /// Any graph-domain error of an event.
    EventFailed,
    BarrierPositionMismatch,
    BarrierNotReached,
    SupersededBarrier,
}

impl FeederRejection {
    pub(crate) const ALL: [Self; 20] = [
        Self::SessionRequired,
        Self::FencedSession,
        Self::LeaseExpired,
        Self::InvalidLeaseDuration,
        Self::GraphUninitialized,
        Self::GraphDesynchronized,
        Self::StaleGeneration,
        Self::SourceMismatch,
        Self::SourceEpochMismatch,
        Self::OutOfOrderSequence,
        Self::SourcePredecessorMismatch,
        Self::ConflictingDuplicate,
        Self::UnverifiableDuplicate,
        Self::SequenceExhausted,
        Self::InvalidRequest,
        Self::TopologyLifecycleDisabled,
        Self::EventFailed,
        Self::BarrierPositionMismatch,
        Self::BarrierNotReached,
        Self::SupersededBarrier,
    ];

    /// Classifies a feeder error. `None` for admission refusals (counted by
    /// the request-rejection counter); mode and request-identity refusals,
    /// which the HTTP adapter checks first and which some service entry
    /// points (`Service::register_session`, `Service::heartbeat`,
    /// `Service::import`) also return; import-only errors; and malformed
    /// input.
    pub(crate) fn of(error: &FeederError) -> Option<Self> {
        Some(match error {
            FeederError::SessionRequired => Self::SessionRequired,
            FeederError::FencedSession => Self::FencedSession,
            FeederError::LeaseExpired => Self::LeaseExpired,
            FeederError::InvalidLeaseDuration => Self::InvalidLeaseDuration,
            FeederError::GraphUninitialized => Self::GraphUninitialized,
            FeederError::GraphDesynchronized => Self::GraphDesynchronized,
            FeederError::StaleGeneration(_) => Self::StaleGeneration,
            FeederError::SourceMismatch(_) => Self::SourceMismatch,
            FeederError::SourceEpochMismatch(_) => Self::SourceEpochMismatch,
            FeederError::OutOfOrderSequence { .. } => Self::OutOfOrderSequence,
            FeederError::SourcePredecessorMismatch(_) => Self::SourcePredecessorMismatch,
            FeederError::ConflictingDuplicate(_) => Self::ConflictingDuplicate,
            FeederError::UnverifiableDuplicate(_) => Self::UnverifiableDuplicate,
            FeederError::SequenceExhausted => Self::SequenceExhausted,
            FeederError::InvalidRequest(_) => Self::InvalidRequest,
            FeederError::TopologyLifecycleDisabled => Self::TopologyLifecycleDisabled,
            FeederError::Domain(..) => Self::EventFailed,
            FeederError::BarrierPositionMismatch { .. } => Self::BarrierPositionMismatch,
            FeederError::BarrierNotReached { .. } => Self::BarrierNotReached,
            FeederError::SupersededBarrier { .. } => Self::SupersededBarrier,
            FeederError::ImportInProgress
            | FeederError::Unavailable
            | FeederError::ModeMismatch
            | FeederError::StaleInstanceId(_)
            | FeederError::InvalidInstanceId(_)
            | FeederError::InvalidGraphId(_)
            | FeederError::GraphAlreadyInitialized
            | FeederError::GenerationExhausted
            | FeederError::PreconditionFailed(_)
            | FeederError::MalformedJson
            | FeederError::UnprocessableJson(_)
            | FeederError::PayloadTooLarge => return None,
        })
    }

    pub(crate) fn label(self) -> &'static str {
        let code = match self {
            Self::SessionRequired => ErrorCode::SessionRequired,
            Self::FencedSession => ErrorCode::FencedFeederSession,
            Self::LeaseExpired => ErrorCode::LeaseExpired,
            Self::InvalidLeaseDuration => ErrorCode::InvalidLeaseDuration,
            Self::GraphUninitialized => ErrorCode::GraphUninitialized,
            Self::GraphDesynchronized => ErrorCode::GraphDesynchronized,
            Self::StaleGeneration => ErrorCode::StaleGeneration,
            Self::SourceMismatch => ErrorCode::SourceMismatch,
            Self::SourceEpochMismatch => ErrorCode::SourceEpochMismatch,
            Self::OutOfOrderSequence => ErrorCode::OutOfOrderSequence,
            Self::SourcePredecessorMismatch => ErrorCode::SourcePredecessorMismatch,
            Self::ConflictingDuplicate => ErrorCode::ConflictingDuplicate,
            Self::UnverifiableDuplicate => ErrorCode::UnverifiableDuplicate,
            Self::SequenceExhausted => ErrorCode::SequenceExhausted,
            Self::InvalidRequest => ErrorCode::InvalidRequest,
            Self::TopologyLifecycleDisabled => ErrorCode::TopologyLifecycleDisabled,
            Self::EventFailed => ErrorCode::EventFailed,
            Self::BarrierPositionMismatch => ErrorCode::BarrierPositionMismatch,
            Self::BarrierNotReached => ErrorCode::BarrierNotReached,
            Self::SupersededBarrier => ErrorCode::SupersededBarrier,
        };
        code.as_str()
    }
}

/// What moved the graph into `desynchronized`: the `cause` label of the
/// desynchronization counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desynchronization {
    GapDeclared,
    SourceEpochMismatch,
    /// A `prev_delivery_sequence` or `prev_source_position` mismatch.
    SourcePredecessorMismatch,
    ConflictingDuplicate,
}

impl Desynchronization {
    pub(crate) const ALL: [Self; 4] = [
        Self::GapDeclared,
        Self::SourceEpochMismatch,
        Self::SourcePredecessorMismatch,
        Self::ConflictingDuplicate,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::GapDeclared => "gap_declared",
            Self::SourceEpochMismatch => "source_epoch_mismatch",
            Self::SourcePredecessorMismatch => "source_predecessor_mismatch",
            Self::ConflictingDuplicate => "conflicting_duplicate",
        }
    }
}

/// Terminal result of an import that obtained the import permit: the
/// `result` label of the import counter. Classified by the phase the import
/// reached, not by error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportResult {
    /// The candidate became the live generation.
    Published,
    /// The body read failed, or parsing, request identity or candidate
    /// construction rejected the payload before the lock request.
    PayloadRejected,
    /// Rejected after the lock request: unavailable service, session or lease
    /// fencing, import mode, `If-Match` or generation exhaustion.
    PublicationRejected,
    /// The import's worker panicked before publication.
    WorkerFailed,
    /// Ownership ended before the worker started and before any other result:
    /// the caller abandoned the body read, or the queued worker closure was
    /// dropped without running.
    Abandoned,
}

impl ImportResult {
    const ALL: [Self; 5] = [
        Self::Published,
        Self::PayloadRejected,
        Self::PublicationRejected,
        Self::WorkerFailed,
        Self::Abandoned,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::PayloadRejected => "payload_rejected",
            Self::PublicationRejected => "publication_rejected",
            Self::WorkerFailed => "worker_failed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// Timed import phase: the `phase` label of the import-duration histogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportPhase {
    /// From permit acquisition until the body read returns.
    Receive,
    /// In the worker, from after its start hook until the graph-lock request.
    Prepare,
    /// From the graph-lock request until the worker has released the lock and
    /// dropped its request state.
    Publish,
}

impl ImportPhase {
    const ALL: [Self; 3] = [Self::Receive, Self::Prepare, Self::Publish];

    fn label(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Prepare => "prepare",
            Self::Publish => "publish",
        }
    }
}

/// Pre-created import gauge, counters and histograms; present only in
/// source-managed mode, the only mode whose router accepts imports.
#[derive(Debug)]
struct ImportMetrics {
    in_progress: Gauge,
    results_family: Family<OutcomeLabels, Counter>,
    results: [Counter; ImportResult::ALL.len()],
    durations_family: PhaseFamily,
    durations: [Histogram; ImportPhase::ALL.len()],
}

/// Kind of catch-up interval: the `kind` label of the catch-up histogram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatchupKind {
    /// Started by a `create_only` import, the first publication.
    Initial,
    /// Started by a `replace` import of an already initialized graph.
    Replacement,
}

impl CatchupKind {
    const ALL: [Self; 2] = [Self::Initial, Self::Replacement];

    fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Replacement => "replacement",
        }
    }
}

/// Pre-created catch-up histograms; present only in source-managed mode.
#[derive(Debug)]
struct CatchupMetrics {
    durations_family: PhaseFamily,
    durations: [Histogram; CatchupKind::ALL.len()],
}

/// Pre-created feeder-rejection and desynchronization counters; present
/// only in source-managed mode.
#[derive(Debug)]
struct FeederFailureCounters {
    rejections_family: Family<OutcomeLabels, Counter>,
    rejections: [Counter; FeederRejection::ALL.len()],
    desyncs_family: Family<OutcomeLabels, Counter>,
    desyncs: [Counter; Desynchronization::ALL.len()],
}

fn catchup_histogram() -> Histogram {
    Histogram::new(CATCHUP_BUCKETS)
}

fn job_phase_histogram() -> Histogram {
    Histogram::new(JOB_PHASE_BUCKETS)
}

/// Pre-created histograms of one job kind.
#[derive(Debug)]
struct PhaseHistograms {
    queue: Histogram,
    lock_wait: Histogram,
    execution: Histogram,
}

/// Rejection and uncertain-outcome counters, graph-size gauges,
/// worker-phase histograms, graph-operation and feeder-event outcome
/// counters, feeder-rejection and desynchronization counters, and the import
/// gauge, counter and histograms for one service instance.
///
/// Owned by `Service` only when metrics are enabled. Every series is created
/// up front, so recording never takes a family lock: counters and gauges are
/// single atomic updates, and a histogram observation takes only that
/// histogram's internal mutex. Holds no reference back to `Service` or the
/// registry.
#[derive(Debug)]
pub(crate) struct ServiceTelemetry {
    rejected_family: Family<RejectionLabels, Counter>,
    uncertain_family: Family<OutcomeLabels, Counter>,
    // Indexed by `Rejection`; `None` for series that do not exist in this
    // mode, whose refusals HTTP cannot produce.
    rejected: [Option<Counter>; Rejection::ALL.len()],
    uncertain: [Counter; Uncertainty::ALL.len()],
    graph_nodes: Gauge,
    graph_edges: Gauge,
    // Present only with lifecycle support.
    lifecycle_gauges: Option<LifecycleGauges>,
    queue_family: PhaseFamily,
    lock_wait_family: PhaseFamily,
    execution_family: PhaseFamily,
    // Indexed by `JobKind`; `None` for kinds HTTP cannot reach in this mode,
    // whose observations (possible only through direct service calls) are dropped.
    phases: [Option<PhaseHistograms>; JobKind::ALL.len()],
    operations_family: Family<OperationLabels, Counter>,
    // Indexed by `GraphOp`, then `outcome_index`; `None` for operations HTTP
    // cannot execute in this mode.
    operations: [Option<[Counter; OperationOutcome::COUNT]>; GraphOp::ALL.len()],
    feeder_events: Option<FeederEventCounters>,
    feeder_failures: Option<FeederFailureCounters>,
    // Lease-clock milliseconds of the last feeder application plus one;
    // 0 means none yet.
    last_applied: AtomicU64,
    imports: Option<ImportMetrics>,
    catchup: Option<CatchupMetrics>,
}

impl ServiceTelemetry {
    pub(crate) fn new(mode: ServerMode, lifecycle: bool, inspection: bool) -> Arc<Self> {
        let rejected_family = Family::<RejectionLabels, Counter>::default();
        let uncertain_family = Family::<OutcomeLabels, Counter>::default();
        let rejected = Rejection::ALL.map(|reason| {
            reason.exists_in(mode).then(|| {
                rejected_family
                    .get_or_create(&[("reason", reason.label()), ("lane", reason.lane_label())])
                    .clone()
            })
        });
        let uncertain = Uncertainty::ALL.map(|cause| {
            uncertain_family
                .get_or_create(&[("cause", cause.label())])
                .clone()
        });
        let queue_family: PhaseFamily = Family::new_with_constructor(job_phase_histogram);
        let lock_wait_family: PhaseFamily = Family::new_with_constructor(job_phase_histogram);
        let execution_family: PhaseFamily = Family::new_with_constructor(job_phase_histogram);
        let phases = JobKind::ALL.map(|kind| {
            kind.reachable_in(mode, inspection).then(|| {
                let labels = [("kind", kind.label())];
                PhaseHistograms {
                    queue: queue_family.get_or_create(&labels).clone(),
                    lock_wait: lock_wait_family.get_or_create(&labels).clone(),
                    execution: execution_family.get_or_create(&labels).clone(),
                }
            })
        });
        let operations_family = Family::<OperationLabels, Counter>::default();
        let operations = GraphOp::ALL.map(|op| {
            op.reachable_in(mode, lifecycle).then(|| {
                op.outcomes().map(|outcome| {
                    operations_family
                        .get_or_create(&[("op", op.label()), ("outcome", outcome)])
                        .clone()
                })
            })
        });
        let feeder_events = (mode == ServerMode::SourceManaged).then(|| {
            let family = Family::<OutcomeLabels, Counter>::default();
            let results = FeederEventResult::ALL
                .map(|result| family.get_or_create(&[("result", result.label())]).clone());
            FeederEventCounters { family, results }
        });
        let feeder_failures = (mode == ServerMode::SourceManaged).then(|| {
            let rejections_family = Family::<OutcomeLabels, Counter>::default();
            let rejections = FeederRejection::ALL.map(|code| {
                rejections_family
                    .get_or_create(&[("code", code.label())])
                    .clone()
            });
            let desyncs_family = Family::<OutcomeLabels, Counter>::default();
            let desyncs = Desynchronization::ALL.map(|cause| {
                desyncs_family
                    .get_or_create(&[("cause", cause.label())])
                    .clone()
            });
            FeederFailureCounters {
                rejections_family,
                rejections,
                desyncs_family,
                desyncs,
            }
        });
        let imports = (mode == ServerMode::SourceManaged).then(|| {
            let results_family = Family::<OutcomeLabels, Counter>::default();
            let results = ImportResult::ALL.map(|result| {
                results_family
                    .get_or_create(&[("result", result.label())])
                    .clone()
            });
            let durations_family: PhaseFamily = Family::new_with_constructor(job_phase_histogram);
            let durations = ImportPhase::ALL.map(|phase| {
                durations_family
                    .get_or_create(&[("phase", phase.label())])
                    .clone()
            });
            ImportMetrics {
                in_progress: Gauge::default(),
                results_family,
                results,
                durations_family,
                durations,
            }
        });
        let catchup = (mode == ServerMode::SourceManaged).then(|| {
            let durations_family: PhaseFamily = Family::new_with_constructor(catchup_histogram);
            let durations = CatchupKind::ALL.map(|kind| {
                durations_family
                    .get_or_create(&[("kind", kind.label())])
                    .clone()
            });
            CatchupMetrics {
                durations_family,
                durations,
            }
        });
        Arc::new(Self {
            rejected_family,
            uncertain_family,
            rejected,
            uncertain,
            graph_nodes: Gauge::default(),
            graph_edges: Gauge::default(),
            lifecycle_gauges: lifecycle.then(LifecycleGauges::default),
            queue_family,
            lock_wait_family,
            execution_family,
            phases,
            operations_family,
            operations,
            feeder_events,
            feeder_failures,
            last_applied: AtomicU64::new(0),
            imports,
            catchup,
        })
    }

    fn register(&self, registry: &mut Registry) {
        registry.register(
            "knotrel_requests_rejected",
            "Requests refused before any graph job or import ran for them, by refusing boundary and admission lane (busy by the full lane, import_capacity on the import lane, none for shutting_down and unavailable); shutting_down includes non-probe requests that would not create a graph job",
            self.rejected_family.clone(),
        );
        registry.register(
            "knotrel_requests_outcome_uncertain",
            "Uncertain-outcome observations by cause: an admitted state-changing job may have taken effect without its caller learning the result; counts observations, not distinct requests",
            self.uncertain_family.clone(),
        );
        registry.register(
            "knotrel_graph_nodes",
            "Nodes in the live graph as last published at construction or by a worker under the graph lock when its state-changing graph work returned; a later panic does not roll it back, so after a worker failure it may not describe the current graph",
            self.graph_nodes.clone(),
        );
        registry.register(
            "knotrel_graph_edges",
            "Edges in the live graph as last published at construction or by a worker under the graph lock when its state-changing graph work returned; a later panic does not roll it back, so after a worker failure it may not describe the current graph",
            self.graph_edges.clone(),
        );
        if let Some(gauges) = &self.lifecycle_gauges {
            registry.register(
                "knotrel_graph_active_nodes",
                "Registered nodes whose flag is active, published with knotrel_graph_nodes at the same boundaries; present only with node and edge lifecycle support",
                gauges.active_nodes.clone(),
            );
            registry.register(
                "knotrel_graph_enabled_edges",
                "Registered edges whose flag is enabled, whether or not both endpoints are active, published with knotrel_graph_edges at the same boundaries; present only with node and edge lifecycle support",
                gauges.enabled_edges.clone(),
            );
            registry.register(
                "knotrel_graph_effective_edges",
                "Edges that participate in connectivity (enabled, both endpoints active), published with knotrel_graph_edges at the same boundaries; present only with node and edge lifecycle support",
                gauges.effective_edges.clone(),
            );
        }
        registry.register(
            "knotrel_graph_job_queue_seconds",
            "Time an admitted graph job waited in the blocking pool, from submission to the start of its worker; recorded by the worker when it starts, also after its caller has gone",
            self.queue_family.clone(),
        );
        registry.register(
            "knotrel_graph_lock_wait_seconds",
            "Time a graph job's worker waited to acquire the graph lock; recorded after the worker releases the lock, also after its caller has gone",
            self.lock_wait_family.clone(),
        );
        registry.register(
            "knotrel_graph_job_execution_seconds",
            "Time a graph job's worker held the graph lock, including validation, graph work and publication under it, until release or unwinding; recorded after release, also after its caller has gone",
            self.execution_family.clone(),
        );
        registry.register(
            "knotrel_graph_operations",
            "Direct-client graph operations from /v1/operations and /v1/batch evaluated by a worker, by operation and outcome; counted when each operation returns, also after its caller has gone; excludes feeder events and imports",
            self.operations_family.clone(),
        );
        if let Some(events) = &self.feeder_events {
            registry.register(
                "knotrel_feeder_events",
                "Feeder events evaluated by the event loop, by result: changed or unchanged (no-op mutation or progress) when applied, duplicate when an exact retained duplicate, rejected when evaluation stopped the request at that event; events never reached count nothing",
                events.family.clone(),
            );
        }
        if let Some(failures) = &self.feeder_failures {
            registry.register(
                "knotrel_feeder_rejections",
                "Feeder session, heartbeat, events, gap and barrier requests the service rejected, one per request by the underlying error code (graph-domain event errors as event_failed); counted by the worker or service where the error is produced, also after its caller has gone; excludes admission refusals, HTTP-adapter checks, malformed bodies and imports",
                failures.rejections_family.clone(),
            );
            registry.register(
                "knotrel_desynchronizations",
                "Transitions of the graph into desynchronized, by cause; counted under the graph lock where the state changes, also after its caller has gone; requests on an already desynchronized graph add none",
                failures.desyncs_family.clone(),
            );
        }
        if let Some(imports) = &self.imports {
            registry.register(
                "knotrel_import_in_progress",
                "1 while an import holds the single import permit (body receipt, worker queue, preparation and publication, also after its caller has gone), else 0",
                imports.in_progress.clone(),
            );
            registry.register(
                "knotrel_imports",
                "Imports that obtained the import permit, one per import by terminal result, recorded by whichever of handler or worker owns the import when it ends, also after its caller has gone; classified by phase reached, not error code; abandoned means ownership ended before the worker started, as when the caller abandons the body read; refusals before the permit count nothing",
                imports.results_family.clone(),
            );
            registry.register(
                "knotrel_import_duration_seconds",
                "Import time by phase on the monotonic clock: receive (permit acquired to body read returned), prepare (worker parse and off-lock candidate build), publish (graph-lock request to the worker's end, including lock wait, validation and swap); disjoint, excluding the handler's pre-spawn work, the blocking-pool queue and the worker start; a failed phase is observed up to its error, a phase interrupted by a panic or an abandoned body read, or never reached, is not",
                imports.durations_family.clone(),
            );
        }
        if let Some(catchup) = &self.catchup {
            registry.register(
                "knotrel_catchup_duration_seconds",
                "Completed catch-up intervals on the monotonic clock, by kind: from the publication of an import (initial for create_only, replacement for replace) to the first synchronized barrier in that generation, observed once per interval; includes any time the generation spent stale; an interval replaced by a newer import, left on a desynchronized graph, or whose barrier worker panicked is not observed; not source lag and not the time since the last event",
                catchup.durations_family.clone(),
            );
        }
    }

    /// Publishes the live graph's maintained counts. Callers hold the graph
    /// lock, so publications are ordered and an older job cannot overwrite a
    /// newer one. The two stores are separate: a concurrent scrape may pair
    /// one job's node count with the next job's edge count. A publication is
    /// never rolled back, even if the worker panics afterwards.
    ///
    /// Node and edge counts are registered counts. With lifecycle support the
    /// three lifecycle gauges are published too; each count is read in `O(1)`.
    pub(crate) fn publish_graph_counts(&self, graph: &crate::graph::ServiceGraph) {
        let gauge = |count: usize| i64::try_from(count).unwrap_or(i64::MAX);
        self.graph_nodes.set(gauge(graph.node_count()));
        self.graph_edges.set(gauge(graph.edge_count()));
        if let (Some(gauges), Some(counts)) = (&self.lifecycle_gauges, graph.lifecycle_counts()) {
            gauges.active_nodes.set(gauge(counts.active_node_count));
            gauges.enabled_edges.set(gauge(counts.enabled_edge_count));
            gauges
                .effective_edges
                .set(gauge(counts.effective_edge_count));
        }
    }

    fn phases(&self, kind: JobKind) -> Option<&PhaseHistograms> {
        self.phases[kind as usize].as_ref()
    }

    /// Counts one evaluated direct-client operation. Atomic, so callers may
    /// hold the graph lock.
    pub(crate) fn count_operation(&self, op: GraphOp, outcome: &Outcome) {
        if let Some(counters) = &self.operations[op as usize] {
            counters[outcome_index(outcome)].inc();
        }
    }

    /// Counts one evaluated feeder event. Atomic, so callers may hold the
    /// graph lock.
    pub(crate) fn count_feeder_event(&self, result: FeederEventResult) {
        if let Some(events) = &self.feeder_events {
            events.results[result as usize].inc();
        }
    }

    /// Counts one rejected feeder request. Atomic, so callers may hold the
    /// graph lock.
    pub(crate) fn count_feeder_rejection(&self, code: FeederRejection) {
        if let Some(failures) = &self.feeder_failures {
            failures.rejections[code as usize].inc();
        }
    }

    /// Counts one transition into `desynchronized`. Atomic, so callers may
    /// hold the graph lock.
    pub(crate) fn count_desynchronization(&self, cause: Desynchronization) {
        if let Some(failures) = &self.feeder_failures {
            failures.desyncs[cause as usize].inc();
        }
    }

    /// Records that a feeder event was applied or an import published now.
    /// Callers hold the graph lock, so stamps arrive in order; `fetch_max`
    /// additionally keeps an older stamp from replacing a newer one.
    pub(crate) fn mark_applied(&self) {
        let stamp = service::now_to_millis().saturating_add(PRESENT_STAMP_OFFSET);
        self.last_applied.fetch_max(stamp, Ordering::Relaxed);
    }

    /// Seconds from the last application to `now_millis`, if there was one.
    fn last_applied_age_seconds(&self, now_millis: u64) -> Option<f64> {
        let stamp = self.last_applied.load(Ordering::Relaxed);
        (stamp != 0).then(|| {
            now_millis.saturating_sub(stamp - PRESENT_STAMP_OFFSET) as f64 / MILLISECONDS_PER_SECOND
        })
    }

    pub(crate) fn reject(&self, reason: Rejection) {
        if let Some(counter) = &self.rejected[reason as usize] {
            counter.inc();
        }
    }

    pub(crate) fn uncertain(&self, cause: Uncertainty) {
        self.uncertain[cause as usize].inc();
    }

    #[cfg(test)]
    pub(crate) fn rejected_count(&self, reason: Rejection) -> u64 {
        self.rejected[reason as usize]
            .as_ref()
            .map_or(0, Counter::get)
    }

    #[cfg(test)]
    pub(crate) fn uncertain_count(&self, cause: Uncertainty) -> u64 {
        self.uncertain[cause as usize].get()
    }

    #[cfg(test)]
    pub(crate) fn feeder_rejection_count(&self, code: FeederRejection) -> u64 {
        self.feeder_failures
            .as_ref()
            .map_or(0, |f| f.rejections[code as usize].get())
    }

    #[cfg(test)]
    pub(crate) fn import_in_progress(&self) -> i64 {
        self.imports.as_ref().map_or(0, |i| i.in_progress.get())
    }

    #[cfg(test)]
    pub(crate) fn import_result_count(&self, result: ImportResult) -> u64 {
        self.imports
            .as_ref()
            .map_or(0, |i| i.results[result as usize].get())
    }

    #[cfg(test)]
    pub(crate) fn desynchronization_count(&self, cause: Desynchronization) -> u64 {
        self.feeder_failures
            .as_ref()
            .map_or(0, |f| f.desyncs[cause as usize].get())
    }
}

/// Import accounting carried by the import permit from its acquisition.
///
/// Exactly one owner, the HTTP handler during body receipt and then the
/// worker, holds it, and it records everything when dropped: the open phase
/// (unless unwinding or an unfinished `receive`), exactly one terminal
/// result, and then the end of in-progress. The permit holder drops it
/// before releasing the permit, so `in_progress` never exceeds 1. Created
/// only when import metrics exist; otherwise no clock is read.
#[derive(Debug)]
pub(crate) struct ImportObservation {
    telemetry: Arc<ServiceTelemetry>,
    open: Option<(ImportPhase, Instant)>,
    pending: Option<ImportResult>,
    in_worker: bool,
}

impl ImportObservation {
    /// Marks the import in progress and starts `receive`; `None` without
    /// source-managed telemetry.
    pub(crate) fn acquired(telemetry: Option<&Arc<ServiceTelemetry>>) -> Option<Self> {
        let telemetry = telemetry?;
        telemetry.imports.as_ref()?.in_progress.inc();
        Some(Self {
            telemetry: Arc::clone(telemetry),
            open: Some((ImportPhase::Receive, timing_now())),
            pending: None,
            in_worker: false,
        })
    }

    fn imports(&self) -> &ImportMetrics {
        self.telemetry
            .imports
            .as_ref()
            .expect("created only with import metrics")
    }

    fn close(&mut self) {
        if let Some((phase, started)) = self.open.take() {
            let elapsed = timing_now().saturating_duration_since(started);
            self.imports().durations[phase as usize].observe(elapsed.as_secs_f64());
        }
    }

    /// Ends `receive`; an incomplete body is the import's terminal result.
    pub(crate) fn received(&mut self, complete: bool) {
        self.close();
        if !complete {
            self.pending = Some(ImportResult::PayloadRejected);
        }
    }

    /// The worker now owns the import: a panic from here is `worker_failed`.
    pub(crate) fn enter_worker(&mut self) {
        self.in_worker = true;
    }

    /// Starts `prepare`; an error from here is a payload rejection.
    pub(crate) fn begin_prepare(&mut self) {
        self.close();
        self.open = Some((ImportPhase::Prepare, timing_now()));
        self.pending = Some(ImportResult::PayloadRejected);
    }

    /// Ends `prepare` and starts `publish` immediately before the graph-lock
    /// request; an error from here is a publication rejection.
    pub(crate) fn begin_publish(&mut self) {
        self.close();
        self.open = Some((ImportPhase::Publish, timing_now()));
        self.pending = Some(ImportResult::PublicationRejected);
    }

    /// The candidate is live; a later panic does not undo that.
    pub(crate) fn published(&mut self) {
        self.pending = Some(ImportResult::Published);
    }
}

impl Drop for ImportObservation {
    fn drop(&mut self) {
        let unwinding = std::thread::panicking();
        // An unwound phase did not end, and an open `receive` means the
        // caller abandoned the body read: neither is a completed phase.
        if unwinding || matches!(self.open, Some((ImportPhase::Receive, _))) {
            self.open = None;
        }
        self.close();
        let result = match self.pending {
            Some(ImportResult::Published) => ImportResult::Published,
            _ if unwinding && self.in_worker => ImportResult::WorkerFailed,
            Some(result) => result,
            None if !self.in_worker => ImportResult::Abandoned,
            // The worker establishes a result before any non-panicking exit;
            // never report its unclassified end as abandoned.
            None => ImportResult::WorkerFailed,
        };
        let imports = self.imports();
        imports.results[result as usize].inc();
        imports.in_progress.dec();
    }
}

/// A catch-up interval open on the graph: owned by the authoritative graph
/// state, started under the graph lock when an import publishes, and taken
/// under the same lock by the first `synchronized` barrier of that generation.
///
/// Exists only when catch-up metrics do (source-managed mode with metrics
/// enabled); otherwise no clock is read.
#[derive(Debug)]
pub(crate) struct CatchupInterval {
    kind: CatchupKind,
    started: Instant,
}

impl CatchupInterval {
    /// Starts an interval now; `None` without catch-up metrics.
    pub(crate) fn start(telemetry: Option<&ServiceTelemetry>, kind: CatchupKind) -> Option<Self> {
        telemetry?.catchup.as_ref()?;
        Some(Self {
            kind,
            started: timing_now(),
        })
    }

    /// Ends the interval now.
    pub(crate) fn complete(self) -> CompletedCatchup {
        CompletedCatchup {
            kind: self.kind,
            elapsed: timing_now().saturating_duration_since(self.started),
        }
    }
}

/// A finished catch-up interval awaiting observation.
#[derive(Debug)]
pub(crate) struct CompletedCatchup {
    kind: CatchupKind,
    elapsed: Duration,
}

/// Observes a completed catch-up interval when dropped.
///
/// A worker declares it before taking the graph lock, so it drops after the
/// lock is released, also while unwinding: the histogram is never updated under
/// the graph lock, and a completed interval is not lost to a later panic.
#[derive(Debug)]
pub(crate) struct CatchupRecorder {
    telemetry: Option<Arc<ServiceTelemetry>>,
    completed: Option<CompletedCatchup>,
}

impl CatchupRecorder {
    pub(crate) fn new(telemetry: Option<Arc<ServiceTelemetry>>) -> Self {
        Self {
            telemetry,
            completed: None,
        }
    }

    /// Hands over the interval to observe at drop.
    pub(crate) fn record(&mut self, completed: Option<CompletedCatchup>) {
        if completed.is_some() {
            self.completed = completed;
        }
    }
}

impl Drop for CatchupRecorder {
    fn drop(&mut self) {
        if let (Some(telemetry), Some(done)) = (&self.telemetry, self.completed.take())
            && let Some(catchup) = &telemetry.catchup
        {
            catchup.durations[done.kind as usize].observe(done.elapsed.as_secs_f64());
        }
    }
}

#[cfg(not(test))]
#[inline(always)]
fn timing_now() -> Instant {
    Instant::now()
}

/// Offset added to job-phase timestamps in unit tests; separate from the
/// lease clock so timing tests cannot disturb lease expiry.
#[cfg(test)]
static TIMING_OFFSET_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn timing_now() -> Instant {
    Instant::now() + Duration::from_nanos(TIMING_OFFSET_NANOS.load(Ordering::SeqCst))
}

/// Moves the job-phase clock forward; it never moves back.
#[cfg(test)]
pub(crate) fn advance_timing(by: Duration) {
    let nanos = u64::try_from(by.as_nanos()).expect("test step fits in u64 nanoseconds");
    TIMING_OFFSET_NANOS.fetch_add(nanos, Ordering::SeqCst);
}

/// Phase timing of one admitted graph job.
///
/// Created by the caller immediately before `spawn_blocking`, and only when
/// metrics are enabled; disabled telemetry reads no clock. It has no `Drop`,
/// so a job whose worker never starts records nothing rather than a zero.
#[derive(Debug)]
pub(crate) struct JobClock {
    telemetry: Arc<ServiceTelemetry>,
    kind: JobKind,
    submitted: Instant,
}

impl JobClock {
    pub(crate) fn submit(telemetry: Option<&Arc<ServiceTelemetry>>, kind: JobKind) -> Option<Self> {
        telemetry.map(|telemetry| Self {
            telemetry: Arc::clone(telemetry),
            kind,
            submitted: timing_now(),
        })
    }

    /// Records queue time. Workers call this first, before any hook or lock.
    pub(crate) fn start(clock: Option<&Self>) {
        if let Some(clock) = clock {
            let queued = timing_now().saturating_duration_since(clock.submitted);
            if let Some(phases) = clock.telemetry.phases(clock.kind) {
                phases.queue.observe(queued.as_secs_f64());
            }
        }
    }
}

/// Acquires the graph lock exclusively, timing the wait when `clock` is present.
///
/// The returned guard records lock wait and execution only after it has
/// released the lock, on normal drop, early return or unwinding alike, so
/// histogram synchronization stays outside the critical section.
pub(crate) fn lock_timed<'a, T>(
    lock: &'a parking_lot::RwLock<T>,
    clock: Option<&'a JobClock>,
    lifecycle: &Lifecycle,
) -> TimedGuard<'a, parking_lot::RwLockWriteGuard<'a, T>> {
    acquire_timed(clock, lifecycle, || lock.write())
}

/// Acquires the graph lock shared, for read-only jobs; see [`lock_timed`].
///
/// `parking_lot::RwLock` is task-fair: while a writer waits, this call blocks
/// even if only readers hold the lock, so readers cannot starve writers.
pub(crate) fn read_timed<'a, T>(
    lock: &'a parking_lot::RwLock<T>,
    clock: Option<&'a JobClock>,
    lifecycle: &Lifecycle,
) -> TimedGuard<'a, parking_lot::RwLockReadGuard<'a, T>> {
    acquire_timed(clock, lifecycle, || lock.read())
}

fn acquire_timed<'a, G>(
    clock: Option<&'a JobClock>,
    lifecycle: &Lifecycle,
    acquire: impl FnOnce() -> G,
) -> TimedGuard<'a, G> {
    let requested = clock.map(|_| timing_now());
    lifecycle.check_lock_request_hook();
    let guard = acquire();
    let timing = clock.zip(requested).map(|(clock, requested)| {
        let acquired = timing_now();
        LockTiming {
            clock,
            wait: acquired.saturating_duration_since(requested),
            acquired,
        }
    });
    TimedGuard {
        guard: Some(guard),
        timing,
    }
}

struct LockTiming<'a> {
    clock: &'a JobClock,
    wait: Duration,
    acquired: Instant,
}

/// Graph-lock guard from [`lock_timed`] or [`read_timed`].
pub(crate) struct TimedGuard<'a, G> {
    guard: Option<G>,
    timing: Option<LockTiming<'a>>,
}

impl<G: Deref> Deref for TimedGuard<'_, G> {
    type Target = G::Target;

    fn deref(&self) -> &G::Target {
        self.guard.as_deref().expect("guard is held until drop")
    }
}

impl<G: DerefMut> DerefMut for TimedGuard<'_, G> {
    fn deref_mut(&mut self) -> &mut G::Target {
        self.guard.as_deref_mut().expect("guard is held until drop")
    }
}

impl<G> Drop for TimedGuard<'_, G> {
    fn drop(&mut self) {
        drop(self.guard.take());
        if let Some(LockTiming {
            clock,
            wait,
            acquired,
        }) = self.timing.take()
        {
            let held = timing_now().saturating_duration_since(acquired);
            if let Some(phases) = clock.telemetry.phases(clock.kind) {
                phases.lock_wait.observe(wait.as_secs_f64());
                phases.execution.observe(held.as_secs_f64());
            }
        }
    }
}

tokio::task_local! {
    /// Set by `lifecycle::request_timeout` before it drops a timed-out handler.
    static TIMED_OUT: Arc<AtomicBool>;
}

/// Runs `future` with a timeout marker visible to [`PendingOutcome`].
pub(crate) fn mark_timeouts<F: std::future::Future>(
    flag: Arc<AtomicBool>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    TIMED_OUT.scope(flag, future)
}

/// Records a caller that stops waiting for an admitted state-changing job.
///
/// Armed after the job is handed to its worker and disarmed when the worker's
/// result arrives. Dropped while armed, it records `request_timeout` if the
/// timeout middleware marked this request, otherwise `cancelled`.
pub(crate) struct PendingOutcome {
    telemetry: Option<Arc<ServiceTelemetry>>,
    timed_out: Option<Arc<AtomicBool>>,
}

impl PendingOutcome {
    pub(crate) fn arm(telemetry: Option<Arc<ServiceTelemetry>>) -> Self {
        let timed_out = telemetry
            .as_ref()
            .and_then(|_| TIMED_OUT.try_with(Arc::clone).ok());
        Self {
            telemetry,
            timed_out,
        }
    }

    pub(crate) fn disarm(mut self) {
        self.telemetry = None;
    }
}

impl Drop for PendingOutcome {
    fn drop(&mut self) {
        if let Some(telemetry) = self.telemetry.take() {
            let timed_out = self
                .timed_out
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire));
            telemetry.uncertain(if timed_out {
                Uncertainty::RequestTimeout
            } else {
                Uncertainty::Cancelled
            });
        }
    }
}

/// Signed seconds from `now_millis` to a published lease deadline, or `None`
/// when none was published (`0`). Both are whole milliseconds since the
/// process start instant, truncated. The result is positive while
/// `now_millis < deadline_millis`, when no expiry check reports the lease
/// expired. It is zero for the whole deadline millisecond
/// (`now_millis == deadline_millis`): published readiness still treats the
/// lease as live, but worker lease validation, which compares unrounded
/// instants, may already report it expired. It is negative from the next
/// millisecond (`now_millis > deadline_millis`), when every check, published
/// or worker-side, reports it expired.
pub(crate) fn lease_remaining_seconds(deadline_millis: u64, now_millis: u64) -> Option<f64> {
    (deadline_millis != 0).then(|| {
        (i128::from(deadline_millis) - i128::from(now_millis)) as f64 / MILLISECONDS_PER_SECOND
    })
}

/// Service-health gauges sampled at encode time, without the graph mutex or
/// any permit. Each lane's held-permit count is a single atomic load of its
/// semaphore; at most four lanes, so a scrape does constant extra work.
struct HealthCollector {
    service: Service,
    lifecycle: Lifecycle,
}

impl std::fmt::Debug for HealthCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthCollector").finish_non_exhaustive()
    }
}

fn gauge(
    encoder: &mut DescriptorEncoder,
    name: &str,
    help: &str,
    value: i64,
) -> Result<(), std::fmt::Error> {
    let metric = encoder.encode_descriptor(name, help, None, MetricType::Gauge)?;
    ConstGauge::new(value).encode(metric)
}

fn gauge_seconds(
    encoder: &mut DescriptorEncoder,
    name: &str,
    help: &str,
    seconds: f64,
) -> Result<(), std::fmt::Error> {
    let metric = encoder.encode_descriptor(name, help, None, MetricType::Gauge)?;
    ConstGauge::new(seconds).encode(metric)
}

impl Collector for HealthCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let info = self.service.info();
        // One occupancy read per lane, so numerator and capacity of a lane
        // come from the same instant.
        let server_mode = self.service.mode();
        let lanes = Lane::ALL.map(|lane| {
            lane.reachable_in(server_mode).then(|| {
                let (held, capacity) = self.service.lane_occupancy(lane);
                (lane, held, capacity)
            })
        });
        let mut outstanding = encoder.encode_descriptor(
            "knotrel_graph_jobs_outstanding",
            "Admission permits of the lane held by work not yet finished by its blocking worker, including work whose HTTP caller has gone (the import lane from permit acquisition, before body receipt, to the import's end); never above knotrel_graph_job_capacity of the same lane.",
            None,
            MetricType::Gauge,
        )?;
        for (lane, held, _) in lanes.iter().flatten() {
            let labels = [("lane", lane.label())];
            ConstGauge::new(i64::try_from(*held).unwrap_or(i64::MAX))
                .encode(outstanding.encode_family(&labels)?)?;
        }
        let mut capacity = encoder.encode_descriptor(
            "knotrel_graph_job_capacity",
            "Admission permit limit of the lane: shared max_pending_jobs, feeder_data KNOTREL_FEEDER_RESERVED_JOBS, feeder_control 2, import 1.",
            None,
            MetricType::Gauge,
        )?;
        for (lane, _, limit) in lanes.iter().flatten() {
            let labels = [("lane", lane.label())];
            ConstGauge::new(i64::try_from(*limit).unwrap_or(i64::MAX))
                .encode(capacity.encode_family(&labels)?)?;
        }
        gauge(
            &mut encoder,
            "knotrel_service_unavailable",
            "1 after a graph worker failure made the service permanently unavailable, else 0.",
            i64::from(self.service.is_unavailable()),
        )?;

        if self.service.mode() == ServerMode::SourceManaged {
            let now = service::now_to_millis();
            let deadline = self.service.published_readiness().deadline_millis;
            if let Some(remaining) = lease_remaining_seconds(deadline, now) {
                gauge_seconds(
                    &mut encoder,
                    "knotrel_feeder_lease_remaining_seconds",
                    "Seconds until the feeder lease deadline last published under the graph lock, derived at scrape from the monotonic clock without renewing it; positive while no check reports the lease expired, 0 throughout the deadline millisecond (feeder calls may already fail with lease_expired), negative once every check, including /ready, reports it expired; absent until a session is registered.",
                    remaining,
                )?;
            }
            let telemetry = self.service.telemetry();
            if let Some(age) = telemetry.and_then(|t| t.last_applied_age_seconds(now)) {
                gauge_seconds(
                    &mut encoder,
                    "knotrel_feeder_last_applied_age_seconds",
                    "Seconds since a worker last applied a feeder event (including no-op mutations and progress) or published an import, on the monotonic clock; absent until the first; grows while the source is idle, so it measures inactivity, not source lag.",
                    age,
                )?;
            }
        }

        let current = lifecycle::readiness_state(&self.lifecycle, &self.service);
        let states: &[ReadinessState] = match self.service.mode() {
            ServerMode::Standalone => &ReadinessState::STANDALONE,
            ServerMode::SourceManaged => &ReadinessState::SOURCE_MANAGED,
        };
        let mut family = encoder.encode_descriptor(
            "knotrel_readiness_state",
            "1 for the state /ready would report now, by the same precedence, else 0.",
            None,
            MetricType::Gauge,
        )?;
        for state in states {
            let labels = [("state", state.as_str())];
            ConstGauge::new(i64::from(*state == current)).encode(family.encode_family(&labels)?)?;
        }

        let mode = self.service.mode().as_str();
        let mut build = encoder.encode_descriptor(
            "knotrel_build_info",
            "Constant 1 labelled with the server version, graph engine and mode.",
            None,
            MetricType::Gauge,
        )?;
        let labels = [
            ("version", env!("CARGO_PKG_VERSION")),
            ("engine", info.engine),
            ("mode", mode),
        ];
        ConstGauge::new(1_i64).encode(build.encode_family(&labels)?)
    }
}

fn route_label(request: &Request) -> &'static str {
    request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|matched| {
            Route::ALL
                .into_iter()
                .map(Route::as_str)
                .find(|route| *route == matched.as_str())
        })
        .unwrap_or("unmatched")
}

fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::CONNECT => "CONNECT",
        Method::OPTIONS => "OPTIONS",
        Method::TRACE => "TRACE",
        Method::PATCH => "PATCH",
        _ => "other",
    }
}

fn status_label(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// Records exactly one request observation: a status class when a response
/// was constructed, otherwise `cancelled` when the future is dropped first.
struct Observation {
    metrics: Arc<Metrics>,
    route: &'static str,
    method: &'static str,
    pending: bool,
}

impl Observation {
    fn count(&self, class: &'static str) {
        self.metrics
            .requests
            .get_or_create(&[
                ("route", self.route),
                ("method", self.method),
                ("status_class", class),
            ])
            .inc();
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        if self.pending {
            self.count("cancelled");
        }
    }
}

/// Counts and times each request at the HTTP boundary.
///
/// The observation ends when the inner service returns a response; a
/// dropped future is counted as `cancelled` and not timed. Neither outcome
/// says anything about a graph worker that may still be running.
pub(crate) async fn track_http(
    State(metrics): State<Arc<Metrics>>,
    request: Request,
    next: Next,
) -> Response {
    let mut observation = Observation {
        route: route_label(&request),
        method: method_label(request.method()),
        metrics,
        pending: true,
    };
    let start = Instant::now();
    let response = next.run(request).await;
    let elapsed = start.elapsed().as_secs_f64();
    observation.pending = false;
    observation.count(status_label(response.status()));
    observation
        .metrics
        .durations
        .get_or_create(&[("route", observation.route)])
        .observe(elapsed);
    response
}

/// Renders the registry in Prometheus text format 0.0.4.
pub(crate) fn render(metrics: &Metrics) -> Response {
    let mut body = String::new();
    match prometheus_text::encode(&mut body, &metrics.registry) {
        Ok(()) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(prometheus_text::CONTENT_TYPE),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_bounded() {
        assert_eq!(
            method_label(&Method::from_bytes(b"PURGE").unwrap()),
            "other"
        );
        assert_eq!(status_label(StatusCode::from_u16(103).unwrap()), "1xx");
        assert_eq!(status_label(StatusCode::from_u16(308).unwrap()), "3xx");
        assert_eq!(status_label(StatusCode::from_u16(999).unwrap()), "other");
        let request = Request::new(axum::body::Body::empty());
        assert_eq!(route_label(&request), "unmatched");
    }

    #[test]
    fn job_kind_labels_are_unique() {
        let labels: std::collections::BTreeSet<_> =
            JobKind::ALL.iter().map(|kind| kind.label()).collect();
        assert_eq!(labels.len(), JobKind::ALL.len());
        assert!(labels.contains("inspect"));
    }
}
