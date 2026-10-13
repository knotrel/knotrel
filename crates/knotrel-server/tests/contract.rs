//! Protocol-level contract tests for the v0.1.0 HTTP service.
//!
//! Each test pins behavior documented in `docs/http-api.md` so the document and
//! the implementation cannot drift apart silently.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use knotrel_core::{EngineConfig, Graph};
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

fn app_for(engine: EngineConfig) -> Router {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 32).unwrap();
    router_with_config(config)
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

async fn raw_json(app: &Router, path: &str, body: &[u8]) -> (StatusCode, Value) {
    let (status, _, bytes) = send(app, "POST", path, Some("application/json"), body.to_vec()).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn op(app: &Router, value: Value) -> (StatusCode, Value) {
    raw_json(app, "/v1/operations", value.to_string().as_bytes()).await
}

fn changed(value: bool) -> (StatusCode, Value) {
    (StatusCode::OK, json!({"result":"changed","changed":value}))
}

fn connected(value: bool) -> (StatusCode, Value) {
    (
        StatusCode::OK,
        json!({"result":"connected","connected":value}),
    )
}

#[tokio::test]
async fn operation_semantics_are_identical_for_every_engine() {
    for engine in ENGINES {
        let app = app_for(engine);
        let add = |node: &str| json!({"op":"add_node","node":node});
        let link = |a: &str, b: &str| json!({"op":"link","source":a,"target":b});
        let cut = |a: &str, b: &str| json!({"op":"cut","source":a,"target":b});
        let query = |a: &str, b: &str| json!({"op":"connected","source":a,"target":b});

        // Vertex creation is idempotent and a vertex connects to itself.
        assert_eq!(op(&app, add("5")).await, changed(true), "{engine:?}");
        assert_eq!(op(&app, add("5")).await, changed(false), "{engine:?}");
        assert_eq!(op(&app, query("5", "5")).await, connected(true));

        // Reversed and repeated edges are the same undirected edge; leading
        // zeroes name the same vertex.
        assert_eq!(op(&app, link("1", "2")).await, changed(true));
        assert_eq!(op(&app, link("2", "1")).await, changed(false));
        assert_eq!(op(&app, link("001", "0002")).await, changed(false));
        assert_eq!(op(&app, query("2", "1")).await, connected(true));
        assert_eq!(op(&app, query("1", "5")).await, connected(false));

        // Edges are transitive for connectivity; cutting a missing edge is a no-op.
        assert_eq!(op(&app, link("2", "3")).await, changed(true));
        assert_eq!(op(&app, query("1", "3")).await, connected(true));
        assert_eq!(op(&app, cut("1", "3")).await, changed(false));
        assert_eq!(op(&app, query("1", "3")).await, connected(true));
        assert_eq!(op(&app, cut("3", "2")).await, changed(true));
        assert_eq!(op(&app, cut("3", "2")).await, changed(false));
        assert_eq!(op(&app, query("1", "3")).await, connected(false));

        // Cut leaves vertices in place, and unknown cut endpoints are a no-op.
        assert_eq!(op(&app, query("3", "3")).await, connected(true));
        assert_eq!(op(&app, cut("40", "41")).await, changed(false));
        assert_eq!(op(&app, cut("40", "1")).await, changed(false));
        assert_eq!(
            op(&app, query("40", "40")).await.0,
            StatusCode::NOT_FOUND,
            "cut must not create vertices"
        );

        // Self-loops are rejected without creating the vertex.
        for request in [link("9", "9"), cut("9", "9"), cut("1", "1")] {
            let (status, body) = op(&app, request).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body["code"], "self_loop");
        }
        assert_eq!(op(&app, query("9", "9")).await.0, StatusCode::NOT_FOUND);

        // Unknown query endpoints report the first unknown endpoint, source first.
        let (status, body) = op(&app, query("70", "80")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "unknown_node");
        assert_eq!(body["message"], "unknown node: 70");
        let (status, body) = op(&app, query("1", "80")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["message"], "unknown node: 80");
    }
}

