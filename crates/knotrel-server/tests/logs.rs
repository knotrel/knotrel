//! Structured logs (M6.3): request correlation across blocking workers,
//! synchronization diagnostics, rejection codes and redaction of graph
//! payloads, session IDs and unsanitized caller text.
//!
//! One process-wide JSON subscriber captures every line, so this file holds a
//! single test that drives all scenarios in order.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use knotrel_core::EngineConfig;
use knotrel_server::{ServerConfig, ServerMode, router_with_config};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

static INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
const SECRET_SESSION: &str = "SECRET-SESSION-7f3a";
const NODE_A: &str = "998877661";
const NODE_B: &str = "998877552";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
    fn lines(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
            .collect()
    }
}

fn install() -> Capture {
    let capture = Capture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_env_filter("info")
        .with_writer(move || writer.clone())
        .init();
    capture
}

async fn send(app: &Router, method: Method, path: &str, body: String) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    "x-knotrel-instance-id",
                    INSTANCE.get().map_or("", String::as_str),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn message(line: &Value) -> &str {
    line["fields"]["message"].as_str().unwrap_or("")
}

fn find(lines: &[Value], pred: impl Fn(&Value) -> bool) -> Vec<&Value> {
    lines.iter().filter(|line| pred(line)).collect()
}

