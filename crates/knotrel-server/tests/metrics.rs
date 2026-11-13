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
