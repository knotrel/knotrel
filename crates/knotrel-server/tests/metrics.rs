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