#[tokio::test]
async fn logs_correlate_requests_and_never_expose_payloads() {
    let capture = install();
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4)
        .unwrap()
        .with_mode(ServerMode::SourceManaged);
    let app = router_with_config(config);

    let (_, info) = send(&app, Method::GET, "/v1/info", String::new()).await;
    let instance = info["instance_id"].as_str().unwrap().to_owned();
    INSTANCE.set(instance.clone()).unwrap();
    let common = |generation: u64| {
        json!({
            "instance_id": instance,
            "graph_id": "default",
            "generation": generation,
            "feeder_session_id": SECRET_SESSION,
            "feeder_epoch": 1,
        })
    };
    let merge = |mut base: Value, extra: Value| {
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    };

    let session = json!({
        "instance_id": instance,
        "feeder_session_id": SECRET_SESSION,
        "feeder_epoch": 1,
        "lease_duration_ms": 60_000,
    });
    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/feeder/session",
        session.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let long_position = format!("p-0\n{}", "z".repeat(200));
    let import = json!({
        "mode": "create_only",
        "instance_id": instance,
        "graph_id": "default",
        "source_id": "pg-1",
        "source_epoch": "ep-1",
        "source_position": long_position,
        "feeder_session_id": SECRET_SESSION,
        "feeder_epoch": 1,
        "nodes": [NODE_A, NODE_B],
        "edges": [],
    });
    let (status, body) = send(&app, Method::POST, "/v1/feeder/import", import.to_string()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let event = json!({
        "event_id": "e-1",
        "delivery_sequence": 1,
        "prev_delivery_sequence": 0,
        "source_position": "p-1",
        "prev_source_position": long_position,
        "op": "link",
        "source": NODE_A,
        "target": NODE_B,
    });
    let events = merge(
        common(1),
        json!({"source_id": "pg-1", "source_epoch": "ep-1", "events": [event]}),
    );
    let (status, body) = send(&app, Method::POST, "/v1/feeder/events", events.to_string()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let barrier = merge(
        common(1),
        json!({"barrier_delivery_sequence": 1, "barrier_source_position": "p-1"}),
    );
    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/feeder/barrier",
        barrier.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let gap = merge(
        common(1),
        json!({"failed_delivery_sequence": 2, "reason": "broker\nINJECTED line"}),
    );
    let (status, body) = send(&app, Method::POST, "/v1/feeder/gap", gap.to_string()).await;
    assert!(status.is_success(), "{status} {body}");

    let (status, _) = send(&app, Method::POST, "/v1/feeder/events", events.to_string()).await;
    assert!(status.is_client_error(), "{status}");

    let unknown = format!("/x{}", "a".repeat(200));
    let (status, _) = send(&app, Method::GET, &unknown, String::new()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    send(&app, Method::GET, "/health", String::new()).await;

    // A request future dropped while its worker runs logs a cancellation
    // line instead of a completion line.
    #[cfg(feature = "test-support")]
    let cancelled_before = {
        let lifecycle = knotrel_server::Lifecycle::new();
        lifecycle.mark_ready();
        let standalone =
            ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4).unwrap();
        let app = knotrel_server::router_with_lifecycle(standalone, lifecycle.clone());
        let mut pause = lifecycle.pause_next_worker();
        let request = tokio::spawn({
            let app = app.clone();
            async move {
                let link = json!({"op": "link", "source": "1", "target": "2"});
                send(&app, Method::POST, "/v1/operations", link.to_string()).await
            }
        });
        pause.wait_started().await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        pause.release();
        true
    };

    let text = capture.text();
    let lines = capture.lines();

    #[cfg(feature = "test-support")]
    {
        assert!(cancelled_before);
        let cancelled = find(&lines, |l| {
            message(l) == "request cancelled before response"
        });
        assert_eq!(cancelled.len(), 1, "{text}");
        assert_eq!(cancelled[0]["span"]["route"], "/v1/operations");
        let id = &cancelled[0]["span"]["request_id"];
        assert!(
            find(&lines, |l| message(l) == "handled request"
                && &l["span"]["request_id"] == id)
            .is_empty()
        );
    }

    // Redaction: no session ID, node IDs or raw multi-line caller text.
    for secret in [
        SECRET_SESSION,
        NODE_A,
        NODE_B,
        "broker\nINJECTED",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        assert!(!text.contains(secret), "log contains {secret:?}:\n{text}");
    }
    assert!(text.contains(r"broker\\nINJECTED line"), "{text}");

    // Exactly one completion line per API request; probes stay below info.
    let handled = find(&lines, |l| message(l) == "handled request");
    assert_eq!(handled.len(), 8, "{text}");
    let mut ids: Vec<u64> = handled
        .iter()
        .map(|l| l["span"]["request_id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), handled.len(), "request IDs must be unique");
    assert!(find(&lines, |l| l["span"]["route"] == "/health").is_empty());

    let request_for = |route: &str| -> &Value {
        handled
            .iter()
            .find(|l| l["span"]["route"] == route && l["fields"]["status"] == 200)
            .copied()
            .unwrap_or_else(|| panic!("no 200 line for {route}:\n{text}"))
    };

    // Worker lines carry the submitting request's ID.
    let barrier_line = request_for("/v1/feeder/barrier");
    let barrier_id = &barrier_line["span"]["request_id"];
    assert_eq!(barrier_line["span"]["generation"], 1);
    assert_eq!(barrier_line["span"]["sync_status"], "ready");
    assert!(barrier_line["span"]["state_version"].is_string());
    let ready = find(&lines, |l| {
        message(l) == "synchronization state changed" && l["fields"]["to"] == "ready"
    });
    assert_eq!(ready.len(), 1, "{text}");
    assert_eq!(&ready[0]["span"]["request_id"], barrier_id);
    assert_eq!(ready[0]["fields"]["from"], "catching_up");

    let published = find(&lines, |l| message(l) == "graph published by import");
    assert_eq!(published.len(), 1, "{text}");
    assert_eq!(published[0]["fields"]["generation"], 1);
    assert_eq!(published[0]["fields"]["node_count"], 2);
    let position = published[0]["fields"]["source_position"].as_str().unwrap();
    assert!(
        position.starts_with(r"p-0\n") && position.ends_with('…'),
        "{position}"
    );
    assert!(position.chars().count() <= 66, "{position}");

    let desync = find(&lines, |l| {
        message(l) == "synchronization state changed" && l["fields"]["to"] == "desynchronized"
    });
    assert_eq!(desync.len(), 1, "{text}");
    assert_eq!(desync[0]["level"], "WARN");
    assert_eq!(desync[0]["span"]["route"], "/v1/feeder/gap");

    let rejected = find(&lines, |l| message(l) == "feeder request rejected");
    assert_eq!(rejected.len(), 1, "{text}");
    assert_eq!(rejected[0]["fields"]["code"], "graph_desynchronized");
    assert_eq!(rejected[0]["span"]["route"], "/v1/feeder/events");

    let unmatched = handled
        .iter()
        .find(|l| l["span"]["route"] == "unmatched")
        .unwrap();
    let uri = unmatched["fields"]["uri"].as_str().unwrap();
    assert!(uri.ends_with('…') && uri.chars().count() == 65, "{uri}");
}
