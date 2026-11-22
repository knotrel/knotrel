//! Uniform feeder input limits (decision 6 of the v1.0 contract freeze
//! review). Source positions (event `source_position` and
//! `prev_source_position`, import `source_position`,
//! `barrier_source_position`) are 1 to 256 bytes of UTF-8, and `event_id` is
//! at most 64 bytes. The limits count **bytes**, not characters: every test
//! covers the empty value, the exact limit, one byte over, and a multibyte
//! string whose character count is under the limit but whose byte count is
//! over it. A refused request answers `422 invalid_request` and changes
//! nothing.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use knotrel_core::EngineConfig;
use knotrel_server::{ServerConfig, ServerMode, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

const MAX_POSITION: usize = 256;
const MAX_EVENT_ID: usize = 64;
/// A two-byte UTF-8 character.
const TWO_BYTES: &str = "é";

fn app() -> Router {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4)
        .unwrap()
        .with_mode(ServerMode::SourceManaged);
    router_with_config(config)
}

async fn get(app: &Router, path: &str) -> Value {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post(app: &Router, instance: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-knotrel-instance-id", instance)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Starts a server with a live feeder session; returns its instance ID.
async fn with_session(app: &Router) -> String {
    let instance = get(app, "/v1/info").await["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post(
        app,
        &instance,
        "/v1/feeder/session",
        json!({
            "instance_id": instance,
            "feeder_session_id": "s",
            "feeder_epoch": 1,
            "lease_duration_ms": 60_000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    instance
}

fn import_body(instance: &str, position: &str) -> Value {
    json!({
        "mode": "create_only",
        "instance_id": instance,
        "graph_id": "default",
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "source_id": "pg-1",
        "source_epoch": "ep-1",
        "source_position": position,
        "nodes": [],
        "edges": [],
    })
}

/// Starts a server with a session and an imported graph at `position`.
async fn imported(position: &str) -> (Router, String) {
    let app = app();
    let instance = with_session(&app).await;
    let (status, body) = post(
        &app,
        &instance,
        "/v1/feeder/import",
        import_body(&instance, position),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (app, instance)
}

fn events_body(instance: &str, event_id: &str, prev: &str, position: &str) -> Value {
    json!({
        "instance_id": instance,
        "graph_id": "default",
        "generation": 1,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "source_id": "pg-1",
        "source_epoch": "ep-1",
        "events": [{
            "event_id": event_id,
            "delivery_sequence": 1,
            "prev_delivery_sequence": 0,
            "source_position": position,
            "prev_source_position": prev,
            "op": "progress",
        }],
    })
}

fn barrier_body(instance: &str, position: &str) -> Value {
    json!({
        "instance_id": instance,
        "graph_id": "default",
        "generation": 1,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "barrier_delivery_sequence": 0,
        "barrier_source_position": position,
    })
}

/// Values refused by the 1–256 byte source-position bound, with a label.
fn bad_positions() -> [(&'static str, String); 3] {
    let multibyte = TWO_BYTES.repeat(MAX_POSITION / 2 + 1);
    assert!(multibyte.chars().count() < MAX_POSITION);
    assert_eq!(multibyte.len(), MAX_POSITION + 2);
    [
        ("empty", String::new()),
        ("one byte over", "x".repeat(MAX_POSITION + 1)),
        ("multibyte over in bytes, under in characters", multibyte),
    ]
}

/// Values accepted at exactly the limit: ASCII and multibyte.
fn positions_at_limit() -> [String; 2] {
    let multibyte = TWO_BYTES.repeat(MAX_POSITION / 2);
    assert_eq!(multibyte.len(), MAX_POSITION);
    ["a".repeat(MAX_POSITION), multibyte]
}

fn assert_invalid(status: StatusCode, body: &Value, field: &str, case: &str) {
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{case}: {body}");
    assert_eq!(body["code"], "invalid_request", "{case}: {body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains(field) && message.contains("bytes"),
        "{case}: {message}"
    );
}

async fn applied_position(app: &Router) -> Value {
    get(app, "/v1/stats").await["source_position"].clone()
}

#[tokio::test]
async fn import_source_position_is_one_to_256_bytes() {
    for (case, position) in bad_positions() {
        let app = app();
        let instance = with_session(&app).await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/import",
            import_body(&instance, &position),
        )
        .await;
        assert_invalid(status, &body, "source_position", case);
        // Nothing was published: the graph is still uninitialized.
        assert_eq!(get(&app, "/v1/stats").await["generation"], 0, "{case}");
    }
    for position in positions_at_limit() {
        let (app, _) = imported(&position).await;
        assert_eq!(applied_position(&app).await, position.as_str());
    }
}

#[tokio::test]
async fn event_source_positions_are_one_to_256_bytes() {
    let start = "p-0";
    for (case, position) in bad_positions() {
        let (app, instance) = imported(start).await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/events",
            events_body(&instance, "e-1", start, &position),
        )
        .await;
        assert_invalid(status, &body, "source_position", case);
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/events",
            events_body(&instance, "e-1", &position, "p-1"),
        )
        .await;
        assert_invalid(status, &body, "prev_source_position", case);
        assert_eq!(applied_position(&app).await, start, "{case}");
        assert_eq!(get(&app, "/v1/stats").await["sync_status"], "catching_up");
    }
    // Exactly at the limit, on both fields: the predecessor equals an
    // imported 256-byte position, and the event moves to another one.
    let [ascii, multibyte] = positions_at_limit();
    let (app, instance) = imported(&ascii).await;
    let (status, body) = post(
        &app,
        &instance,
        "/v1/feeder/events",
        events_body(&instance, "e-1", &ascii, &multibyte),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(applied_position(&app).await, multibyte.as_str());
}

#[tokio::test]
async fn event_id_is_at_most_64_bytes() {
    let multibyte_over = TWO_BYTES.repeat(MAX_EVENT_ID / 2 + 1);
    assert!(multibyte_over.chars().count() < MAX_EVENT_ID);
    for (case, event_id) in [
        ("one byte over", "x".repeat(MAX_EVENT_ID + 1)),
        (
            "multibyte over in bytes, under in characters",
            multibyte_over,
        ),
    ] {
        let (app, instance) = imported("p-0").await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/events",
            events_body(&instance, &event_id, "p-0", "p-1"),
        )
        .await;
        assert_invalid(status, &body, "event_id", case);
        assert_eq!(applied_position(&app).await, "p-0", "{case}");
    }
    // Accepted: exactly 64 bytes (ASCII and multibyte), and the empty
    // string, for which no minimum is documented.
    for event_id in [
        "x".repeat(MAX_EVENT_ID),
        TWO_BYTES.repeat(MAX_EVENT_ID / 2),
        String::new(),
    ] {
        let (app, instance) = imported("p-0").await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/events",
            events_body(&instance, &event_id, "p-0", "p-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{event_id:?}: {body}");
    }
}

#[tokio::test]
async fn barrier_source_position_is_one_to_256_bytes() {
    for (case, position) in bad_positions() {
        let (app, instance) = imported("p-0").await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/barrier",
            barrier_body(&instance, &position),
        )
        .await;
        assert_invalid(status, &body, "barrier_source_position", case);
        assert_eq!(get(&app, "/v1/stats").await["sync_status"], "catching_up");
    }
    for position in positions_at_limit() {
        let (app, instance) = imported(&position).await;
        let (status, body) = post(
            &app,
            &instance,
            "/v1/feeder/barrier",
            barrier_body(&instance, &position),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["result"], "synchronized", "{body}");
    }
}
