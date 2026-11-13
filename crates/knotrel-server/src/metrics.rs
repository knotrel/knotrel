//! Opt-in Prometheus exposition (`GET /metrics`), owned per router instance.
//!
//! HTTP accounting lives in [`track_http`]; service-health gauges are read at
//! scrape time by [`HealthCollector`]. Nothing here acquires the graph mutex,
//! a graph-job admission permit or an import permit. Scrapes and request
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
        MetricType, counter::Counter, family::Family, gauge::ConstGauge, histogram::Histogram,
    },
    registry::Registry,
};
use std::{sync::Arc, time::Instant};

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
        registry.register_collector(Box::new(HealthCollector { service, lifecycle }));
        Arc::new(Self {
            registry,
            requests,
            durations,
        })
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
