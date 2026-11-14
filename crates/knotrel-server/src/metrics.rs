//! Opt-in Prometheus exposition (`GET /metrics`), owned per router instance.
//!
//! HTTP accounting lives in [`track_http`]; service-health gauges are read at
//! scrape time by [`HealthCollector`]. Rejection and uncertain-outcome
//! counters, the worker-published graph-size gauges and the worker-phase
//! histograms live in [`ServiceTelemetry`], owned by the `Service` they
//! observe. Nothing here acquires the graph mutex, a graph-job admission
//! permit or an import permit; graph counts are published by workers that
//! already hold the graph lock, and phase timings are recorded by workers
//! after they release it. Scrapes and request
//! accounting do use synchronization internal to the `prometheus-client`
//! registry (metric-family maps and histogram state) and the outstanding-job
//! `watch` channel. Semantics are specified in `docs/observability-contract.md`.

use crate::{
    Lifecycle, ServerMode,
    api::{Operation, Outcome},
    lifecycle,
    service::Service,
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
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Route templates registered by the router; anything else is `unmatched`.
const ROUTES: [&str; 13] = [
    "/health",
    "/ready",
    "/metrics",
    "/v1/info",
    "/v1/stats",
    "/v1/operations",
    "/v1/batch",
    "/v1/feeder/session",
    "/v1/feeder/heartbeat",
    "/v1/feeder/import",
    "/v1/feeder/events",
    "/v1/feeder/gap",
    "/v1/feeder/barrier",
];

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

const STANDALONE_STATES: [&str; 4] = ["unavailable", "draining", "starting", "ready"];
const SOURCE_MANAGED_STATES: [&str; 9] = [
    "unavailable",
    "draining",
    "starting",
    "ready",
    "uninitialized",
    "loading",
    "catching_up",
    "stale",
    "desynchronized",
];

type RequestLabels = [(&'static str, &'static str); 3];
type RouteLabels = [(&'static str, &'static str); 1];
type OutcomeLabels = [(&'static str, &'static str); 1];
type OperationLabels = [(&'static str, &'static str); 2];
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

/// Why a request was refused before any graph job or import ran for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    Busy,
    ImportCapacity,
    ShuttingDown,
    Unavailable,
}

impl Rejection {
    const ALL: [Self; 4] = [
        Self::Busy,
        Self::ImportCapacity,
        Self::ShuttingDown,
        Self::Unavailable,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::ImportCapacity => "import_capacity",
            Self::ShuttingDown => "shutting_down",
            Self::Unavailable => "unavailable",
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
}

impl JobKind {
    const ALL: [Self; 9] = [
        Self::Operation,
        Self::Batch,
        Self::Stats,
        Self::FeederSession,
        Self::FeederHeartbeat,
        Self::FeederEvents,
        Self::FeederGap,
        Self::FeederBarrier,
        Self::ImportPublish,
    ];

    fn label(self) -> &'static str {
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
        }
    }

    /// Whether HTTP can create this job in `mode`; feeder and import
    /// endpoints answer `mode_conflict` in standalone mode before admission.
    fn reachable_in(self, mode: ServerMode) -> bool {
        mode == ServerMode::SourceManaged
            || matches!(self, Self::Operation | Self::Batch | Self::Stats)
    }
}

/// Direct-client graph operation, the `op` label of the operation counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GraphOp {
    AddNode,
    Link,
    Cut,
    Connected,
}

impl GraphOp {
    const ALL: [Self; 4] = [Self::AddNode, Self::Link, Self::Cut, Self::Connected];

    pub(crate) fn of(operation: &Operation) -> Self {
        match operation {
            Operation::AddNode { .. } => Self::AddNode,
            Operation::Link { .. } => Self::Link,
            Operation::Cut { .. } => Self::Cut,
            Operation::Connected { .. } => Self::Connected,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AddNode => "add_node",
            Self::Link => "link",
            Self::Cut => "cut",
            Self::Connected => "connected",
        }
    }

    /// `outcome` labels, indexed by [`outcome_index`].
    fn outcomes(self) -> [&'static str; 3] {
        match self {
            Self::Connected => ["true", "false", "error"],
            _ => ["changed", "unchanged", "error"],
        }
    }

    /// Whether HTTP can execute this operation in `mode`; source-managed
    /// mode refuses direct mutations with `direct_mutation_disabled` before
    /// admission.
    fn reachable_in(self, mode: ServerMode) -> bool {
        mode == ServerMode::Standalone || self == Self::Connected
    }
}

