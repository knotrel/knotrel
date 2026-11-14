//! Opt-in Prometheus exposition: configuration, label bounds, scrape
//! availability under saturation, draining and worker failure, and HTTP
//! accounting for completed, timed-out and cancelled requests.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use knotrel_core::EngineConfig;
use knotrel_server::{Lifecycle, ServerConfig, router, router_with_config, router_with_lifecycle};
use std::collections::BTreeMap;
use tower::ServiceExt;

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
const METHODS: [&str; 10] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH", "other",
];
const STATUS_CLASSES: [&str; 7] = ["1xx", "2xx", "3xx", "4xx", "5xx", "other", "cancelled"];
const READINESS_STATES: [&str; 9] = [
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

const REJECTION_REASONS: [&str; 4] = ["busy", "import_capacity", "shutting_down", "unavailable"];
const UNCERTAINTY_CAUSES: [&str; 3] = ["request_timeout", "cancelled", "worker_panic"];
const JOB_KINDS: [&str; 9] = [
    "operation",
    "batch",
    "stats",
    "feeder_session",
    "feeder_heartbeat",
    "feeder_events",
    "feeder_gap",
    "feeder_barrier",
    "import_publish",
];
const GRAPH_OPS: [&str; 4] = ["add_node", "link", "cut", "connected"];
const OPERATION_OUTCOMES: [&str; 5] = ["changed", "unchanged", "true", "false", "error"];
const EVENT_RESULTS: [&str; 4] = ["changed", "unchanged", "duplicate", "rejected"];

fn config(capacity: usize) -> ServerConfig {
    ServerConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        EngineConfig::CompactBfs,
        capacity,
    )
    .unwrap()
    .with_metrics_enabled(true)
}

async fn send(app: &Router, method: Method, path: &str, body: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

const LINK: &str = r#"{"op":"link","source":"1","target":"2"}"#;

#[derive(Debug)]
struct Sample {
    name: String,
    labels: BTreeMap<String, String>,
    value: f64,
}

/// Parses Prometheus text format 0.0.4, panicking on any malformed line.
///
/// Every sample must belong to a family announced by a preceding `# TYPE`
/// line; histogram samples use the `_bucket`, `_sum` and `_count` suffixes.
fn parse(text: &str) -> Vec<Sample> {
    assert!(text.ends_with('\n'), "exposition must end with a newline");
    let mut families: BTreeMap<String, String> = BTreeMap::new();
    let mut samples = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').expect("TYPE line");
            assert!(
                matches!(kind, "counter" | "gauge" | "histogram"),
                "unexpected type {kind}"
            );
            assert!(name.starts_with("knotrel_"), "unprefixed family {name}");
            assert!(families.insert(name.into(), kind.into()).is_none());
            continue;
        }
        if line.starts_with("# HELP ") {
            assert!(!line.ends_with(".."), "doubled HELP punctuation: {line}");
            continue;
        }
        assert!(!line.starts_with('#'), "unexpected comment {line}");
        let (series, value) = line.rsplit_once(' ').expect("sample line");
        let value: f64 = match value {
            "+Inf" => f64::INFINITY,
            other => other.parse().unwrap_or_else(|_| panic!("value in {line}")),
        };
        let (name, labels) = match series.split_once('{') {
            None => (series.to_owned(), BTreeMap::new()),
            Some((name, rest)) => {
                let body = rest.strip_suffix('}').expect("closing brace");
                let mut labels = BTreeMap::new();
                for pair in body.split(',') {
                    let (key, quoted) = pair.split_once('=').expect("label pair");
                    let value = quoted
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .expect("quoted label value");
                    assert!(
                        value.chars().all(|c| c.is_ascii_graphic()),
                        "label value {value:?} needs escaping"
                    );
                    assert!(labels.insert(key.to_owned(), value.to_owned()).is_none());
                }
                (name.to_owned(), labels)
            }
        };
        let family = ["_bucket", "_sum", "_count"]
            .iter()
            .find_map(|suffix| {
                name.strip_suffix(suffix)
                    .filter(|base| families.get(*base).map(String::as_str) == Some("histogram"))
            })
            .unwrap_or(&name);
        assert!(families.contains_key(family), "sample {name} without TYPE");
        samples.push(Sample {
            name,
            labels,
            value,
        });
    }
    samples
}

fn value(samples: &[Sample], name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let wanted: BTreeMap<String, String> = labels
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    samples
        .iter()
        .find(|s| s.name == name && s.labels == wanted)
        .map(|s| s.value)
}

fn requests(samples: &[Sample], route: &str, method: &str, class: &str) -> f64 {
    value(
        samples,
        "knotrel_http_requests_total",
        &[
            ("route", route),
            ("method", method),
            ("status_class", class),
        ],
    )
    .unwrap_or(0.0)
}

fn route_total(samples: &[Sample], route: &str) -> f64 {
    samples
        .iter()
        .filter(|s| {
            s.name == "knotrel_http_requests_total"
                && s.labels.get("route").map(String::as_str) == Some(route)
        })
        .map(|s| s.value)
        .sum()
}

fn duration_count(samples: &[Sample], route: &str) -> f64 {
    value(
        samples,
        "knotrel_http_request_duration_seconds_count",
        &[("route", route)],
    )
    .unwrap_or(0.0)
}

fn readiness(samples: &[Sample]) -> Vec<&str> {
    samples
        .iter()
        .filter(|s| s.name == "knotrel_readiness_state" && s.value == 1.0)
        .map(|s| s.labels["state"].as_str())
        .collect()
}

async fn scrape(app: &Router) -> Vec<Sample> {
    let response = app
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    parse(std::str::from_utf8(&bytes).unwrap())
}

#[tokio::test]
async fn metrics_route_is_absent_when_disabled() {
    let disabled =
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4).unwrap();
    assert!(!disabled.metrics_enabled());
    for app in [
        router(knotrel_core::Graph::new()),
        router_with_config(disabled),
    ] {
        let (status, body) = send(&app, Method::GET, "/metrics", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains("knotrel_"));
    }
}

