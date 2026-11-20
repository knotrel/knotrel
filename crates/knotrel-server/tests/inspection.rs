//! HTTP contract tests for the read-only inspection routes
//! (`KNOTREL_INSPECTION`, documented in `docs/http-api.md` "Inspection").

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use knotrel_core::EngineConfig;
use knotrel_server::{ServerConfig, ServerMode, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app(engine: EngineConfig, lifecycle: bool, inspection: bool) -> Router {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 32)
        .unwrap()
        .with_topology_lifecycle(lifecycle)
        .unwrap()
        .with_inspection(inspection);
    router_with_config(config)
}

async fn get(app: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

async fn op(app: &Router, value: Value) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/operations")
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

const MAX: &str = "18446744073709551615";
const BEYOND_JS: &str = "9007199254740993";

#[tokio::test]
async fn disabled_by_default_without_admission() {
    let app = app(EngineConfig::CompactBfs, false, false);
    for path in ["/v1/nodes", "/v1/nodes/1", "/v1/nodes/1/edges", "/v1/edges"] {
        let (status, headers, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}");
        assert_eq!(body["code"], "inspection_disabled");
        assert_eq!(headers["cache-control"], "no-store");
    }
    let (_, _, info) = get(&app, "/v1/info").await;
    assert!(info.get("inspection").is_none());
}

#[tokio::test]
async fn empty_graph_has_a_complete_empty_first_page() {
    let app = app(EngineConfig::CompactBfs, false, true);
    let (_, _, info) = get(&app, "/v1/info").await;
    assert_eq!(info["inspection"], true);
    let (status, headers, body) = get(&app, "/v1/nodes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"nodes": [], "next_after": null}));
    assert_eq!(headers["x-knotrel-state-version"], "0");
    let (_, _, edges) = get(&app, "/v1/edges").await;
    assert_eq!(
        edges,
        json!({"edges": [], "next_after_source": null, "next_after_target": null})
    );
}

#[tokio::test]
async fn nodes_page_in_ascending_id_with_exact_u64_values() {
    let app = app(EngineConfig::CompactBfs, true, true);
    op(&app, json!({"op": "link", "source": MAX, "target": "2"})).await;
    op(
        &app,
        json!({"op": "link", "source": BEYOND_JS, "target": "2"}),
    )
    .await;
    op(&app, json!({"op": "add_node", "node": "0007"})).await;
    op(
        &app,
        json!({"op": "set_node_active", "node": BEYOND_JS, "active": false}),
    )
    .await;

    let (status, headers, first) = get(&app, "/v1/nodes?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first,
        json!({
            "nodes": [
                {"node": "2", "active": true, "degree": 2},
                {"node": "7", "active": true, "degree": 0}
            ],
            "next_after": "7"
        })
    );
    // Version headers are captured with the page.
    assert_eq!(headers["x-knotrel-state-version"], "4");
    assert_eq!(headers["cache-control"], "no-store");
    let (_, _, second) = get(&app, "/v1/nodes?after=7&limit=2").await;
    assert_eq!(second["nodes"][0]["node"], BEYOND_JS);
    assert_eq!(second["nodes"][1]["node"], MAX);
    assert_eq!(second["next_after"], Value::Null);

    let (_, _, inactive) = get(&app, "/v1/nodes?state=inactive").await;
    assert_eq!(
        inactive["nodes"],
        json!([{"node": BEYOND_JS, "active": false, "degree": 1}])
    );
    let (_, _, detail) = get(&app, &format!("/v1/nodes/{MAX}")).await;
    assert_eq!(detail, json!({"node": MAX, "active": true, "degree": 1}));
    let (status, _, missing) = get(&app, "/v1/nodes/3").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "unknown_node");
}

#[tokio::test]
async fn incident_and_edge_pages_report_flags() {
    let app = app(EngineConfig::CompactBfs, true, true);
    for (a, b) in [("1", "2"), ("1", "3"), ("2", "3"), ("1", MAX)] {
        op(&app, json!({"op": "link", "source": a, "target": b})).await;
    }
    op(
        &app,
        json!({"op": "set_edge_enabled", "source": "1", "target": "2", "enabled": false}),
    )
    .await;
    op(
        &app,
        json!({"op": "set_node_active", "node": "3", "active": false}),
    )
    .await;

    let (_, _, incident) = get(&app, "/v1/nodes/1/edges?limit=2").await;
    assert_eq!(
        incident,
        json!({
            "node": {"node": "1", "active": true, "degree": 3},
            "edges": [
                {"neighbor": "2", "enabled": false, "effective": false},
                {"neighbor": "3", "enabled": true, "effective": false}
            ],
            "next_after": "3"
        })
    );
    let (_, _, rest) = get(&app, "/v1/nodes/1/edges?after=3").await;
    assert_eq!(
        rest["edges"],
        json!([{"neighbor": MAX, "enabled": true, "effective": true}])
    );
    let (_, _, effective) = get(&app, "/v1/edges?state=effective").await;
    assert_eq!(
        effective["edges"],
        json!([{"source": "1", "target": MAX, "enabled": true, "effective": true}])
    );

    // Edge cursor: every edge exactly once across pages of one.
    let mut seen = Vec::new();
    let mut path = "/v1/edges?limit=1".to_string();
    loop {
        let (_, _, page) = get(&app, &path).await;
        for edge in page["edges"].as_array().unwrap() {
            seen.push((edge["source"].clone(), edge["target"].clone()));
        }
        match (
            page["next_after_source"].as_str(),
            page["next_after_target"].as_str(),
        ) {
            (None, _) => break,
            (Some(source), None) => path = format!("/v1/edges?limit=1&after_source={source}"),
            (Some(source), Some(target)) => {
                path = format!("/v1/edges?limit=1&after_source={source}&after_target={target}");
            }
        }
    }
    assert_eq!(seen.len(), 4);
}

#[tokio::test]
async fn plain_compact_mode_lists_registration_order_and_validates_cursors() {
    let app = app(EngineConfig::CompactBfs, false, true);
    for neighbor in ["9", "1", "7"] {
        op(
            &app,
            json!({"op": "link", "source": "5", "target": neighbor}),
        )
        .await;
    }
    let (_, _, page) = get(&app, "/v1/nodes/5/edges").await;
    let neighbors: Vec<&str> = page["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| edge["neighbor"].as_str().unwrap())
        .collect();
    assert_eq!(neighbors, ["9", "1", "7"]);
    let (status, _, invalid) = get(&app, "/v1/nodes/5/edges?after=424242").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid["code"], "invalid_cursor");
    // Without lifecycle support every node is active.
    let (_, _, inactive) = get(&app, "/v1/nodes?state=inactive").await;
    assert_eq!(inactive["nodes"], json!([]));
}

#[tokio::test]
async fn experimental_engines_list_nodes_only() {
    for engine in [EngineConfig::EulerTour, EngineConfig::Hdt] {
        let app = app(engine, false, true);
        op(&app, json!({"op": "link", "source": "1", "target": "2"})).await;
        let (_, _, nodes) = get(&app, "/v1/nodes").await;
        assert_eq!(
            nodes["nodes"][0],
            json!({"node": "1", "active": true, "degree": null})
        );
        for path in ["/v1/nodes/1/edges", "/v1/edges"] {
            let (status, _, body) = get(&app, path).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["code"], "inspection_unsupported");
        }
    }
}

#[tokio::test]
async fn invalid_parameters_are_rejected_before_admission() {
    let app = app(EngineConfig::CompactBfs, true, true);
    for path in [
        "/v1/nodes?bogus=1",
        "/v1/nodes?limit=0",
        "/v1/nodes?limit=1001",
        "/v1/nodes?limit=+5",
        "/v1/nodes?after=-1",
        "/v1/nodes?after=18446744073709551616",
        "/v1/nodes?state=weird",
        "/v1/nodes?after=1&after=2",
        "/v1/nodes/abc",
        "/v1/nodes/1?x=1",
        "/v1/nodes/1/edges?state=active",
        "/v1/edges?after_target=1",
        "/v1/edges?state=inactive",
        "/v1/nodes?after=%ZZ",
    ] {
        let (status, _, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert_eq!(body["code"], "invalid_request", "{path}");
    }
}

#[tokio::test]
async fn source_managed_reads_are_available_without_mutation() {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged)
        .with_inspection(true);
    let app = router_with_config(config);
    let (status, headers, body) = get(&app, "/v1/nodes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nodes"], json!([]));
    assert_eq!(headers["x-knotrel-sync-status"], "uninitialized");
}