/// Index into [`GraphOp::outcomes`]: changed or `true`, unchanged or `false`, error.
fn outcome_index(outcome: &Outcome) -> usize {
    match outcome {
        Outcome::Changed { changed: true } | Outcome::Connected { connected: true } => 0,
        Outcome::Changed { changed: false } | Outcome::Connected { connected: false } => 1,
        Outcome::Error { .. } => 2,
    }
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
    results: [Counter; 4],
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
/// worker-phase histograms and graph-operation and feeder-event outcome
/// counters for one service instance.
///
/// Owned by `Service` only when metrics are enabled. Every series is created
/// up front, so recording never takes a family lock: counters and gauges are
/// single atomic updates, and a histogram observation takes only that
/// histogram's internal mutex. Holds no reference back to `Service` or the
/// registry.
#[derive(Debug)]
pub(crate) struct ServiceTelemetry {
    rejected_family: Family<OutcomeLabels, Counter>,
    uncertain_family: Family<OutcomeLabels, Counter>,
    rejected: [Counter; 4],
    uncertain: [Counter; 3],
    graph_nodes: Gauge,
    graph_edges: Gauge,
    queue_family: PhaseFamily,
    lock_wait_family: PhaseFamily,
    execution_family: PhaseFamily,
    // Indexed by `JobKind`; `None` for kinds HTTP cannot reach in this mode,
    // whose observations (possible only through direct service calls) are dropped.
    phases: [Option<PhaseHistograms>; 9],
    operations_family: Family<OperationLabels, Counter>,
    // Indexed by `GraphOp`, then `outcome_index`; `None` for operations HTTP
    // cannot execute in this mode.
    operations: [Option<[Counter; 3]>; 4],
    feeder_events: Option<FeederEventCounters>,
}

impl ServiceTelemetry {
    pub(crate) fn new(mode: ServerMode) -> Arc<Self> {
        let rejected_family = Family::<OutcomeLabels, Counter>::default();
        let uncertain_family = Family::<OutcomeLabels, Counter>::default();
        let rejected = Rejection::ALL.map(|reason| {
            rejected_family
                .get_or_create(&[("reason", reason.label())])
                .clone()
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
            kind.reachable_in(mode).then(|| {
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
            op.reachable_in(mode).then(|| {
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
        Arc::new(Self {
            rejected_family,
            uncertain_family,
            rejected,
            uncertain,
            graph_nodes: Gauge::default(),
            graph_edges: Gauge::default(),
            queue_family,
            lock_wait_family,
            execution_family,
            phases,
            operations_family,
            operations,
            feeder_events,
        })
    }

    fn register(&self, registry: &mut Registry) {
        registry.register(
            "knotrel_requests_rejected",
            "Requests refused before any graph job or import ran for them, by refusing boundary; shutting_down includes non-probe requests that would not create a graph job",
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
    }

    /// Publishes the live graph's maintained counts. Callers hold the graph
    /// lock, so publications are ordered and an older job cannot overwrite a
    /// newer one. The two stores are separate: a concurrent scrape may pair
    /// one job's node count with the next job's edge count. A publication is
    /// never rolled back, even if the worker panics afterwards.
    pub(crate) fn publish_graph_counts(&self, nodes: usize, edges: usize) {
        self.graph_nodes
            .set(i64::try_from(nodes).unwrap_or(i64::MAX));
        self.graph_edges
            .set(i64::try_from(edges).unwrap_or(i64::MAX));
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

    pub(crate) fn reject(&self, reason: Rejection) {
        self.rejected[reason as usize].inc();
    }

    pub(crate) fn uncertain(&self, cause: Uncertainty) {
        self.uncertain[cause as usize].inc();
    }

    #[cfg(test)]
    pub(crate) fn rejected_count(&self, reason: Rejection) -> u64 {
        self.rejected[reason as usize].get()
    }

    #[cfg(test)]
    pub(crate) fn uncertain_count(&self, cause: Uncertainty) -> u64 {
        self.uncertain[cause as usize].get()
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

/// Acquires `mutex`, timing the wait when `clock` is present.
///
/// The returned guard records lock wait and execution only after it has
/// released the lock, on normal drop, early return or unwinding alike, so
/// histogram synchronization stays outside the critical section.
pub(crate) fn lock_timed<'a, T>(
    mutex: &'a parking_lot::Mutex<T>,
    clock: Option<&'a JobClock>,
    lifecycle: &Lifecycle,
) -> TimedGuard<'a, T> {
    let requested = clock.map(|_| timing_now());
    lifecycle.check_lock_request_hook();
    let guard = mutex.lock();
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

/// Graph-lock guard from [`lock_timed`].
pub(crate) struct TimedGuard<'a, T> {
    guard: Option<parking_lot::MutexGuard<'a, T>>,
    timing: Option<LockTiming<'a>>,
}

impl<T> Deref for TimedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.as_deref().expect("guard is held until drop")
    }
}

impl<T> DerefMut for TimedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_deref_mut().expect("guard is held until drop")
    }
}

impl<T> Drop for TimedGuard<'_, T> {
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

/// Service-health gauges sampled at encode time, without the graph mutex or
/// any permit. The outstanding-job count is read through its `watch` channel.
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

impl Collector for HealthCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let info = self.service.info();
        gauge(
            &mut encoder,
            "knotrel_graph_jobs_outstanding",
            "Graph jobs admitted and not yet finished by their blocking worker, including jobs whose HTTP caller has gone.",
            i64::try_from(self.lifecycle.outstanding_jobs()).unwrap_or(i64::MAX),
        )?;
        gauge(
            &mut encoder,
            "knotrel_graph_job_capacity",
            "Graph-job admission permit limit (max_pending_jobs).",
            i64::try_from(info.max_pending_jobs).unwrap_or(i64::MAX),
        )?;
        gauge(
            &mut encoder,
            "knotrel_service_unavailable",
            "1 after a graph worker failure made the service permanently unavailable, else 0.",
            i64::from(self.service.is_unavailable()),
        )?;

        let current = lifecycle::readiness_state(&self.lifecycle, &self.service);
        let states: &[&str] = match self.service.mode() {
            ServerMode::Standalone => &STANDALONE_STATES,
            ServerMode::SourceManaged => &SOURCE_MANAGED_STATES,
        };
        let mut family = encoder.encode_descriptor(
            "knotrel_readiness_state",
            "1 for the state /ready would report now, by the same precedence, else 0.",
            None,
            MetricType::Gauge,
        )?;
        for state in states {
            let labels = [("state", *state)];
            ConstGauge::new(i64::from(*state == current)).encode(family.encode_family(&labels)?)?;
        }

        let mode = match self.service.mode() {
            ServerMode::Standalone => "standalone",
            ServerMode::SourceManaged => "source_managed",
        };
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
        .and_then(|matched| ROUTES.iter().find(|route| **route == matched.as_str()))
        .copied()
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
}