#[tokio::test]
async fn exposition_is_valid_and_uses_only_bounded_labels() {
    let app = router_with_config(config(4));
    send(&app, Method::POST, "/v1/operations", LINK).await;
    send(&app, Method::GET, "/v1/nodes/12345/neighbors", "").await;
    send(&app, Method::GET, "/v1/info?node=987654", "").await;
    let purge = Method::from_bytes(b"PURGE").unwrap();
    let (status, _) = send(&app, purge.clone(), "/v1/info", "").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    send(&app, purge, "/no/such/route", "").await;

    let samples = scrape(&app).await;
    for sample in &samples {
        for (key, label) in &sample.labels {
            let allowed: &[&str] = match key.as_str() {
                "route" => &ROUTES,
                "method" => &METHODS,
                "status_class" => &STATUS_CLASSES,
                "state" => &READINESS_STATES,
                "reason" => &REJECTION_REASONS,
                "cause" => &UNCERTAINTY_CAUSES,
                "kind" => &JOB_KINDS,
                "op" => &GRAPH_OPS,
                "outcome" => &OPERATION_OUTCOMES,
                "result" => &EVENT_RESULTS,
                "le" => &[],
                "version" | "engine" | "mode" => &[],
                other => panic!("unexpected label {other}"),
            };
            if key == "route" {
                assert!(
                    allowed.contains(&label.as_str()) || label == "unmatched",
                    "route {label}"
                );
            } else if !allowed.is_empty() {
                assert!(allowed.contains(&label.as_str()), "{key}={label}");
            }
        }
    }
    assert_eq!(requests(&samples, "/v1/operations", "POST", "2xx"), 1.0);
    assert_eq!(requests(&samples, "unmatched", "GET", "4xx"), 1.0);
    assert_eq!(requests(&samples, "/v1/info", "GET", "2xx"), 1.0);
    assert_eq!(requests(&samples, "/v1/info", "other", "4xx"), 1.0);
    assert_eq!(requests(&samples, "unmatched", "other", "4xx"), 1.0);
    let text = format!("{samples:?}");
    for raw in ["12345", "987654", "PURGE", "/no/such/route"] {
        assert!(!text.contains(raw), "raw value {raw} leaked into labels");
    }

    assert_eq!(
        value(&samples, "knotrel_graph_jobs_outstanding", &[]),
        Some(0.0)
    );
    assert_eq!(
        value(&samples, "knotrel_graph_job_capacity", &[]),
        Some(4.0)
    );
    assert_eq!(
        value(&samples, "knotrel_service_unavailable", &[]),
        Some(0.0)
    );
    assert_eq!(readiness(&samples), ["ready"]);
    let states: Vec<_> = samples
        .iter()
        .filter(|s| s.name == "knotrel_readiness_state")
        .map(|s| s.labels["state"].as_str())
        .collect();
    assert_eq!(states, ["unavailable", "draining", "starting", "ready"]);
    let build = value(
        &samples,
        "knotrel_build_info",
        &[
            ("version", env!("CARGO_PKG_VERSION")),
            ("engine", "compact-bfs"),
            ("mode", "standalone"),
        ],
    );
    assert_eq!(build, Some(1.0));
    let buckets = samples
        .iter()
        .filter(|s| {
            s.name == "knotrel_http_request_duration_seconds_bucket"
                && s.labels["route"] == "/v1/operations"
        })
        .count();
    assert_eq!(buckets, 16, "15 explicit buckets plus +Inf");
}

#[tokio::test]
async fn capacity_help_describes_the_admission_permit_limit() {
    let app = router_with_config(config(4));
    let (status, body) = send(&app, Method::GET, "/metrics", "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.lines().any(|line| line
            == "# HELP knotrel_graph_job_capacity Graph-job admission permit limit (max_pending_jobs)."),
        "capacity HELP line missing or changed"
    );
}

#[tokio::test]
async fn every_registered_route_is_labelled_with_its_template() {
    let app = router_with_config(config(4).with_mode(knotrel_server::ServerMode::SourceManaged));
    for route in ROUTES {
        send(&app, Method::GET, route, "").await;
    }
    let samples = scrape(&app).await;
    for route in ROUTES {
        assert!(route_total(&samples, route) >= 1.0, "{route} not counted");
    }
    assert_eq!(route_total(&samples, "unmatched"), 0.0);
    let states: Vec<_> = samples
        .iter()
        .filter(|s| s.name == "knotrel_readiness_state")
        .map(|s| s.labels["state"].as_str())
        .collect();
    assert_eq!(states, READINESS_STATES);
    assert_eq!(readiness(&samples), ["uninitialized"]);
}

#[tokio::test]
async fn a_scrape_is_counted_after_it_completes() {
    let app = router_with_config(config(4));
    let first = scrape(&app).await;
    assert_eq!(route_total(&first, "/metrics"), 0.0);
    let second = scrape(&app).await;
    assert_eq!(requests(&second, "/metrics", "GET", "2xx"), 1.0);
    assert_eq!(duration_count(&second, "/metrics"), 1.0);
}

#[tokio::test]
async fn completed_request_is_counted_once_with_duration() {
    let app = router_with_config(config(4));
    let (status, _) = send(&app, Method::POST, "/v1/operations", LINK).await;
    assert_eq!(status, StatusCode::OK);
    let samples = scrape(&app).await;
    assert_eq!(route_total(&samples, "/v1/operations"), 1.0);
    assert_eq!(requests(&samples, "/v1/operations", "POST", "2xx"), 1.0);
    assert_eq!(duration_count(&samples, "/v1/operations"), 1.0);
    let inf = value(
        &samples,
        "knotrel_http_request_duration_seconds_bucket",
        &[("route", "/v1/operations"), ("le", "+Inf")],
    );
    assert_eq!(inf, Some(1.0));
}

#[tokio::test]
async fn scrape_is_served_while_draining() {
    let lifecycle = Lifecycle::new();
    let app = router_with_lifecycle(config(4), lifecycle.clone());
    lifecycle.mark_ready();
    lifecycle.begin_draining();

    let response = app
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONNECTION], "close");
    let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("shutting_down"));

    let samples = scrape(&app).await;
    assert_eq!(readiness(&samples), ["draining"]);
    assert_eq!(requests(&samples, "/v1/operations", "POST", "5xx"), 1.0);
}

#[tokio::test]
async fn disabled_metrics_path_is_not_a_probe_while_draining() {
    let lifecycle = Lifecycle::new();
    lifecycle.mark_ready();
    let disabled =
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4).unwrap();
    let app = router_with_lifecycle(disabled, lifecycle.clone());
    lifecycle.begin_draining();
    let (status, body) = send(&app, Method::GET, "/metrics", "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("shutting_down"));
}

#[tokio::test]
async fn scrape_reports_starting_before_the_listener_is_ready() {
    let lifecycle = Lifecycle::new();
    let app = router_with_lifecycle(config(4), lifecycle);
    assert_eq!(readiness(&scrape(&app).await), ["starting"]);
}

#[tokio::test]
async fn server_instances_have_isolated_registries() {
    let first = router_with_config(config(4));
    let second = router_with_config(config(7));
    send(&first, Method::POST, "/v1/operations", LINK).await;
    send(&first, Method::POST, "/v1/operations", LINK).await;
    let a = scrape(&first).await;
    let b = scrape(&second).await;
    assert_eq!(route_total(&a, "/v1/operations"), 2.0);
    assert_eq!(route_total(&b, "/v1/operations"), 0.0);
    assert_eq!(value(&b, "knotrel_graph_job_capacity", &[]), Some(7.0));
    let cloned = first.clone();
    assert_eq!(route_total(&scrape(&cloned).await, "/v1/operations"), 2.0);
}

#[cfg(feature = "test-support")]
mod paused {
    use super::*;
    use std::time::Duration;

    async fn wait_idle(lifecycle: &Lifecycle) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while lifecycle.outstanding_jobs() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn scrape_is_served_while_graph_jobs_are_saturated() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        let app = router_with_lifecycle(config(1), lifecycle.clone());
        let mut pause = lifecycle.pause_next_worker();
        let held = tokio::spawn({
            let app = app.clone();
            async move { send(&app, Method::POST, "/v1/operations", LINK).await }
        });
        pause.wait_started().await;

