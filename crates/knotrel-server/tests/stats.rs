//! HTTP snapshots expose coherent state without counting queries or failed work.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use knotrel_core::{EngineConfig, Graph, GraphLimits};
use knotrel_server::{ServerConfig, router, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn request(app: &Router, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = match body {
        Some(body) => Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => Request::get(path).body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    if path == "/v1/stats" {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let value = serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
        .unwrap();
    (status, value)
}

#[tokio::test]
async fn snapshots_count_only_effective_operations_for_every_engine() {
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        let app = router_with_config(
            ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 32)
                .unwrap()
                .with_graph_limits(GraphLimits {
                    max_nodes: Some(3),
                    max_edges: Some(1),
                }),
        );
        let original_info = request(&app, "/v1/info", None).await;
        assert_eq!(
            request(&app, "/v1/stats", None).await,
            (
                StatusCode::OK,
                json!({
                    "node_count":0,"edge_count":0,"max_nodes":3,"max_edges":1,
                    "remaining_nodes":3,"remaining_edges":1,"state_version":"0"
                })
            )
        );
        request(
            &app,
            "/v1/operations",
            Some(json!({"op":"link","source":"1","target":"2"})),
        )
        .await;
        assert_eq!(
            request(&app, "/v1/stats", None).await.1["state_version"],
            "1"
        );
        request(
            &app,
            "/v1/batch",
            Some(json!({"operations":[
                {"op":"link","source":"2","target":"1"},
                {"op":"add_node","node":"1"},
                {"op":"link","source":"2","target":"3"},
                {"op":"link","source":"4","target":"5"},
                {"op":"link","source":"9","target":"9"},
                {"op":"connected","source":"1","target":"2"},
                {"op":"connected","source":"9","target":"9"},
                {"op":"cut","source":"1","target":"9"},
                {"op":"cut","source":"1","target":"2"},
                {"op":"add_node","node":"3"},
                {"op":"link","source":"2","target":"3"}
            ]})),
        )
        .await;
        let expected = json!({"node_count":3,"edge_count":1,"max_nodes":3,"max_edges":1,
            "remaining_nodes":0,"remaining_edges":0,"state_version":"4"});
        assert_eq!(
            request(&app, "/v1/stats", None).await,
            (StatusCode::OK, expected.clone())
        );
        // Malformed batches reject all operations before mutation.
        request(
            &app,
            "/v1/batch",
            Some(json!({"operations":[{"op":"cut","source":"2","target":"3"},{"op":"bad"}]})),
        )
        .await;
        assert_eq!(request(&app, "/v1/stats", None).await.1, expected);
        assert_eq!(request(&app, "/v1/info", None).await, original_info);
    }
}

#[tokio::test]
async fn populated_legacy_router_starts_at_zero_and_instances_are_independent() {
    let mut graph = Graph::new();
    graph.link(10, 20).unwrap();
    graph.add_node(99);
    let app = router(graph);
    let expected = json!({"node_count":3,"edge_count":1,"max_nodes":null,"max_edges":null,
        "remaining_nodes":null,"remaining_edges":null,"state_version":"0"});
    assert_eq!(request(&app, "/v1/stats", None).await.1, expected);
    request(
        &app.clone(),
        "/v1/operations",
        Some(json!({"op":"cut","source":"10","target":"20"})),
    )
    .await;
    assert_eq!(
        request(&app, "/v1/stats", None).await.1["state_version"],
        "1"
    );
    assert_eq!(
        request(&router(Graph::new()), "/v1/stats", None).await.1["state_version"],
        "0"
    );
}

#[tokio::test]
async fn concurrent_snapshots_never_observe_half_of_an_http_batch() {
    let app = router(Graph::new());
    let mut writers = Vec::new();
    for i in 0..12_u64 {
        let app = app.clone();
        writers.push(tokio::spawn(async move {
            let a = (2 * i).to_string();
            let b = (2 * i + 1).to_string();
            let response = request(
                &app,
                "/v1/batch",
                Some(json!({"operations":[
                    {"op":"link","source":a,"target":b},
                    {"op":"cut","source":a,"target":b}
                ]})),
            )
            .await;
            assert_eq!(response.0, StatusCode::OK);
        }));
    }
    for _ in 0..24 {
        let (status, snapshot) = request(&app, "/v1/stats", None).await;
        assert_eq!(status, StatusCode::OK);
        let version = snapshot["state_version"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(version % 2, 0);
        assert_eq!(snapshot["node_count"], version);
        assert_eq!(snapshot["edge_count"], 0);
    }
    for writer in writers {
        writer.await.unwrap();
    }
    let snapshot = request(&app, "/v1/stats", None).await.1;
    assert_eq!(snapshot["state_version"], "24");
    assert_eq!(snapshot["node_count"], 24);
}
