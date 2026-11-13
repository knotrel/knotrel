//! Opt-in Prometheus exposition (`GET /metrics`), owned per router instance.
//!
//! HTTP accounting lives in [`track_http`]; service-health gauges are read at
//! scrape time by [`HealthCollector`]. Rejection and uncertain-outcome
//! counters and the worker-published graph-size gauges live in
//! [`ServiceTelemetry`], owned by the `Service` they observe. Nothing here
//! acquires the graph mutex, a graph-job admission permit or an import
//! permit; graph counts are published by workers that already hold the
//! graph lock. Scrapes and request
//! accounting do use synchronization internal to the `prometheus-client`
//! registry (metric-family maps and histogram state) and the outstanding-job
//! `watch` channel. Semantics are specified in `docs/observability-contract.md`.

use crate::{Lifecycle, ServerMode, lifecycle, service::Service};
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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
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
type DurationFamily = Family<RouteLabels, Histogram, fn() -> Histogram>;

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

/// Rejection and uncertain-outcome counters and graph-size gauges for one
/// service instance.
///
/// Owned by `Service` only when metrics are enabled. Every series is created
/// up front, so recording is a single atomic store or increment with no
/// family lock. Holds no reference back to `Service` or the registry.
#[derive(Debug)]
pub(crate) struct ServiceTelemetry {
    rejected_family: Family<OutcomeLabels, Counter>,
    uncertain_family: Family<OutcomeLabels, Counter>,
    rejected: [Counter; 4],
    uncertain: [Counter; 3],
    graph_nodes: Gauge,
    graph_edges: Gauge,
}

impl ServiceTelemetry {
    pub(crate) fn new() -> Arc<Self> {
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
        Arc::new(Self {
            rejected_family,
            uncertain_family,
            rejected,
            uncertain,
            graph_nodes: Gauge::default(),
            graph_edges: Gauge::default(),
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
