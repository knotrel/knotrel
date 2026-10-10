//! Integration tests for the HTTP graph contract.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use knotrel_core::Graph;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn serves_mutations_and_exact_queries_with_full_width_ids() {
    let app = knotrel_server::router(Graph::new());
    let link = json!({"op":"link", "source":"0", "target":"18446744073709551615"});
    assert_eq!(
        post(&app, "/v1/operations", link.clone()).await,
        (StatusCode::OK, json!({"result":"changed", "changed":true}))
    );
    assert_eq!(post(&app, "/v1/operations", link).await.1["changed"], false);
    let query = json!({"op":"connected", "source":"18446744073709551615", "target":"0"});
    assert_eq!(
        post(&app, "/v1/operations", query.clone()).await.1["connected"],
        true
    );
    post(
        &app,
        "/v1/operations",
        json!({"op":"cut", "source":"0", "target":"18446744073709551615"}),
    )
    .await;
    assert_eq!(
        post(&app, "/v1/operations", query).await.1["connected"],
        false
    );
}

#[tokio::test]
async fn batch_preserves_order_and_continues_after_operation_errors() {
    let app = knotrel_server::router(Graph::new());
    let (status, body) = post(
        &app,
        "/v1/batch",
        json!({"operations":[
            {"op":"link", "source":"1", "target":"2"},
            {"op":"connected", "source":"1", "target":"2"},
            {"op":"link", "source":"8", "target":"8"},
            {"op":"connected", "source":"8", "target":"8"},
            {"op":"cut", "source":"2", "target":"1"},
            {"op":"connected", "source":"1", "target":"2"}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0]["changed"], true);
    assert_eq!(results[1]["connected"], true);
    assert_eq!(results[2]["code"], "self_loop");
    assert_eq!(results[3]["code"], "unknown_node");
    assert_eq!(results[4]["changed"], true);
    assert_eq!(results[5]["connected"], false);
}

#[tokio::test]
async fn malformed_batch_executes_no_operations() {
    let app = knotrel_server::router(Graph::new());
    let (status, _) = post(
        &app,
        "/v1/batch",
        json!({"operations":[
            {"op":"add_node", "node":"42"}, {"op":"add_node", "node":43}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = post(
        &app,
        "/v1/operations",
        json!({"op":"connected", "source":"42", "target":"42"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "unknown_node");
}

#[tokio::test]
async fn rejects_invalid_ids_fields_and_batch_sizes() {
    let app = knotrel_server::router(Graph::new());
    for node in [
        json!(1),
        json!("-1"),
        json!("18446744073709551616"),
        json!(""),
        json!("+1"),
        json!(" 1"),
    ] {
        assert_eq!(
            post(
                &app,
                "/v1/operations",
                json!({"op":"add_node", "node":node})
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        post(
            &app,
            "/v1/operations",
            json!({"op":"add_node", "node":"1", "typo":true})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for operations in [vec![], vec![json!({"op":"add_node", "node":"19"}); 1025]] {
        assert_eq!(
            post(&app, "/v1/batch", json!({"operations":operations}))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        post(
            &app,
            "/v1/operations",
            json!({"op":"connected", "source":"19", "target":"19"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let operations = vec![json!({"op":"add_node", "node":"19"}); 1024];
    let (status, body) = post(&app, "/v1/batch", json!({"operations":operations})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"].as_array().unwrap().len(), 1024);
}

#[tokio::test]
async fn oversized_bodies_are_rejected_before_mutation() {
    let app = knotrel_server::router(Graph::new());
    let body = format!(
        "{{\"op\":\"add_node\",\"node\":\"5\"}}{}",
        " ".repeat(1024 * 1024)
    );
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/operations")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        post(
            &app,
            "/v1/operations",
            json!({"op":"connected", "source":"5", "target":"5"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn concurrent_duplicate_links_change_the_graph_once() {
    let app = knotrel_server::router(Graph::new());
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let app = app.clone();
        requests.spawn(async move {
            post(
                &app,
                "/v1/operations",
                json!({"op":"link", "source":"1", "target":"2"}),
            )
            .await
        });
    }
    let mut changes = 0;
    while let Some(result) = requests.join_next().await {
        let (status, body) = result.unwrap();
        assert_eq!(status, StatusCode::OK);
        changes += usize::from(body["changed"].as_bool().unwrap());
    }
    assert_eq!(changes, 1);
}

async fn info(app: &Router) -> Value {
    let response = app
        .clone()
        .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap()
}

#[tokio::test]
async fn configured_engines_report_actual_settings_and_observe_every_mutation() {
    use knotrel_core::EngineConfig;
    use knotrel_server::{ServerConfig, router_with_config};
    for (engine, name, experimental) in [
        (EngineConfig::CompactBfs, "compact-bfs", false),
        (EngineConfig::EulerTour, "ett", true),
        (EngineConfig::Hdt, "hdt", true),
    ] {
        let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 1).unwrap();
        let app = router_with_config(config);
        assert_eq!(
            info(&app).await,
            json!({
                "engine":name, "experimental":experimental,
                "max_pending_jobs":1, "max_body_bytes":1048576, "max_batch_operations":1024,
                "max_nodes":null, "max_edges":null
            })
        );
        let (status, body) = post(
            &app,
            "/v1/batch",
            json!({"operations":[
                {"op":"link","source":"0","target":"18446744073709551615"},
                {"op":"link","source":"0","target":"1"},
                {"op":"link","source":"1","target":"18446744073709551615"},
                {"op":"cut","source":"0","target":"18446744073709551615"},
                {"op":"connected","source":"0","target":"18446744073709551615"},
                {"op":"cut","source":"0","target":"1"},
                {"op":"connected","source":"0","target":"18446744073709551615"},
                {"op":"link","source":"7","target":"7"},
                {"op":"connected","source":"7","target":"7"}
            ]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["results"][4]["connected"], true);
        assert_eq!(body["results"][6]["connected"], false);
        assert_eq!(body["results"][7]["code"], "self_loop");
        assert_eq!(body["results"][8]["code"], "unknown_node");
        // A per-request engine override is rejected before the link executes.
        assert_eq!(
            post(
                &app,
                "/v1/operations",
                json!({
                    "op":"link","source":"0","target":"1","engine":"ett"
                })
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        for _ in 0..3 {
            assert_eq!(
                post(
                    &app,
                    "/v1/operations",
                    json!({
                        "op":"connected","source":"0","target":"1"
                    })
                )
                .await
                .1["connected"],
                false
            );
        }
        assert_eq!(info(&app).await["engine"], name);
    }
}

#[tokio::test]
async fn legacy_router_retains_populated_state_and_reports_defaults() {
    let mut graph = Graph::new();
    graph.add_node(99);
    graph.link(1, 2).unwrap();
    let app = knotrel_server::router(graph);
    assert_eq!(info(&app).await["engine"], "compact-bfs");
    assert_eq!(info(&app).await["max_pending_jobs"], 32);
    for (source, target) in [("99", "99"), ("1", "2")] {
        assert_eq!(
            post(
                &app,
                "/v1/operations",
                json!({
                    "op":"connected","source":source,"target":target
                })
            )
            .await
            .1["connected"],
            true
        );
    }
}

#[test]
fn invalid_admission_limits_are_rejected_before_router_construction() {
    use knotrel_core::EngineConfig;
    use knotrel_server::ServerConfig;
    for capacity in [0, usize::MAX] {
        assert!(
            ServerConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                EngineConfig::CompactBfs,
                capacity
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn graph_limits_apply_to_batches_and_concurrent_requests() {
    use knotrel_core::{EngineConfig, GraphLimits};
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        let config = knotrel_server::ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 32)
            .unwrap()
            .with_graph_limits(GraphLimits {
                max_nodes: Some(3),
                max_edges: Some(1),
            });
        let app = knotrel_server::router_with_config(config);
        let response = app
            .clone()
            .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let info: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(info["max_nodes"], 3);
        assert_eq!(info["max_edges"], 1);
        let (a, b) = tokio::join!(
            post(
                &app,
                "/v1/operations",
                json!({"op":"link","source":"1","target":"2"})
            ),
            post(
                &app,
                "/v1/operations",
                json!({"op":"link","source":"1","target":"3"})
            )
        );
        assert_eq!(
            [a.0, b.0]
                .into_iter()
                .filter(|s| *s == StatusCode::OK)
                .count(),
            1
        );
        let loser = if a.0 == StatusCode::OK { "3" } else { "2" };
        let winner = if a.0 == StatusCode::OK { "2" } else { "3" };
        let error = if a.0 == StatusCode::OK { b } else { a };
        assert_eq!(error.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.1["code"], "edge_limit_exceeded");
        let (status, body) = post(
            &app,
            "/v1/batch",
            json!({"operations":[
                {"op":"connected","source":loser,"target":loser},
                {"op":"link","source":winner,"target":"1"},
                {"op":"link","source":"4","target":"5"},
                {"op":"cut","source":"1","target":winner},
                {"op":"link","source":"1","target":loser},
                {"op":"add_node","node":"4"},
                {"op":"connected","source":"1","target":loser}
            ]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let results = &body["results"];
        assert_eq!(results[0]["code"], "unknown_node");
        assert_eq!(results[1]["changed"], false);
        assert_eq!(results[2]["code"], "node_limit_exceeded");
        assert_eq!(results[3]["changed"], true);
        assert_eq!(results[4]["changed"], true);
        assert_eq!(results[5]["code"], "node_limit_exceeded");
        assert_eq!(results[6]["connected"], true);
    }
}

#[tokio::test]
async fn content_type_requires_a_json_media_type_before_mutation() {
    for path in ["/v1/operations", "/v1/batch"] {
        for (content_type, accepted) in [
            (None, false),
            (Some("application/jsonp"), false),
            (Some("application/json-invalid"), false),
            (Some("text/plain; note=+json"), false),
            (Some("text/example+json"), false),
            (Some("application/json; broken"), false),
            (Some("application/json"), true),
            (Some("Application/JSON"), true),
            (Some("application/json; charset=utf-8"), true),
            (Some("application/vnd.knotrel+json; charset=utf-8"), true),
        ] {
            let app = knotrel_server::router(Graph::new());
            let operation = json!({"op":"add_node", "node":"42"});
            let body = if path == "/v1/batch" {
                json!({"operations":[operation]})
            } else {
                operation
            };
            let mut request = Request::post(path);
            if let Some(value) = content_type {
                request = request.header("content-type", value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if accepted {
                    StatusCode::OK
                } else {
                    StatusCode::UNSUPPORTED_MEDIA_TYPE
                },
                "{path} {content_type:?}"
            );
            if !accepted {
                let body: Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                        .unwrap();
                assert_eq!(body["code"], "invalid_request");
            }
            let response = app
                .oneshot(Request::get("/v1/stats").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let stats: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(stats["node_count"], usize::from(accepted));
            assert_eq!(stats["state_version"], if accepted { "1" } else { "0" });
        }
    }
}