        let (status, body) = send(&app, Method::GET, "/v1/stats", "").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("busy"));
        let samples = scrape(&app).await;
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(1.0)
        );
        assert_eq!(
            value(&samples, "knotrel_graph_job_capacity", &[]),
            Some(1.0)
        );
        assert_eq!(requests(&samples, "/v1/stats", "GET", "5xx"), 1.0);
        assert_eq!(route_total(&samples, "/v1/operations"), 0.0);

        pause.release();
        assert_eq!(held.await.unwrap().0, StatusCode::OK);
        let samples = scrape(&app).await;
        assert_eq!(requests(&samples, "/v1/operations", "POST", "2xx"), 1.0);
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(0.0)
        );
    }

    #[tokio::test]
    async fn scrape_reports_worker_failure_as_unavailable() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        let app = router_with_lifecycle(config(4), lifecycle.clone());
        let mut pause = lifecycle.pause_next_worker();
        let failed = tokio::spawn({
            let app = app.clone();
            async move { send(&app, Method::POST, "/v1/operations", LINK).await }
        });
        pause.wait_started().await;
        pause.panic_on_release();
        assert_eq!(failed.await.unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
        wait_idle(&lifecycle).await;

        let samples = scrape(&app).await;
        assert_eq!(
            value(&samples, "knotrel_service_unavailable", &[]),
            Some(1.0)
        );
        assert_eq!(readiness(&samples), ["unavailable"]);
        assert_eq!(requests(&samples, "/v1/operations", "POST", "5xx"), 1.0);
    }

    #[tokio::test]
    async fn timed_out_request_is_completed_while_its_worker_stays_outstanding() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        let app = router_with_lifecycle(
            config(4)
                .with_request_timeout(Duration::from_millis(50))
                .unwrap(),
            lifecycle.clone(),
        );
        let mut pause = lifecycle.pause_next_worker();
        let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("request_timeout"));
        pause.wait_started().await;

        let samples = scrape(&app).await;
        assert_eq!(requests(&samples, "/v1/operations", "POST", "5xx"), 1.0);
        assert_eq!(route_total(&samples, "/v1/operations"), 1.0);
        assert_eq!(duration_count(&samples, "/v1/operations"), 1.0);
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(1.0)
        );

        pause.release();
        wait_idle(&lifecycle).await;
        let samples = scrape(&app).await;
        assert_eq!(route_total(&samples, "/v1/operations"), 1.0);
        assert_eq!(duration_count(&samples, "/v1/operations"), 1.0);
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(0.0)
        );
    }

    #[tokio::test]
    async fn cancelled_request_future_is_counted_once_without_a_status() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        let app = router_with_lifecycle(config(4), lifecycle.clone());
        let mut pause = lifecycle.pause_next_worker();
        let request = tokio::spawn({
            let app = app.clone();
            async move { send(&app, Method::POST, "/v1/operations", LINK).await }
        });
        pause.wait_started().await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());

        let samples = scrape(&app).await;
        assert_eq!(
            requests(&samples, "/v1/operations", "POST", "cancelled"),
            1.0
        );
        assert_eq!(route_total(&samples, "/v1/operations"), 1.0);
        assert_eq!(duration_count(&samples, "/v1/operations"), 0.0);
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(1.0)
        );

        pause.release();
        wait_idle(&lifecycle).await;
        let samples = scrape(&app).await;
        assert_eq!(route_total(&samples, "/v1/operations"), 1.0);
        assert_eq!(
            requests(&samples, "/v1/operations", "POST", "cancelled"),
            1.0
        );
        assert_eq!(
            value(&samples, "knotrel_graph_jobs_outstanding", &[]),
            Some(0.0)
        );
    }
}

mod outcomes {
    use super::*;

    const REJECTED: &str = "knotrel_requests_rejected_total";
    const UNCERTAIN: &str = "knotrel_requests_outcome_uncertain_total";

    /// Asserts every rejection and uncertainty series, treating unlisted ones as zero.
    fn assert_outcomes(samples: &[Sample], nonzero: &[(&str, f64)]) {
        let expected = |key: &str| {
            nonzero
                .iter()
                .find(|(k, _)| *k == key)
                .map_or(0.0, |(_, v)| *v)
        };
        for reason in REJECTION_REASONS {
            let actual = value(samples, REJECTED, &[("reason", reason)])
                .unwrap_or_else(|| panic!("missing {REJECTED}{{reason={reason}}}"));
            assert_eq!(actual, expected(reason), "rejected reason={reason}");
        }
        for cause in UNCERTAINTY_CAUSES {
            let actual = value(samples, UNCERTAIN, &[("cause", cause)])
                .unwrap_or_else(|| panic!("missing {UNCERTAIN}{{cause={cause}}}"));
            assert_eq!(actual, expected(cause), "uncertain cause={cause}");
        }
    }

    #[tokio::test]
    async fn outcome_series_exist_at_zero_in_both_modes() {
        for config in [
            config(4),
            config(4).with_mode(knotrel_server::ServerMode::SourceManaged),
        ] {
            let app = router_with_config(config);
            let text = send(&app, Method::GET, "/metrics", "").await.1;
            assert!(text.contains(&format!("# TYPE {REJECTED} counter")));
            assert!(text.contains(&format!("# TYPE {UNCERTAIN} counter")));
            assert_outcomes(&scrape(&app).await, &[]);
        }
    }

