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
