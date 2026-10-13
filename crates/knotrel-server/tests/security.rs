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