    #[tokio::test]
    async fn shutdown_rejection_is_counted_once_and_probes_are_not() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        let app = router_with_lifecycle(config(4), lifecycle.clone());
        lifecycle.begin_draining();
        let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("shutting_down"));
        send(&app, Method::GET, "/ready", "").await;
        send(&app, Method::GET, "/health", "").await;
        assert_outcomes(&scrape(&app).await, &[("shutting_down", 1.0)]);
    }

    #[tokio::test]
    async fn validation_and_domain_errors_are_not_rejections() {
        let app = router_with_config(config(4));
        let (status, _) = send(&app, Method::POST, "/v1/operations", "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let cut = r#"{"operations":[{"op":"cut","source":"1","target":"2"},{"op":"connected","source":"1","target":"2"}]}"#;
        let (status, _) = send(&app, Method::POST, "/v1/batch", cut).await;
        assert_eq!(status, StatusCode::OK);
        assert_outcomes(&scrape(&app).await, &[]);
    }

    #[tokio::test]
    async fn server_instances_count_outcomes_independently() {
        let draining = Lifecycle::new();
        draining.mark_ready();
        let first = router_with_lifecycle(config(4), draining.clone());
        let ready = Lifecycle::new();
        ready.mark_ready();
        let second = router_with_lifecycle(config(4), ready);
        draining.begin_draining();
        send(&first, Method::POST, "/v1/operations", LINK).await;
        send(&second, Method::POST, "/v1/operations", LINK).await;
        assert_outcomes(&scrape(&first).await, &[("shutting_down", 1.0)]);
        assert_outcomes(&scrape(&second).await, &[]);
    }

    #[cfg(feature = "test-support")]
    mod paused {
        use super::*;
        use std::time::Duration;

        const CONNECTED: &str = r#"{"op":"connected","source":"1","target":"2"}"#;

        async fn wait_idle(lifecycle: &Lifecycle) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lifecycle.outstanding_jobs() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        fn source_managed(capacity: usize) -> ServerConfig {
            config(capacity).with_mode(knotrel_server::ServerMode::SourceManaged)
        }

        fn timed(capacity: usize) -> ServerConfig {
            config(capacity)
                .with_request_timeout(Duration::from_millis(50))
                .unwrap()
        }

        async fn instance_id(app: &Router) -> String {
            let (_, body) = send(app, Method::GET, "/v1/info", "").await;
            let info: serde_json::Value = serde_json::from_str(&body).unwrap();
            info["instance_id"].as_str().unwrap().to_owned()
        }

        async fn send_feeder(
            app: &Router,
            instance: &str,
            path: &str,
            body: String,
        ) -> (StatusCode, String) {
            let response = app
                .clone()
                .oneshot(
                    Request::post(path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-knotrel-instance-id", instance)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            (status, String::from_utf8(bytes.to_vec()).unwrap())
        }

        fn gap_body(instance: &str) -> String {
            serde_json::json!({
                "instance_id": instance,
                "graph_id": "default",
                "generation": 1,
                "feeder_session_id": "s",
                "feeder_epoch": 1,
                "failed_delivery_sequence": 1,
            })
            .to_string()
        }

        #[tokio::test]
        async fn busy_rejection_is_counted_once_through_service_and_api_layers() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(config(1), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let held = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;

            let (status, body) = send(
                &app,
                Method::POST,
                "/v1/batch",
                &format!(r#"{{"operations":[{LINK}]}}"#),
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"busy\""));
            assert_outcomes(&scrape(&app).await, &[("busy", 1.0)]);

            pause.release();
            assert_eq!(held.await.unwrap().0, StatusCode::OK);
            wait_idle(&lifecycle).await;
            assert_outcomes(&scrape(&app).await, &[("busy", 1.0)]);
        }

        #[tokio::test]
        async fn feeder_admission_rejection_keeps_import_in_progress_and_counts_busy() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(source_managed(1), lifecycle.clone());
            let instance = instance_id(&app).await;
            let mut pause = lifecycle.pause_next_worker();
            let held = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::GET, "/v1/stats", "").await }
            });
            pause.wait_started().await;

            let (status, body) =
                send_feeder(&app, &instance, "/v1/feeder/gap", gap_body(&instance)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("import_in_progress"), "{body}");
            assert_outcomes(&scrape(&app).await, &[("busy", 1.0)]);

            pause.release();
            assert_eq!(held.await.unwrap().0, StatusCode::OK);
        }

        #[tokio::test]
        async fn import_permit_rejection_is_counted_as_import_capacity() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(source_managed(4), lifecycle.clone());
            let instance = instance_id(&app).await;
            let mut pause = lifecycle.pause_next_worker();
            let held = tokio::spawn({
                let app = app.clone();
                let instance = instance.clone();
                async move { send_feeder(&app, &instance, "/v1/feeder/import", "{}".into()).await }
            });
            pause.wait_started().await;

            let (status, body) =
                send_feeder(&app, &instance, "/v1/feeder/import", "{}".into()).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("import_in_progress"), "{body}");
            assert_outcomes(&scrape(&app).await, &[("import_capacity", 1.0)]);

            pause.release();
            assert_eq!(held.await.unwrap().0, StatusCode::UNPROCESSABLE_ENTITY);
            wait_idle(&lifecycle).await;
            assert_outcomes(&scrape(&app).await, &[("import_capacity", 1.0)]);
        }

        #[tokio::test]
        async fn timeout_with_outstanding_worker_then_success_counts_one_observation() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(timed(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("request_timeout"));
            pause.wait_started().await;
            assert_outcomes(&scrape(&app).await, &[("request_timeout", 1.0)]);

            pause.release();
            wait_idle(&lifecycle).await;
            let samples = scrape(&app).await;
            assert_outcomes(&samples, &[("request_timeout", 1.0)]);
            assert_eq!(
                value(&samples, "knotrel_service_unavailable", &[]),
                Some(0.0)
            );
        }

        #[tokio::test]
        async fn worker_panic_after_caller_cancellation_is_counted() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(config(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let request = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            assert_outcomes(&scrape(&app).await, &[("cancelled", 1.0)]);

            pause.panic_on_release();
            wait_idle(&lifecycle).await;
            assert_outcomes(
                &scrape(&app).await,
                &[("cancelled", 1.0), ("worker_panic", 1.0)],
            );
        }

        #[tokio::test]
        async fn timeout_followed_by_worker_panic_counts_two_observations() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(timed(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let (status, _) = send(&app, Method::POST, "/v1/operations", LINK).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            pause.wait_started().await;
            pause.panic_on_release();
            wait_idle(&lifecycle).await;
            assert_outcomes(
                &scrape(&app).await,
                &[("request_timeout", 1.0), ("worker_panic", 1.0)],
            );
        }

        #[tokio::test]
        async fn read_only_timeout_and_cancellation_are_not_uncertain() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(timed(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let (status, body) = send(&app, Method::GET, "/v1/stats", "").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("request_timeout"));
            pause.wait_started().await;
            pause.release();
            wait_idle(&lifecycle).await;

            let mut pause = lifecycle.pause_next_worker();
            let request = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", CONNECTED).await }
            });
            pause.wait_started().await;
            request.abort();
            assert!(request.await.unwrap_err().is_cancelled());
            pause.release();
            wait_idle(&lifecycle).await;

            let samples = scrape(&app).await;
            assert_outcomes(&samples, &[]);
            assert_eq!(
                requests(&samples, "/v1/operations", "POST", "cancelled"),
                1.0
            );
        }

        #[tokio::test]
        async fn worker_panic_is_not_a_rejection_but_later_refusals_are() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(config(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let failed = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;
            pause.panic_on_release();
            let (status, body) = failed.await.unwrap();
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"unavailable\""));
            wait_idle(&lifecycle).await;
            assert_outcomes(&scrape(&app).await, &[("worker_panic", 1.0)]);

            let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"unavailable\""));
            send(&app, Method::GET, "/v1/stats", "").await;
            assert_outcomes(
                &scrape(&app).await,
                &[("worker_panic", 1.0), ("unavailable", 2.0)],
            );
        }

        #[tokio::test]
        async fn read_only_panic_then_feeder_refusal_is_counted_once() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(source_managed(4), lifecycle.clone());
            let instance = instance_id(&app).await;
            let mut pause = lifecycle.pause_next_worker();
            let failed = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::GET, "/v1/stats", "").await }
            });
            pause.wait_started().await;
            pause.panic_on_release();
            assert_eq!(failed.await.unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
            wait_idle(&lifecycle).await;
            let samples = scrape(&app).await;
            assert_outcomes(&samples, &[]);
            assert_eq!(
                value(&samples, "knotrel_service_unavailable", &[]),
                Some(1.0)
            );

            let (status, body) =
                send_feeder(&app, &instance, "/v1/feeder/gap", gap_body(&instance)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"unavailable\""), "{body}");
            assert_outcomes(&scrape(&app).await, &[("unavailable", 1.0)]);
        }
    }
}

mod graph_counts {
    use super::*;

    const NODES: &str = "knotrel_graph_nodes";
    const EDGES: &str = "knotrel_graph_edges";

    fn counts(samples: &[Sample]) -> (f64, f64) {
        (
            value(samples, NODES, &[]).unwrap_or_else(|| panic!("missing {NODES}")),
            value(samples, EDGES, &[]).unwrap_or_else(|| panic!("missing {EDGES}")),
        )
    }

    async fn op(app: &Router, body: &str) -> StatusCode {
        send(app, Method::POST, "/v1/operations", body).await.0
    }

    pub(super) fn source_managed() -> ServerConfig {
        config(4).with_mode(knotrel_server::ServerMode::SourceManaged)
    }

    pub(super) async fn instance_id(app: &Router) -> String {
        let (_, body) = send(app, Method::GET, "/v1/info", "").await;
        let info: serde_json::Value = serde_json::from_str(&body).unwrap();
        info["instance_id"].as_str().unwrap().to_owned()
    }

    pub(super) async fn feeder(
        app: &Router,
        instance: &str,
        path: &str,
        if_match: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-knotrel-instance-id", instance);
        if let Some(etag) = if_match {
            request = request.header(header::IF_MATCH, etag);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    pub(super) fn import(
        instance: &str,
        mode: &str,
        position: &str,
        nodes: &[&str],
        edges: &[(&str, &str)],
    ) -> serde_json::Value {
        let edges: Vec<_> = edges
            .iter()
            .map(|(s, t)| serde_json::json!({"source": s, "target": t}))
            .collect();
        serde_json::json!({
            "mode": mode,
            "instance_id": instance,
            "graph_id": "default",
            "source_id": "pg-1",
            "source_epoch": "ep-1",
            "source_position": position,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "nodes": nodes,
            "edges": edges,
        })
    }

    pub(super) fn events(
        instance: &str,
        generation: u64,
        events: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "instance_id": instance,
            "graph_id": "default",
            "generation": generation,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "source_id": "pg-1",
            "source_epoch": "ep-1",
            "events": events,
        })
    }

