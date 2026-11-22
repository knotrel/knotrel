//! Security-baseline tests: bounded resources, safe defaults and stable errors.
//!
//! These tests use in-process routers only, so they do not need a network.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use knotrel_core::{EngineConfig, GraphLimits};
use knotrel_server::{ServerConfig, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

// Documented limits; deliberately not imported so a change breaks this test.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_BATCH_OPERATIONS: usize = 1024;

const ENGINES: [EngineConfig; 3] = [
    EngineConfig::CompactBfs,
    EngineConfig::EulerTour,
    EngineConfig::Hdt,
];

fn config(engine: EngineConfig) -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 32).unwrap()
}

fn limited(engine: EngineConfig, max_nodes: Option<usize>, max_edges: Option<usize>) -> Router {
    router_with_config(config(engine).with_graph_limits(GraphLimits {
        max_nodes,
        max_edges,
    }))
}

async fn send(app: &Router, method: &str, path: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn op(app: &Router, value: Value) -> (StatusCode, Value) {
    let (status, bytes) = send(app, "POST", "/v1/operations", value.to_string().into()).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn get(app: &Router, path: &str) -> Value {
    let (status, bytes) = send(app, "GET", path, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice(&bytes).unwrap()
}

fn add(node: usize) -> Value {
    json!({"op":"add_node","node":node.to_string()})
}

#[tokio::test]
async fn configured_servers_default_to_finite_graph_limits_on_every_engine() {
    assert_eq!(ServerConfig::DEFAULT_MAX_NODES, 100_000);
    assert_eq!(ServerConfig::DEFAULT_MAX_EDGES, 500_000);
    for engine in ENGINES {
        for config in [config(engine), {
            let mut config = ServerConfig::default();
            config =
                ServerConfig::new(config.address(), engine, config.max_pending_jobs()).unwrap();
            config
        }] {
            assert!(!config.is_exposed());
            let app = router_with_config(config);
            let info = get(&app, "/v1/info").await;
            assert_eq!(info["max_nodes"], 100_000, "{engine:?}");
            assert_eq!(info["max_edges"], 500_000, "{engine:?}");
            assert_eq!(info["max_body_bytes"], MAX_BODY_BYTES);
            assert_eq!(info["max_batch_operations"], MAX_BATCH_OPERATIONS);
            let stats = get(&app, "/v1/stats").await;
            assert_eq!(stats["remaining_nodes"], 100_000);
            assert_eq!(stats["remaining_edges"], 500_000);
        }
    }
}

#[tokio::test]
async fn node_limit_boundary_is_exact_and_rejections_do_not_mutate() {
    for engine in ENGINES {
        for limit in [0usize, 1, 3] {
            let app = limited(engine, Some(limit), None);
            for node in 0..limit {
                let (status, body) = op(&app, add(node)).await;
                assert_eq!((status, &body["changed"]), (StatusCode::OK, &json!(true)));
            }
            // Re-adding an existing node does not consume capacity.
            if limit > 0 {
                let (status, body) = op(&app, add(0)).await;
                assert_eq!((status, &body["changed"]), (StatusCode::OK, &json!(false)));
            }
            let (status, body) = op(&app, add(limit)).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{engine:?} {limit}"
            );
            assert_eq!(body["code"], "node_limit_exceeded");
            let stats = get(&app, "/v1/stats").await;
            assert_eq!(stats["node_count"], limit);
            assert_eq!(stats["remaining_nodes"], 0);
            // The rejected request must not have advanced the state version.
            assert_eq!(stats["state_version"], limit.to_string());
        }
    }
}

#[tokio::test]
async fn edge_limit_boundary_is_exact_and_unlimited_is_an_explicit_opt_out() {
    for engine in ENGINES {
        let app = limited(engine, None, Some(2));
        for (source, target) in [(0, 1), (1, 2)] {
            let (status, _) = op(
                &app,
                json!({"op":"link","source":source.to_string(),"target":target.to_string()}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, body) = op(&app, json!({"op":"link","source":"2","target":"3"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{engine:?}");
        assert_eq!(body["code"], "edge_limit_exceeded");
        // A cut frees capacity, so the limit bounds live edges only.
        op(&app, json!({"op":"cut","source":"0","target":"1"})).await;
        let (status, _) = op(&app, json!({"op":"link","source":"2","target":"3"})).await;
        assert_eq!(status, StatusCode::OK);

        let app = limited(engine, None, None);
        let info = get(&app, "/v1/info").await;
        assert_eq!(
            (&info["max_nodes"], &info["max_edges"]),
            (&Value::Null, &Value::Null)
        );
        for node in 0..200 {
            assert_eq!(op(&app, add(node)).await.0, StatusCode::OK);
        }
    }
}

#[tokio::test]
async fn batches_cannot_exceed_limits_and_report_per_operation_rejections() {
    for engine in ENGINES {
        let app = limited(engine, Some(2), None);
        let operations: Vec<Value> = (0..4).map(add).collect();
        let (status, bytes) = send(
            &app,
            "POST",
            "/v1/batch",
            json!({"operations":operations}).to_string().into(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let codes: Vec<_> = body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["code"].as_str().unwrap_or("ok").to_owned())
            .collect();
        assert_eq!(
            codes,
            ["ok", "ok", "node_limit_exceeded", "node_limit_exceeded"]
        );
        assert_eq!(get(&app, "/v1/stats").await["node_count"], 2);
    }
}

#[tokio::test]
async fn request_size_bounds_are_enforced_before_parsing_on_every_engine() {
    for engine in ENGINES {
        let app = router_with_config(config(engine));
        for path in ["/v1/operations", "/v1/batch"] {
            // Not even valid JSON: a 413 proves the body was never parsed.
            let (status, bytes) = send(&app, "POST", path, vec![b'x'; MAX_BODY_BYTES + 1]).await;
            assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{path}");
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["code"], "invalid_request");
            assert!(bytes.len() < 200, "error echoed the request body");
        }
        // Exactly at the limit the body is accepted and parsed (here: rejected as JSON).
        let (status, _) = send(&app, "POST", "/v1/operations", vec![b'x'; MAX_BODY_BYTES]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A padded but valid body of exactly MAX_BODY_BYTES is processed normally.
        let mut padded = br#"{"op":"add_node","node":"1"}"#.to_vec();
        padded.resize(MAX_BODY_BYTES, b' ');
        let (status, _) = send(&app, "POST", "/v1/operations", padded).await;
        assert_eq!(status, StatusCode::OK);

        let operations: Vec<Value> = (0..=MAX_BATCH_OPERATIONS).map(add).collect();
        let (status, bytes) = send(
            &app,
            "POST",
            "/v1/batch",
            json!({"operations":operations}).to_string().into(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "invalid_batch_size");
        // Rejected whole: nothing from the oversized batch was applied.
        assert_eq!(get(&app, "/v1/stats").await["node_count"], 1);
    }
}

#[tokio::test]
async fn client_errors_are_stable_and_do_not_leak_internals_or_payloads() {
    const FORBIDDEN: [&str; 8] = [
        "simd",
        "serde",
        "panick",
        "src/",
        ".rs",
        "/Users/",
        "thread",
        "secret-marker",
    ];
    let app = router_with_config(config(EngineConfig::default()));
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        (
            "POST",
            "/v1/operations",
            br#"{"op":"link","secret-marker"#.to_vec(),
        ),
        (
            "POST",
            "/v1/operations",
            br#"{"op":"secret-marker"}"#.to_vec(),
        ),
        (
            "POST",
            "/v1/operations",
            br#"{"op":"add_node","node":"secret-marker"}"#.to_vec(),
        ),
        (
            "POST",
            "/v1/batch",
            br#"{"operations":"secret-marker"}"#.to_vec(),
        ),
        (
            "POST",
            "/v1/batch",
            br#"{"operations":[],"secret-marker":1}"#.to_vec(),
        ),
        ("POST", "/v1/operations", vec![0xff, 0xfe]),
        ("GET", "/secret-marker", Vec::new()),
        ("DELETE", "/v1/operations", Vec::new()),
        ("POST", "/v1/operations", vec![b'x'; MAX_BODY_BYTES + 1]),
    ];
    for (method, path, body) in cases {
        let (status, bytes) = send(&app, method, path, body).await;
        assert!(status.is_client_error(), "{method} {path}: {status}");
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        for word in FORBIDDEN {
            assert!(
                !text.contains(&word.to_lowercase()),
                "{method} {path} leaked {word}: {text}"
            );
        }
        if !bytes.is_empty() {
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["result"], "error", "{method} {path}");
            assert!(body["code"].is_string() && body["message"].is_string());
        }
    }
}

#[tokio::test]
async fn schema_violations_never_echo_client_supplied_names_or_values() {
    let app = router_with_config(config(EngineConfig::default()));
    for body in [
        r#"{"op":"secret-marker"}"#,
        r#"{"op":"add_node","node":1,"secret-marker":1}"#,
        r#"{"op":"add_node","node":["secret-marker"]}"#,
        r#"{"op":"secret-marker","source":"1","target":"2"}"#,
    ] {
        let (status, bytes) = send(&app, "POST", "/v1/operations", body.into()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["message"], "request body does not match the expected shape",
            "{body}"
        );
    }
}

#[test]
fn non_loopback_listener_requires_a_separate_constructor() {
    for address in ["0.0.0.0:8080", "[::]:8080", "192.0.2.1:80"] {
        let address = address.parse().unwrap();
        let error = ServerConfig::new(address, EngineConfig::Hdt, 1).unwrap_err();
        assert!(error.to_string().contains("KNOTREL_ALLOW_NON_LOOPBACK"));
        let exposed =
            ServerConfig::new_allowing_non_loopback(address, EngineConfig::Hdt, 1).unwrap();
        assert!(exposed.is_exposed());
        assert_eq!(exposed.address(), address);
        // Exposure does not alter the resource bounds.
        assert_eq!(exposed.graph_limits(), ServerConfig::DEFAULT_GRAPH_LIMITS);
    }
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let config = ServerConfig::new(address.parse().unwrap(), EngineConfig::Hdt, 1).unwrap();
        assert!(!config.is_exposed());
    }
    assert!(!ServerConfig::default().is_exposed());
    // The admission bound is still validated on the exposed path.
    assert!(
        ServerConfig::new_allowing_non_loopback("0.0.0.0:1".parse().unwrap(), EngineConfig::Hdt, 0)
            .is_err()
    );
}

/// A source-managed router with a registered session (`f`, epoch 1) and a
/// `create_only` baseline at source position `p0` (generation 1), plus the
/// instance ID every feeder request must carry.
async fn bootstrapped_feeder() -> (Router, String) {
    let app = router_with_config(
        config(EngineConfig::CompactBfs).with_mode(knotrel_server::ServerMode::SourceManaged),
    );
    let instance = get(&app, "/v1/info").await["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = json!({
        "instance_id": instance, "feeder_session_id": "f", "feeder_epoch": 1,
        "lease_duration_ms": 30_000,
    });
    let (status, body) = feeder(&app, &instance, "session", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = feeder(&app, &instance, "import", &import_body(&instance), None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (app, instance)
}

fn import_body(instance: &str) -> Value {
    json!({
        "instance_id": instance, "graph_id": "default", "mode": "create_only",
        "feeder_session_id": "f", "feeder_epoch": 1, "source_id": "s",
        "source_epoch": "e", "source_position": "p0", "nodes": ["1", "2"], "edges": [],
    })
}

fn events_body(instance: &str, events: Value) -> Value {
    json!({
        "instance_id": instance, "graph_id": "default", "generation": 1,
        "feeder_session_id": "f", "feeder_epoch": 1, "source_id": "s",
        "source_epoch": "e", "events": events,
    })
}

fn event(sequence: u64, event_id: &str, prev_position: &str) -> Value {
    json!({
        "event_id": event_id, "delivery_sequence": sequence,
        "prev_delivery_sequence": sequence - 1,
        "source_position": format!("p{sequence}"), "prev_source_position": prev_position,
        "op": "progress",
    })
}

/// POSTs `body` to `/v1/feeder/{route}` with the instance-ID header and an
/// optional `If-Match`; returns the status and the raw body text.
async fn feeder(
    app: &Router,
    instance: &str,
    route: &str,
    body: &Value,
    if_match: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/v1/feeder/{route}"))
        .header("content-type", "application/json")
        .header("x-knotrel-instance-id", instance);
    if let Some(if_match) = if_match {
        request = request.header("if-match", if_match);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

const MARKER: &str = "zq-echo-marker-7f3a";

fn assert_not_echoed(case: &str, status: StatusCode, body: &str, code: &str) {
    assert!(status.is_client_error(), "{case}: {status} {body}");
    let value: Value = serde_json::from_str(body).unwrap();
    assert_eq!(value["code"], code, "{case}: {body}");
    assert!(!body.contains(MARKER), "{case} echoed client input: {body}");
}

/// Feeder error messages may quote server state (the applied position, the
/// server's identifiers) but never a client-supplied string (T1.9,
/// `docs/http-api.md` "Client error messages never quote request input").
#[tokio::test]
async fn feeder_errors_never_echo_client_supplied_strings() {
    // Paths that leave the session synchronizable share one router.
    let (app, instance) = bootstrapped_feeder().await;

    let mut wrong_import = import_body(&instance);
    wrong_import["mode"] = json!("replace");
    let (status, body) = feeder(
        &app,
        &instance,
        "import",
        &wrong_import,
        Some(&format!("\"1.0.{MARKER}\"")),
    )
    .await;
    assert_not_echoed("If-Match", status, &body, "precondition_failed");

    let mut wrong_instance = import_body(&instance);
    wrong_instance["instance_id"] = json!(MARKER);
    let (status, body) = feeder(&app, &instance, "import", &wrong_instance, None).await;
    assert_not_echoed("import instance_id", status, &body, "stale_instance_id");

    let mut wrong_graph = import_body(&instance);
    wrong_graph["mode"] = json!("replace");
    wrong_graph["graph_id"] = json!(MARKER);
    let (status, body) = feeder(&app, &instance, "import", &wrong_graph, None).await;
    assert_not_echoed("import graph_id", status, &body, "invalid_graph_id");

    let mut wrong_source = events_body(&instance, json!([event(1, "e1", "p0")]));
    wrong_source["source_id"] = json!(MARKER);
    let (status, body) = feeder(&app, &instance, "events", &wrong_source, None).await;
    assert_not_echoed("source_id", status, &body, "source_mismatch");

    let mut unknown_op = event(1, "e1", "p0");
    unknown_op["op"] = json!(MARKER);
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([unknown_op])),
        None,
    )
    .await;
    assert_not_echoed("unknown op", status, &body, "invalid_request");

    let mut wrong_graph_events = events_body(&instance, json!([event(1, "e1", "p0")]));
    wrong_graph_events["graph_id"] = json!(MARKER);
    let (status, body) = feeder(&app, &instance, "events", &wrong_graph_events, None).await;
    assert_not_echoed("events graph_id", status, &body, "invalid_graph_id");

    let barrier = |position: Value| {
        json!({
            "instance_id": instance, "graph_id": "default", "generation": 1,
            "feeder_session_id": "f", "feeder_epoch": 1,
            "barrier_delivery_sequence": 0, "barrier_source_position": position,
        })
    };
    let (status, body) = feeder(&app, &instance, "barrier", &barrier(json!(MARKER)), None).await;
    assert_not_echoed(
        "barrier position",
        status,
        &body,
        "barrier_position_mismatch",
    );
    assert!(body.contains("'p0'"), "server state stays quoted: {body}");

    // Each of the following desynchronizes the graph, so each gets a fresh one.
    let (app, instance) = bootstrapped_feeder().await;
    let mut wrong_epoch = events_body(&instance, json!([event(1, "e1", "p0")]));
    wrong_epoch["source_epoch"] = json!(MARKER);
    let (status, body) = feeder(&app, &instance, "events", &wrong_epoch, None).await;
    assert_not_echoed("source_epoch", status, &body, "source_epoch_mismatch");
    assert!(body.contains("'e'"), "server state stays quoted: {body}");

    let (app, instance) = bootstrapped_feeder().await;
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([event(1, MARKER, "p0")])),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([event(2, MARKER, "p1")])),
        None,
    )
    .await;
    assert_not_echoed("repeated event_id", status, &body, "conflicting_duplicate");

    let (app, instance) = bootstrapped_feeder().await;
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([event(1, "e1", "p0")])),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([event(1, MARKER, "p0")])),
        None,
    )
    .await;
    assert_not_echoed(
        "conflicting event_id",
        status,
        &body,
        "conflicting_duplicate",
    );

    let (app, instance) = bootstrapped_feeder().await;
    let (status, body) = feeder(
        &app,
        &instance,
        "events",
        &events_body(&instance, json!([event(1, "e1", MARKER)])),
        None,
    )
    .await;
    assert_not_echoed(
        "prev_source_position",
        status,
        &body,
        "source_predecessor_mismatch",
    );
}

/// Inspection query errors name the allowed parameters, never the rejected one.
#[tokio::test]
async fn inspection_errors_never_echo_client_supplied_names() {
    let app = router_with_config(config(EngineConfig::CompactBfs).with_inspection(true));
    for path in [
        format!("/v1/nodes?{MARKER}=1"),
        format!("/v1/nodes/1?{MARKER}=1"),
        format!("/v1/nodes/1/edges?{MARKER}=1"),
        format!("/v1/edges?{MARKER}=1"),
    ] {
        let (status, bytes) = send(&app, "GET", &path, Vec::new()).await;
        let body = String::from_utf8(bytes).unwrap();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert_not_echoed(&path, status, &body, "invalid_request");
    }
}