#[tokio::test]
async fn malformed_json_and_invalid_shapes_use_stable_error_messages() {
    let app = app_for(EngineConfig::CompactBfs);
    let syntax_error = json!({
        "result":"error","code":"invalid_request","message":"request body is not valid JSON"
    });
    for path in ["/v1/operations", "/v1/batch"] {
        for body in [
            &b""[..],
            b"{",
            b"{\"op\":\"add_node\",\"node\":\"1\"} trailing",
            b"\xff\xfe",
        ] {
            let (status, value) = raw_json(&app, path, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {body:?}");
            assert_eq!(value, syntax_error, "{path} {body:?}");
        }
    }

    let (status, body) = raw_json(&app, "/v1/batch", br#"{"operations":{}}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"],
        "request body does not match the expected shape"
    );

    let cases: [(&str, &[u8], &str); 5] = [
        ("/v1/operations", b"{}", "missing field `op`"),
        (
            "/v1/operations",
            br#"{"op":"link"}"#,
            "missing field `source`",
        ),
        (
            "/v1/operations",
            br#"{"op":"add_node","node":"1","node":"2"}"#,
            "duplicate field `node`",
        ),
        ("/v1/batch", b"{}", "missing field `operations`"),
        (
            "/v1/operations",
            br#"{"op":"add_node","node":"\u0661"}"#,
            "node IDs must be nonempty strings of ASCII decimal digits",
        ),
    ];
    for (path, body, message) in cases {
        let (status, value) = raw_json(&app, path, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(value["result"], "error");
        assert_eq!(value["code"], "invalid_request");
        assert_eq!(value["message"], message, "{body:?}");
    }

    // Parser internals, byte offsets and echoed input never reach clients.
    let (_, value) = raw_json(&app, "/v1/operations", b"\xc3\x28 secret-payload").await;
    let message = value["message"].as_str().unwrap();
    assert!(!message.contains("character"), "{message}");
    assert!(!message.contains("secret"), "{message}");
}

#[tokio::test]
async fn validation_order_is_content_type_then_size_then_parse_then_batch_size() {
    let app = app_for(EngineConfig::CompactBfs);
    let oversized = vec![b' '; MAX_BODY_BYTES + 1];

    // 1. Content type is checked before the body is read.
    let (status, _, _) = send(&app, "POST", "/v1/operations", None, oversized.clone()).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (status, _, _) = send(&app, "POST", "/v1/batch", Some("text/plain"), vec![]).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // 2. Body size is enforced before parsing, on both endpoints.
    for path in ["/v1/operations", "/v1/batch"] {
        let (status, _, bytes) = send(
            &app,
            "POST",
            path,
            Some("application/json"),
            oversized.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{path}");
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["result"], "error");
        assert_eq!(body["code"], "invalid_request");
    }

    // 3. Parsing and shape validation precede batch-size validation, so an
    // oversized batch with one malformed element reports the malformed element.
    let mut operations: Vec<Value> = (0..=MAX_BATCH_OPERATIONS)
        .map(|node| json!({"op":"add_node","node":node.to_string()}))
        .collect();
    operations.push(json!({"op":"add_node","node":1}));
    let (status, body) = op_batch(&app, &operations).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_request");

    // 4. Batch size is validated before any operation executes.
    operations.pop();
    let (status, body) = op_batch(&app, &operations).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_batch_size");
    let (status, _) = op(&app, json!({"op":"connected","source":"0","target":"0"})).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "rejected batch must not mutate"
    );

    // The maximum batch size is accepted in full.
    operations.pop();
    assert_eq!(operations.len(), MAX_BATCH_OPERATIONS);
    let (status, body) = op_batch(&app, &operations).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["results"].as_array().unwrap().len(),
        MAX_BATCH_OPERATIONS
    );
}

async fn op_batch(app: &Router, operations: &[Value]) -> (StatusCode, Value) {
    raw_json(
        app,
        "/v1/batch",
        json!({"operations": operations}).to_string().as_bytes(),
    )
    .await
}

#[tokio::test]
async fn unregistered_paths_and_methods_are_rejected_without_state_changes() {
    let app = app_for(EngineConfig::CompactBfs);
    for (method, path, expected) in [
        ("GET", "/v1/operations", StatusCode::METHOD_NOT_ALLOWED),
        ("PUT", "/v1/operations", StatusCode::METHOD_NOT_ALLOWED),
        ("GET", "/v1/batch", StatusCode::METHOD_NOT_ALLOWED),
        ("POST", "/health", StatusCode::METHOD_NOT_ALLOWED),
        ("POST", "/v1/info", StatusCode::METHOD_NOT_ALLOWED),
        ("POST", "/v1/stats", StatusCode::METHOD_NOT_ALLOWED),
        ("GET", "/ready", StatusCode::NOT_FOUND),
        ("POST", "/v1/operations/", StatusCode::NOT_FOUND),
        ("POST", "/operations", StatusCode::NOT_FOUND),
    ] {
        let body = json!({"op":"add_node","node":"1"}).to_string().into_bytes();
        let (status, _, _) = send(&app, method, path, Some("application/json"), body).await;
        assert_eq!(status, expected, "{method} {path}");
    }
    let (status, _) = op(&app, json!({"op":"connected","source":"1","target":"1"})).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "rejected routes must not mutate"
    );
}

#[tokio::test]
async fn successful_mutations_are_visible_to_every_later_request_in_one_process() {
    let app = app_for(EngineConfig::Hdt);
    let clone = app.clone();
    assert_eq!(
        op(&app, json!({"op":"link","source":"1","target":"2"})).await,
        changed(true)
    );
    assert_eq!(
        op(&clone, json!({"op":"connected","source":"1","target":"2"})).await,
        connected(true)
    );
    let (_, _, bytes) = send(&clone, "GET", "/v1/stats", None, vec![]).await;
    let stats: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stats["node_count"], 2);
    assert_eq!(stats["edge_count"], 1);
}

#[tokio::test]
async fn caller_supplied_graph_seeds_the_service_state() {
    let mut graph = Graph::new();
    graph.link(1, 2).unwrap();
    let app = knotrel_server::router(graph);
    assert_eq!(
        op(&app, json!({"op":"connected","source":"1","target":"2"})).await,
        connected(true)
    );
}