    pub(super) fn event(seq: u64, fields: serde_json::Value) -> serde_json::Value {
        let mut event = serde_json::json!({
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

    /// Registers the session and publishes a create-only baseline at `p-0`.
    pub(super) async fn bootstrap(app: &Router, nodes: &[&str], edges: &[(&str, &str)]) -> String {
        let instance = instance_id(app).await;
        let session = serde_json::json!({
            "instance_id": instance,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "lease_duration_ms": 60_000,
        });
        let (status, body) = feeder(app, &instance, "/v1/feeder/session", None, session).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let body = import(&instance, "create_only", "p-0", nodes, edges);
        let (status, body) = feeder(app, &instance, "/v1/feeder/import", None, body).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        instance
    }

    #[tokio::test]
    async fn fresh_server_reports_an_empty_graph_in_both_modes() {
        for config in [config(4), source_managed()] {
            let app = router_with_config(config);
            let text = send(&app, Method::GET, "/metrics", "").await.1;
            assert!(text.contains(&format!("# TYPE {NODES} gauge")), "{text}");
            assert!(text.contains(&format!("# TYPE {EDGES} gauge")), "{text}");
            assert_eq!(counts(&scrape(&app).await), (0.0, 0.0));
        }
    }

    #[tokio::test]
    async fn effective_operations_change_counts_and_no_ops_do_not() {
        let app = router_with_config(config(4));
        let add = r#"{"op":"add_node","node":"1"}"#;
        let cut = r#"{"op":"cut","source":"1","target":"2"}"#;
        let connected = r#"{"op":"connected","source":"1","target":"2"}"#;
        let steps = [
            (add, (1.0, 0.0)),
            (add, (1.0, 0.0)),
            (LINK, (2.0, 1.0)),
            (LINK, (2.0, 1.0)),
            (connected, (2.0, 1.0)),
            (cut, (2.0, 0.0)),
            (cut, (2.0, 0.0)),
        ];
        for (body, expected) in steps {
            assert_eq!(op(&app, body).await, StatusCode::OK, "{body}");
            assert_eq!(counts(&scrape(&app).await), expected, "after {body}");
        }
        send(&app, Method::GET, "/v1/stats", "").await;
        assert_eq!(counts(&scrape(&app).await), (2.0, 0.0));
    }

    #[tokio::test]
    async fn legacy_batch_keeps_successful_operations_around_a_domain_error() {
        let app = router_with_config(config(4));
        let batch = r#"{"operations":[
            {"op":"link","source":"1","target":"2"},
            {"op":"link","source":"3","target":"3"},
            {"op":"add_node","node":"4"}
        ]}"#;
        let (status, body) = send(&app, Method::POST, "/v1/batch", batch).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("self_loop"), "{body}");
        assert_eq!(counts(&scrape(&app).await), (3.0, 1.0));
    }

    #[tokio::test]
    async fn feeder_events_publish_effective_changes_and_partial_prefixes() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1", "2", "3"], &[("1", "2")]).await;
        assert_eq!(counts(&scrape(&app).await), (3.0, 1.0));

        let first = serde_json::json!([
            event(
                1,
                serde_json::json!({"op": "link", "source": "2", "target": "3"})
            ),
            event(2, serde_json::json!({"op": "progress"})),
        ]);
        let body = events(&instance, 1, first.clone());
        let (status, reply) = feeder(&app, &instance, "/v1/feeder/events", None, body).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(counts(&scrape(&app).await), (3.0, 2.0));

        let body = events(&instance, 1, first);
        let (status, reply) = feeder(&app, &instance, "/v1/feeder/events", None, body).await;
        assert_eq!(status, StatusCode::OK, "duplicate: {reply}");
        assert_eq!(counts(&scrape(&app).await), (3.0, 2.0));

        let partial = serde_json::json!([
            event(
                3,
                serde_json::json!({"op": "cut", "source": "1", "target": "2"})
            ),
            event(4, serde_json::json!({"op": "add_node", "node": "9"})),
            event(
                5,
                serde_json::json!({"op": "link", "source": "3", "target": "3"})
            ),
        ]);
        let body = events(&instance, 1, partial);
        let (status, reply) = feeder(&app, &instance, "/v1/feeder/events", None, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reply}");
        assert_eq!(reply["result"], "partial_failure");
        assert_eq!(reply["applied_delivery_sequence"], 4);
        assert_eq!(counts(&scrape(&app).await), (4.0, 1.0));
    }

    #[tokio::test]
    async fn only_a_published_replacement_import_changes_counts() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1", "2"], &[("1", "2")]).await;
        assert_eq!(counts(&scrape(&app).await), (2.0, 1.0));

        let candidate = import(
            &instance,
            "replace",
            "p-9",
            &["10", "20", "30", "40"],
            &[("10", "20"), ("20", "30")],
        );
        let (status, reply) = feeder(
            &app,
            &instance,
            "/v1/feeder/import",
            Some("\"1.0.wrong\""),
            candidate.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{reply}");
        assert_eq!(counts(&scrape(&app).await), (2.0, 1.0));

        let (status, reply) = feeder(
            &app,
            &instance,
            "/v1/feeder/import",
            Some("\"1.0.p-0\""),
            candidate,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
        assert_eq!(
            (reply["node_count"].clone(), reply["edge_count"].clone()),
            (4.into(), 2.into())
        );
        assert_eq!(counts(&scrape(&app).await), (4.0, 2.0));
    }

    #[tokio::test]
    async fn server_instances_report_their_own_graph() {
        let first = router_with_config(config(4));
        let second = router_with_config(config(4));
        assert_eq!(op(&first, LINK).await, StatusCode::OK);
        assert_eq!(counts(&scrape(&first).await), (2.0, 1.0));
        assert_eq!(counts(&scrape(&second).await), (0.0, 0.0));
    }

    #[cfg(feature = "test-support")]
    mod paused {
        use super::*;
        use std::time::Duration;

        async fn wait_idle(lifecycle: &Lifecycle) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lifecycle.outstanding_jobs() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_timed_out_publishes_counts() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let timed = config(4)
                .with_request_timeout(Duration::from_millis(50))
                .unwrap();
            let app = router_with_lifecycle(timed, lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("request_timeout"), "{body}");
            pause.wait_started().await;
            assert_eq!(counts(&scrape(&app).await), (0.0, 0.0));

            pause.release();
            wait_idle(&lifecycle).await;
            assert_eq!(counts(&scrape(&app).await), (2.0, 1.0));
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_was_cancelled_publishes_counts() {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            let app = router_with_lifecycle(config(4), lifecycle.clone());
            let mut pause = lifecycle.pause_next_worker();
            let caller = tokio::spawn({
                let app = app.clone();
                async move { op(&app, LINK).await }
            });
            pause.wait_started().await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert_eq!(counts(&scrape(&app).await), (0.0, 0.0));

            pause.release();
            wait_idle(&lifecycle).await;
            assert_eq!(counts(&scrape(&app).await), (2.0, 1.0));
        }
    }
}

mod job_timing {
    use super::graph_counts::{bootstrap, event, events, feeder, import, source_managed};
    use super::*;

    const PHASES: [&str; 3] = [
        "knotrel_graph_job_queue_seconds",
        "knotrel_graph_lock_wait_seconds",
        "knotrel_graph_job_execution_seconds",
    ];
    const STANDALONE_KINDS: [&str; 3] = ["operation", "batch", "stats"];

    /// Observation count of the queue, lock-wait and execution histograms
    /// for `kind`, or `None` where the series does not exist.
    fn phases(samples: &[Sample], kind: &str) -> [Option<f64>; 3] {
        PHASES.map(|phase| value(samples, &format!("{phase}_count"), &[("kind", kind)]))
    }

    fn all(count: f64) -> [Option<f64>; 3] {
        [Some(count); 3]
    }

    #[cfg(feature = "test-support")]
    fn queue_only(count: f64) -> [Option<f64>; 3] {
        [Some(count), Some(0.0), Some(0.0)]
    }

    fn timing_samples(samples: &[Sample]) -> usize {
        samples
            .iter()
            .filter(|s| PHASES.iter().any(|phase| s.name.starts_with(phase)))
            .count()
    }

    #[tokio::test]
    async fn series_exist_only_for_kinds_reachable_in_the_mode() {
        let app = router_with_config(config(4));
        let text = send(&app, Method::GET, "/metrics", "").await.1;
        for phase in PHASES {
            assert!(
                text.contains(&format!("# TYPE {phase} histogram")),
                "{text}"
            );
        }
        let samples = scrape(&app).await;
        for kind in JOB_KINDS {
            let expected = if STANDALONE_KINDS.contains(&kind) {
                all(0.0)
            } else {
                [None; 3]
            };
            assert_eq!(phases(&samples, kind), expected, "{kind}");
        }
        let buckets = samples
            .iter()
            .filter(|s| {
                s.name == "knotrel_graph_lock_wait_seconds_bucket" && s.labels["kind"] == "batch"
            })
            .count();
        assert_eq!(buckets, 15, "14 explicit buckets plus +Inf");
        assert_eq!(timing_samples(&samples), 3 * 3 * 17);

        let app = router_with_config(source_managed());
        let samples = scrape(&app).await;
        for kind in JOB_KINDS {
            assert_eq!(phases(&samples, kind), all(0.0), "{kind}");
        }
        assert_eq!(timing_samples(&samples), 3 * 9 * 17);
    }

    #[tokio::test]
    async fn standalone_jobs_are_observed_once_per_phase_under_their_kind() {
        let app = router_with_config(config(4));
        let connected = r#"{"op":"connected","source":"1","target":"2"}"#;
        assert_eq!(
            send(&app, Method::POST, "/v1/operations", LINK).await.0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, Method::POST, "/v1/operations", connected)
                .await
                .0,
            StatusCode::OK
        );
        let batch = format!(r#"{{"operations":[{LINK},{connected}]}}"#);
        assert_eq!(
            send(&app, Method::POST, "/v1/batch", &batch).await.0,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, Method::GET, "/v1/stats", "").await.0,
            StatusCode::OK
        );
        let (status, _) = send(&app, Method::POST, "/v1/operations", "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let samples = scrape(&app).await;
        assert_eq!(phases(&samples, "operation"), all(2.0));
        assert_eq!(phases(&samples, "batch"), all(1.0));
        assert_eq!(phases(&samples, "stats"), all(1.0));
        for phase in PHASES {
            let sum = value(&samples, &format!("{phase}_sum"), &[("kind", "operation")]);
            assert!(sum.is_some_and(|sum| sum >= 0.0), "{phase}");
        }
    }

    #[tokio::test]
    async fn feeder_jobs_and_import_publication_use_their_own_kinds() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1", "2"], &[]).await;
        let samples = scrape(&app).await;
        assert_eq!(phases(&samples, "feeder_session"), all(1.0));
        assert_eq!(phases(&samples, "import_publish"), all(1.0));

        let heartbeat = serde_json::json!({
            "instance_id": instance,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
        });
        let (status, body) = feeder(&app, &instance, "/v1/feeder/heartbeat", None, heartbeat).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let link = event(
            1,
            serde_json::json!({"op": "link", "source": "1", "target": "2"}),
        );
        let body = events(&instance, 1, serde_json::json!([link]));
        let (status, body) = feeder(&app, &instance, "/v1/feeder/events", None, body).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let barrier = serde_json::json!({
            "instance_id": instance,
            "graph_id": "default",
            "generation": 1,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "barrier_delivery_sequence": 1,
            "barrier_source_position": "p-1",
        });
        let (status, body) = feeder(&app, &instance, "/v1/feeder/barrier", None, barrier).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let gap = serde_json::json!({
            "instance_id": instance,
            "graph_id": "default",
            "generation": 1,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "failed_delivery_sequence": 2,
        });
        let (status, body) = feeder(&app, &instance, "/v1/feeder/gap", None, gap).await;
        assert!(status.is_success(), "{status} {body}");

        let samples = scrape(&app).await;
        for kind in [
            "feeder_session",
            "feeder_heartbeat",
            "feeder_events",
            "feeder_barrier",
            "feeder_gap",
            "import_publish",
        ] {
            assert_eq!(phases(&samples, kind), all(1.0), "{kind}");
        }
        for kind in STANDALONE_KINDS {
            assert_eq!(phases(&samples, kind), all(0.0), "{kind}");
        }
    }

    #[tokio::test]
    async fn import_records_only_the_phases_it_reached() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1"], &[]).await;
        assert_eq!(phases(&scrape(&app).await, "import_publish"), all(1.0));

        // Rejected while parsing, before the publication lock.
        let (status, body) = send_raw_import(&app, &instance, "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            phases(&scrape(&app).await, "import_publish"),
            [Some(2.0), Some(1.0), Some(1.0)]
        );

        // Rejected under the publication lock.
        let candidate = import(&instance, "replace", "p-9", &["5"], &[]);
        let (status, body) = feeder(
            &app,
            &instance,
            "/v1/feeder/import",
            Some("\"1.0.wrong\""),
            candidate,
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(
            phases(&scrape(&app).await, "import_publish"),
            [Some(3.0), Some(2.0), Some(2.0)]
        );
    }

    async fn send_raw_import(app: &Router, instance: &str, body: &str) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/feeder/import")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-knotrel-instance-id", instance)
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn server_instances_observe_their_own_jobs() {
        let first = router_with_config(config(4));
        let second = router_with_config(config(4));
        assert_eq!(
            send(&first, Method::POST, "/v1/operations", LINK).await.0,
            StatusCode::OK
        );
        assert_eq!(phases(&scrape(&first).await, "operation"), all(1.0));
        assert_eq!(phases(&scrape(&second).await, "operation"), all(0.0));
    }

    #[cfg(feature = "test-support")]
    mod paused {
        use super::*;
        use std::time::Duration;

        async fn wait_idle(lifecycle: &Lifecycle) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lifecycle.outstanding_jobs() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        fn paused_app(config: ServerConfig) -> (Router, Lifecycle) {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            (router_with_lifecycle(config, lifecycle.clone()), lifecycle)
        }

        #[tokio::test]
        async fn admission_rejections_are_not_observed_as_jobs() {
            let (app, lifecycle) = paused_app(config(1));
            let mut pause = lifecycle.pause_next_worker();
            let held = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;
            let batch = format!(r#"{{"operations":[{LINK}]}}"#);
            let (status, body) = send(&app, Method::POST, "/v1/batch", &batch).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"busy\""), "{body}");

            pause.panic_on_release();
            assert_eq!(held.await.unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
            wait_idle(&lifecycle).await;
            let (status, body) = send(&app, Method::GET, "/v1/stats", "").await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"unavailable\""), "{body}");

            let samples = scrape(&app).await;
            assert_eq!(phases(&samples, "operation"), queue_only(1.0));
            assert_eq!(phases(&samples, "batch"), all(0.0));
            assert_eq!(phases(&samples, "stats"), all(0.0));
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_timed_out_is_observed_once() {
            let timed = config(4)
                .with_request_timeout(Duration::from_millis(50))
                .unwrap();
            let (app, lifecycle) = paused_app(timed);
            let mut pause = lifecycle.pause_next_worker();
            let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("request_timeout"), "{body}");
            pause.wait_started().await;
            assert_eq!(phases(&scrape(&app).await, "operation"), queue_only(1.0));

            pause.release();
            wait_idle(&lifecycle).await;
            assert_eq!(phases(&scrape(&app).await, "operation"), all(1.0));
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_was_cancelled_is_observed_once() {
            let (app, lifecycle) = paused_app(config(4));
            let mut pause = lifecycle.pause_next_worker();
            let caller = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert_eq!(phases(&scrape(&app).await, "operation"), queue_only(1.0));

            pause.release();
            wait_idle(&lifecycle).await;
            assert_eq!(phases(&scrape(&app).await, "operation"), all(1.0));
        }

        #[tokio::test]
        async fn panic_after_the_lock_records_every_phase_once() {
            let (app, lifecycle) = paused_app(config(4));
            let mut pause = lifecycle.pause_next_worker_after_lock();
            let caller = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::GET, "/v1/stats", "").await }
            });
            pause.wait_started().await;
            assert_eq!(phases(&scrape(&app).await, "stats"), all(1.0));

            pause.panic_on_release();
            assert_eq!(caller.await.unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
            wait_idle(&lifecycle).await;
            assert_eq!(phases(&scrape(&app).await, "stats"), all(1.0));
        }
    }
}

mod outcome_counters {
    use super::graph_counts::{bootstrap, event, events, feeder, import, source_managed};
    use super::*;

    const OPERATIONS: &str = "knotrel_graph_operations_total";
    const EVENTS: &str = "knotrel_feeder_events_total";

    /// Every `(op, outcome)` series of the operation counter, sorted by labels.
    fn operation_series(samples: &[Sample]) -> Vec<(String, String, f64)> {
        let mut series: Vec<_> = samples
            .iter()
            .filter(|s| s.name == OPERATIONS)
            .map(|s| (s.labels["op"].clone(), s.labels["outcome"].clone(), s.value))
            .collect();
        series.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        series
    }

    /// Asserts the nonzero operation series; every other existing series is zero.
    fn assert_operations(samples: &[Sample], expected: &[(&str, &str, f64)]) {
        let actual: Vec<_> = operation_series(samples)
            .into_iter()
            .filter(|(_, _, value)| *value != 0.0)
            .collect();
        let mut expected: Vec<_> = expected
            .iter()
            .map(|(op, outcome, value)| ((*op).to_owned(), (*outcome).to_owned(), *value))
            .collect();
        expected.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        assert_eq!(actual, expected);
    }

    /// Asserts the nonzero event series; every other existing series is zero.
    fn assert_events(samples: &[Sample], expected: &[(&str, f64)]) {
        let mut actual: Vec<_> = samples
            .iter()
            .filter(|s| s.name == EVENTS && s.value != 0.0)
            .map(|s| (s.labels["result"].clone(), s.value))
            .collect();
        actual.sort_by(|a, b| a.0.cmp(&b.0));
        let mut expected: Vec<_> = expected
            .iter()
            .map(|(result, value)| ((*result).to_owned(), *value))
            .collect();
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn series_exist_at_zero_only_for_reachable_combinations() {
        let samples = scrape(&router_with_config(config(4))).await;
        let mut expected = Vec::new();
        for op in ["add_node", "link", "cut"] {
            for outcome in ["changed", "unchanged", "error"] {
                expected.push((op.to_owned(), outcome.to_owned(), 0.0));
            }
        }
        for outcome in ["true", "false", "error"] {
            expected.push(("connected".to_owned(), outcome.to_owned(), 0.0));
        }
        let connected = expected.split_off(9);
        let sorted = |mut series: Vec<(String, String, f64)>| {
            series.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
            series
        };
        let all = sorted([expected, connected.clone()].concat());
        assert_eq!(operation_series(&samples), all);
        assert!(!samples.iter().any(|s| s.name == EVENTS));

        let samples = scrape(&router_with_config(source_managed())).await;
        assert_eq!(operation_series(&samples), sorted(connected));
        let mut results: Vec<_> = samples
            .iter()
            .filter(|s| s.name == EVENTS)
            .map(|s| (s.labels["result"].as_str(), s.value))
            .collect();
        results.sort_by(|a, b| a.0.cmp(b.0));
        let mut zero: Vec<_> = EVENT_RESULTS.iter().map(|r| (*r, 0.0)).collect();
        zero.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(results, zero);
    }

    #[tokio::test]
    async fn every_executed_operation_is_counted_once_by_outcome() {
        let app = router_with_config(config(4));
        let singles = [
            (r#"{"op":"add_node","node":"1"}"#, StatusCode::OK),
            (r#"{"op":"add_node","node":"1"}"#, StatusCode::OK),
            (LINK, StatusCode::OK),
            (LINK, StatusCode::OK),
            (r#"{"op":"cut","source":"1","target":"2"}"#, StatusCode::OK),
            (r#"{"op":"cut","source":"1","target":"2"}"#, StatusCode::OK),
            (
                r#"{"op":"connected","source":"1","target":"2"}"#,
                StatusCode::OK,
            ),
            (LINK, StatusCode::OK),
            (
                r#"{"op":"connected","source":"1","target":"2"}"#,
                StatusCode::OK,
            ),
            (
                r#"{"op":"connected","source":"1","target":"99"}"#,
                StatusCode::NOT_FOUND,
            ),
            (
                r#"{"op":"link","source":"3","target":"3"}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ];
        for (body, status) in singles {
            assert_eq!(
                send(&app, Method::POST, "/v1/operations", body).await.0,
                status,
                "{body}"
            );
        }
        // Execution continues after the domain error at index 1.
        let batch = r#"{"operations":[
            {"op":"link","source":"3","target":"4"},
            {"op":"link","source":"5","target":"5"},
            {"op":"connected","source":"3","target":"4"},
            {"op":"add_node","node":"3"}
        ]}"#;
        let (status, body) = send(&app, Method::POST, "/v1/batch", batch).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("self_loop"), "{body}");

        assert_operations(
            &scrape(&app).await,
            &[
                ("add_node", "changed", 1.0),
                ("add_node", "unchanged", 2.0),
                ("link", "changed", 3.0),
                ("link", "unchanged", 1.0),
                ("link", "error", 2.0),
                ("cut", "changed", 1.0),
                ("cut", "unchanged", 1.0),
                ("connected", "true", 2.0),
                ("connected", "false", 1.0),
                ("connected", "error", 1.0),
            ],
        );
    }

    #[tokio::test]
    async fn requests_rejected_before_execution_count_no_operations() {
        let app = router_with_config(config(4));
        for (path, body, status) in [
            ("/v1/operations", "{", StatusCode::BAD_REQUEST),
            (
                "/v1/operations",
                r#"{"op":"link","source":"x","target":"2"}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "/v1/batch",
                r#"{"operations":[]}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            assert_eq!(
                send(&app, Method::POST, path, body).await.0,
                status,
                "{body}"
            );
        }
        assert_operations(&scrape(&app).await, &[]);

        // Source-managed: direct mutations are refused before admission, and
        // queries are refused per entry by the worker's sync-status gate
        // before any is evaluated.
        let app = router_with_config(source_managed());
        let connected = r#"{"op":"connected","source":"1","target":"2"}"#;
        let (status, body) = send(&app, Method::POST, "/v1/operations", connected).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body.contains("uninitialized"), "{body}");
        let (status, body) = send(&app, Method::POST, "/v1/operations", LINK).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_operations(&scrape(&app).await, &[]);
    }

    #[tokio::test]
    async fn feeder_events_are_counted_per_evaluated_event_without_operations() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1", "2", "3"], &[("1", "2")]).await;
        let samples = scrape(&app).await;
        assert_events(&samples, &[]);
        assert_operations(&samples, &[]);

        let link = |seq, s: &str, t: &str| {
            event(
                seq,
                serde_json::json!({"op": "link", "source": s, "target": t}),
            )
        };
        let cut = |seq, s: &str, t: &str| {
            event(
                seq,
                serde_json::json!({"op": "cut", "source": s, "target": t}),
            )
        };
        let add = |seq, n: &str| event(seq, serde_json::json!({"op": "add_node", "node": n}));
        let post = |body: serde_json::Value| {
            let app = app.clone();
            let instance = instance.clone();
            async move { feeder(&app, &instance, "/v1/feeder/events", None, body).await }
        };

        let first = serde_json::json!([
            link(1, "2", "3"),
            event(2, serde_json::json!({"op": "progress"})),
            link(3, "1", "2"),
            add(4, "1"),
            cut(5, "2", "3"),
        ]);
        let (status, reply) = post(events(&instance, 1, first)).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_events(&scrape(&app).await, &[("changed", 2.0), ("unchanged", 3.0)]);

        // A retry of retained events followed by a new one.
        let retry = serde_json::json!([add(4, "1"), cut(5, "2", "3"), add(6, "7")]);
        let (status, reply) = post(events(&instance, 1, retry)).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        let (status, reply) = post(events(&instance, 1, serde_json::json!([add(6, "7")]))).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(reply["result"], "duplicate", "{reply}");

        // Applied prefix, failing event, untouched suffix.
        let partial = serde_json::json!([cut(7, "1", "2"), link(8, "3", "3"), add(9, "8")]);
        let (status, reply) = post(events(&instance, 1, partial)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reply}");
        assert_eq!(reply["applied_delivery_sequence"], 7, "{reply}");

        // A continuity error found while evaluating the event is counted;
        // the same status from whole-request fencing is not.
        let (status, reply) = post(events(&instance, 1, serde_json::json!([add(9, "8")]))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{reply}");
        assert_eq!(reply["code"], "out_of_order_sequence", "{reply}");
        let (status, reply) = post(events(&instance, 2, serde_json::json!([add(8, "8")]))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{reply}");
        assert_eq!(reply["code"], "stale_generation", "{reply}");
        let mut long_id = add(8, "8");
        long_id["event_id"] = serde_json::json!("x".repeat(65));
        let (status, reply) = post(events(
            &instance,
            1,
            serde_json::json!([add(8, "8"), long_id]),
        ))
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reply}");
        assert_eq!(reply["code"], "invalid_request", "{reply}");

        let samples = scrape(&app).await;
        assert_events(
            &samples,
            &[
                ("changed", 4.0),
                ("unchanged", 3.0),
                ("duplicate", 3.0),
                ("rejected", 2.0),
            ],
        );
        assert_operations(&samples, &[]);

        let connected = r#"{"op":"connected","source":"1","target":"7"}"#;
        assert_eq!(
            send(&app, Method::POST, "/v1/operations", connected)
                .await
                .0,
            StatusCode::OK
        );
        let samples = scrape(&app).await;
        assert_operations(&samples, &[("connected", "false", 1.0)]);
        assert_eq!(
            samples
                .iter()
                .filter(|s| s.name == EVENTS)
                .map(|s| s.value)
                .sum::<f64>(),
            12.0
        );
    }

    #[tokio::test]
    async fn import_publication_counts_no_operations_or_events() {
        let app = router_with_config(source_managed());
        let instance = bootstrap(&app, &["1", "2"], &[("1", "2")]).await;
        let body = import(&instance, "replace", "p-9", &["5", "6"], &[("5", "6")]);
        let (status, reply) = feeder(
            &app,
            &instance,
            "/v1/feeder/import",
            Some("\"1.0.p-0\""),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
        let samples = scrape(&app).await;
        assert_operations(&samples, &[]);
        assert_events(&samples, &[]);
    }

    #[tokio::test]
    async fn server_instances_count_their_own_outcomes() {
        let first = router_with_config(config(4));
        let second = router_with_config(config(4));
        assert_eq!(
            send(&first, Method::POST, "/v1/operations", LINK).await.0,
            StatusCode::OK
        );
        assert_operations(&scrape(&first).await, &[("link", "changed", 1.0)]);
        assert_operations(&scrape(&second).await, &[]);
    }

    #[cfg(feature = "test-support")]
    mod paused {
        use super::*;
        use std::time::Duration;

        async fn wait_idle(lifecycle: &Lifecycle) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lifecycle.outstanding_jobs() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }

        fn paused_app(config: ServerConfig) -> (Router, Lifecycle) {
            let lifecycle = Lifecycle::new();
            lifecycle.mark_ready();
            (router_with_lifecycle(config, lifecycle.clone()), lifecycle)
        }

        #[tokio::test]
        async fn admission_rejection_counts_no_operations() {
            let (app, lifecycle) = paused_app(config(1));
            let mut pause = lifecycle.pause_next_worker();
            let held = tokio::spawn({
                let app = app.clone();
                async move { send(&app, Method::POST, "/v1/operations", LINK).await }
            });
            pause.wait_started().await;
            let batch = format!(r#"{{"operations":[{LINK},{LINK}]}}"#);
            let (status, body) = send(&app, Method::POST, "/v1/batch", &batch).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("\"busy\""), "{body}");
            pause.release();
            assert_eq!(held.await.unwrap().0, StatusCode::OK);
            assert_operations(&scrape(&app).await, &[("link", "changed", 1.0)]);
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_timed_out_counts_its_operations() {
            let timed = config(4)
                .with_request_timeout(Duration::from_millis(50))
                .unwrap();
            let (app, lifecycle) = paused_app(timed);
            let mut pause = lifecycle.pause_next_worker();
            let batch = format!(r#"{{"operations":[{LINK},{LINK}]}}"#);
            let (status, body) = send(&app, Method::POST, "/v1/batch", &batch).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(body.contains("request_timeout"), "{body}");
            pause.wait_started().await;
            assert_operations(&scrape(&app).await, &[]);

            pause.release();
            wait_idle(&lifecycle).await;
            assert_operations(
                &scrape(&app).await,
                &[("link", "changed", 1.0), ("link", "unchanged", 1.0)],
            );
        }

        #[tokio::test]
        async fn worker_finishing_after_its_caller_was_cancelled_counts_its_events() {
            let (app, lifecycle) = paused_app(source_managed());
            let instance = bootstrap(&app, &["1", "2"], &[]).await;
            let mut pause = lifecycle.pause_next_worker();
            let caller = tokio::spawn({
                let app = app.clone();
                let instance = instance.clone();
                let body = events(
                    &instance,
                    1,
                    serde_json::json!([event(
                        1,
                        serde_json::json!({"op": "link", "source": "1", "target": "2"})
                    )]),
                );
                async move { feeder(&app, &instance, "/v1/feeder/events", None, body).await }
            });
            pause.wait_started().await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert_events(&scrape(&app).await, &[]);

            pause.release();
            wait_idle(&lifecycle).await;
            assert_events(&scrape(&app).await, &[("changed", 1.0)]);
        }

        /// The batch completes under the lock and the worker then panics in
        /// the post-lock hook: completed outcomes stay counted, nothing more.
        #[tokio::test]
        async fn outcomes_recorded_before_a_worker_panic_are_kept() {
            let (app, lifecycle) = paused_app(config(4));
            let mut pause = lifecycle.pause_next_worker_after_lock();
            let caller = tokio::spawn({
                let app = app.clone();
                let batch = format!(r#"{{"operations":[{LINK},{LINK}]}}"#);
                async move { send(&app, Method::POST, "/v1/batch", &batch).await }
            });
            pause.wait_started().await;
            let expected = [("link", "changed", 1.0), ("link", "unchanged", 1.0)];
            assert_operations(&scrape(&app).await, &expected);

            pause.panic_on_release();
            assert_eq!(caller.await.unwrap().0, StatusCode::SERVICE_UNAVAILABLE);
            wait_idle(&lifecycle).await;
            assert_operations(&scrape(&app).await, &expected);
        }
    }
}
