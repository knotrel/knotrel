//! Deterministic integration tests for Feature M3.1:
//! Compatible Batch Contract and Result/Version Reporting.
//!
//! Validates:
//! 1. One-to-one positional outcome mapping and ordered execution.
//! 2. Continue-on-error semantics preserving earlier and later mutations without rollback.
//! 3. Revision progression arithmetic where Delta V = count(changed: true).
//! 4. Multi-digit decimal string revision carry across batches.
//! 5. Intra-batch sequential visibility of prior mutations to subsequent queries.
//! 6. Concurrency isolation between batches and live statistics snapshots.
//! 7. Source-managed mode rejection of mutating batches and allowance of query-only batches.
//! 8. Batch boundary validation (empty, 1024 max, 1025 oversized, malformed).
//! 9. Timeout and cancellation uncertainty where admitted workers complete in the background.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use knotrel_core::{EngineConfig, GraphLimits};
use knotrel_server::{ServerConfig, ServerMode, router, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post_batch(
    app: &axum::Router,
    operations: Value,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/batch")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "operations": operations })).unwrap(),
        ))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, json_body)
}

async fn get_stats(app: &axum::Router) -> (StatusCode, axum::http::HeaderMap, Value) {
    let req = Request::builder()
        .method("GET")
        .uri("/v1/stats")
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_body: Value = serde_json::from_slice(&bytes).unwrap();
    (status, headers, json_body)
}

#[tokio::test]
async fn test_01_batch_outcome_one_to_one_mapping_and_order() {
    let config =
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32).unwrap();
    let app = router_with_config(config);

    // Initial stats version is 0
    let (s_status, _, s_body) = get_stats(&app).await;
    assert_eq!(s_status, StatusCode::OK);
    assert_eq!(s_body["state_version"], "0");

    let operations = json!([
        {"op": "add_node", "node": "1"},
        {"op": "link", "source": "1", "target": "2"},
        {"op": "link", "source": "1", "target": "2"},
        {"op": "connected", "source": "1", "target": "2"},
        {"op": "cut", "source": "1", "target": "2"},
        {"op": "connected", "source": "1", "target": "2"},
        {"op": "cut", "source": "1", "target": "2"},
        {"op": "link", "source": "3", "target": "3"},
        {"op": "connected", "source": "3", "target": "3"},
        {"op": "add_node", "node": "4"}
    ]);

    let (status, headers, body) = post_batch(&app, operations).await;
    assert_eq!(status, StatusCode::OK);

    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        10,
        "each operation must have a 1:1 corresponding outcome"
    );

    // 0: add_node 1 -> changed: true
    assert_eq!(results[0], json!({"result": "changed", "changed": true}));
    // 1: link 1-2 -> changed: true (endpoints created, edge inserted)
    assert_eq!(results[1], json!({"result": "changed", "changed": true}));
    // 2: duplicate link 1-2 -> changed: false (no-op)
    assert_eq!(results[2], json!({"result": "changed", "changed": false}));
    // 3: query connected 1-2 -> connected: true
    assert_eq!(
        results[3],
        json!({"result": "connected", "connected": true})
    );
    // 4: cut 1-2 -> changed: true (edge removed)
    assert_eq!(results[4], json!({"result": "changed", "changed": true}));
    // 5: query connected 1-2 -> connected: false
    assert_eq!(
        results[5],
        json!({"result": "connected", "connected": false})
    );
    // 6: redundant cut 1-2 -> changed: false (missing edge no-op)
    assert_eq!(results[6], json!({"result": "changed", "changed": false}));
    // 7: self-loop link 3-3 -> error self_loop
    assert_eq!(results[7]["result"], "error");
    assert_eq!(results[7]["code"], "self_loop");
    // 8: query connected 3-3 -> error unknown_node (node 3 was not created because link 3-3 failed)
    assert_eq!(results[8]["result"], "error");
    assert_eq!(results[8]["code"], "unknown_node");
    // 9: add_node 4 -> changed: true (succeeds after domain errors; continue-on-error verified)
    assert_eq!(results[9], json!({"result": "changed", "changed": true}));

    // Effective mutations:
    // op 0 (true) + op 1 (true) + op 4 (true) + op 9 (true) = 4 effective mutations.
    // Total revision advance = 4. Final state_version = 4.
    assert_eq!(headers.get("x-knotrel-state-version").unwrap(), "4");
    assert_eq!(headers.get("x-knotrel-generation").unwrap(), "0");
    assert_eq!(headers.get("etag").unwrap(), "\"0.4.\"");

    // Live stats confirms exact node and edge count
    let (_, _, s_after) = get_stats(&app).await;
    assert_eq!(s_after["state_version"], "4");
    assert_eq!(s_after["node_count"], 3); // nodes 1, 2, 4 (node 3 was never created)
    assert_eq!(s_after["edge_count"], 0); // edge 1-2 was linked then cut
}

#[tokio::test]
async fn test_02_batch_revision_progression_and_effective_mutations() {
    let config =
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32).unwrap();
    let app = router_with_config(config);

    // Initial revision: 0
    let (_, _, s0) = get_stats(&app).await;
    let v_start: u64 = s0["state_version"].as_str().unwrap().parse().unwrap();
    assert_eq!(v_start, 0);

    // Batch 1: 3 effective mutations, 2 no-ops, 1 query, 1 domain error
    let ops1 = json!([
        {"op": "add_node", "node": "10"},
        {"op": "add_node", "node": "10"}, // no-op
        {"op": "add_node", "node": "20"},
        {"op": "link", "source": "10", "target": "20"},
        {"op": "link", "source": "10", "target": "20"}, // no-op
        {"op": "connected", "source": "10", "target": "20"}, // query
        {"op": "link", "source": "50", "target": "50"} // error
    ]);

    let (status1, headers1, body1) = post_batch(&app, ops1).await;
    assert_eq!(status1, StatusCode::OK);

    let results1 = body1["results"].as_array().unwrap();
    let effective_count1 = results1
        .iter()
        .filter(|r| {
            r.get("result") == Some(&json!("changed")) && r.get("changed") == Some(&json!(true))
        })
        .count() as u64;
    assert_eq!(effective_count1, 3);

    let v_final1: u64 = headers1
        .get("x-knotrel-state-version")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(v_final1, v_start + effective_count1);
    assert_eq!(v_final1, 3);

    // Batch 2: Advance from 3 to 10 via 7 effective operations
    let ops2 = json!([
        {"op": "add_node", "node": "101"},
        {"op": "add_node", "node": "102"},
        {"op": "add_node", "node": "103"},
        {"op": "add_node", "node": "104"},
        {"op": "add_node", "node": "105"},
        {"op": "add_node", "node": "106"},
        {"op": "add_node", "node": "107"}
    ]);
    let (status2, headers2, body2) = post_batch(&app, ops2).await;
    assert_eq!(status2, StatusCode::OK);
    let results2 = body2["results"].as_array().unwrap();
    let effective_count2 = results2
        .iter()
        .filter(|r| {
            r.get("result") == Some(&json!("changed")) && r.get("changed") == Some(&json!(true))
        })
        .count() as u64;
    assert_eq!(effective_count2, 7);

    let v_final2 = headers2
        .get("x-knotrel-state-version")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(v_final2, "10");
    assert_eq!(headers2.get("etag").unwrap(), "\"0.10.\"");
}

#[tokio::test]
async fn test_03_decimal_revision_carry_across_batches() {
    let config =
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32).unwrap();
    let app = router_with_config(config);

    // Add 9 nodes in batch 1: version goes from 0 to 9
    let mut ops1 = Vec::new();
    for i in 1..=9_u64 {
        ops1.push(json!({"op": "add_node", "node": i.to_string()}));
    }
    let (status1, headers1, _) = post_batch(&app, json!(ops1)).await;
    assert_eq!(status1, StatusCode::OK);
    assert_eq!(headers1.get("x-knotrel-state-version").unwrap(), "9");

    // Add 1 node in batch 2: single carry 9 -> 10
    let (status2, headers2, _) = post_batch(&app, json!([{"op": "add_node", "node": "10"}])).await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(headers2.get("x-knotrel-state-version").unwrap(), "10");
    assert_eq!(headers2.get("etag").unwrap(), "\"0.10.\"");

    // Add 89 nodes in batch 3: version goes from 10 to 99
    let mut ops3 = Vec::new();
    for i in 11..=99_u64 {
        ops3.push(json!({"op": "add_node", "node": i.to_string()}));
    }
    let (status3, headers3, _) = post_batch(&app, json!(ops3)).await;
    assert_eq!(status3, StatusCode::OK);
    assert_eq!(headers3.get("x-knotrel-state-version").unwrap(), "99");

    // Add 1 node in batch 4: double carry 99 -> 100
    let (status4, headers4, _) = post_batch(&app, json!([{"op": "add_node", "node": "100"}])).await;
    assert_eq!(status4, StatusCode::OK);
    assert_eq!(headers4.get("x-knotrel-state-version").unwrap(), "100");
    assert_eq!(headers4.get("etag").unwrap(), "\"0.100.\"");
}

#[tokio::test]
async fn test_04_intra_batch_sequential_visibility() {
    let app = router(knotrel_core::Graph::new());

    // Intra-batch sequential visibility:
    // A query inside a batch immediately reflects preceding operations in the exact same batch.
    let ops = json!([
        {"op": "connected", "source": "100", "target": "200"}, // 0: unknown_node (neither exists)
        {"op": "link", "source": "100", "target": "200"},      // 1: creates both, adds edge
        {"op": "connected", "source": "100", "target": "200"}, // 2: connected: true (immediately visible!)
        {"op": "cut", "source": "100", "target": "200"},       // 3: removes edge
        {"op": "connected", "source": "100", "target": "200"}  // 4: connected: false (immediately visible!)
    ]);

    let (status, _, body) = post_batch(&app, ops).await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();

    assert_eq!(results[0]["result"], "error");
    assert_eq!(results[0]["code"], "unknown_node");
    assert_eq!(results[1]["changed"], true);
    assert_eq!(results[2]["connected"], true);
    assert_eq!(results[3]["changed"], true);
    assert_eq!(results[4]["connected"], false);
}

#[tokio::test]
async fn test_05_source_managed_mode_rejects_mutating_batches_allows_query_only() {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged);
    let app = router_with_config(config);

    // Case 1: Batch containing a mutation is rejected with 409 direct_mutation_disabled
    let mutating_batch = json!([
        {"op": "connected", "source": "1", "target": "2"},
        {"op": "link", "source": "1", "target": "2"}
    ]);
    let (status1, _, body1) = post_batch(&app, mutating_batch).await;
    assert_eq!(status1, StatusCode::CONFLICT);
    assert_eq!(body1["code"], "direct_mutation_disabled");

    // Case 2: Batch containing only queries in source-managed uninitialized mode
    // returns 200 with per-item uninitialized domain error under continue-on-error
    let query_batch = json!([
        {"op": "connected", "source": "1", "target": "2"}
    ]);
    let (status2, _, body2) = post_batch(&app, query_batch).await;
    assert_eq!(status2, StatusCode::OK);
    let results2 = body2["results"].as_array().unwrap();
    assert_eq!(results2[0]["result"], "error");
    assert_eq!(results2[0]["code"], "uninitialized");
}

#[tokio::test]
async fn test_06_batch_capacity_limits_continue_on_error() {
    // Configure tight limits: 3 nodes, 2 edges
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_graph_limits(GraphLimits {
            max_nodes: Some(3),
            max_edges: Some(2),
        });
    let app = router_with_config(config);

    let ops = json!([
        {"op": "add_node", "node": "1"},                       // 0: node 1/3 (changed: true)
        {"op": "add_node", "node": "2"},                       // 1: node 2/3 (changed: true)
        {"op": "add_node", "node": "3"},                       // 2: node 3/3 (changed: true)
        {"op": "add_node", "node": "4"},                       // 3: node limit exceeded!
        {"op": "link", "source": "1", "target": "2"},          // 4: edge 1/2 (changed: true)
        {"op": "link", "source": "2", "target": "3"},          // 5: edge 2/2 (changed: true)
        {"op": "link", "source": "1", "target": "3"},          // 6: edge limit exceeded!
        {"op": "cut", "source": "1", "target": "2"},           // 7: cut releases edge capacity (changed: true)
        {"op": "link", "source": "1", "target": "3"}           // 8: link now succeeds! (changed: true)
    ]);

    let (status, headers, body) = post_batch(&app, ops).await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();

    assert_eq!(results[0]["changed"], true);
    assert_eq!(results[1]["changed"], true);
    assert_eq!(results[2]["changed"], true);
    assert_eq!(results[3]["code"], "node_limit_exceeded");
    assert_eq!(results[4]["changed"], true);
    assert_eq!(results[5]["changed"], true);
    assert_eq!(results[6]["code"], "edge_limit_exceeded");
    assert_eq!(results[7]["changed"], true);
    assert_eq!(results[8]["changed"], true);

    // Total effective operations: 7 (3 nodes, 2 links, 1 cut, 1 link).
    assert_eq!(headers.get("x-knotrel-state-version").unwrap(), "7");

    let (_, _, s) = get_stats(&app).await;
    assert_eq!(s["node_count"], 3);
    assert_eq!(s["edge_count"], 2);
    assert_eq!(s["remaining_nodes"], 0);
    assert_eq!(s["remaining_edges"], 0);
}

#[tokio::test]
async fn test_07_concurrent_batches_and_snapshots_isolation() {
    let app = router(knotrel_core::Graph::new());
    let mut tasks = Vec::new();

    // Spawn 10 concurrent batch writers, each performing 2 operations: link then cut
    for i in 0..10_u64 {
        let app_clone = app.clone();
        tasks.push(tokio::spawn(async move {
            let src = (2 * i).to_string();
            let dst = (2 * i + 1).to_string();
            let ops = json!([
                {"op": "link", "source": src, "target": dst},
                {"op": "cut", "source": src, "target": dst}
            ]);
            let (status, _, body) = post_batch(&app_clone, ops).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["results"].as_array().unwrap().len(), 2);
        }));
    }

    // Simultaneously poll stats: every snapshot must observe an even version and 0 edges!
    for _ in 0..20 {
        let (status, _, s) = get_stats(&app).await;
        assert_eq!(status, StatusCode::OK);
        let version: u64 = s["state_version"].as_str().unwrap().parse().unwrap();
        // Since each batch consists of 1 link (changed: true) and 1 cut (changed: true),
        // version must always be an even number under the exclusive lock!
        assert_eq!(
            version % 2,
            0,
            "reader must never observe an intermediate batch state"
        );
        assert_eq!(
            s["edge_count"], 0,
            "edges linked must be cut within the same lock boundary"
        );
    }

    for task in tasks {
        task.await.unwrap();
    }

    // Final state: 20 effective operations (10 batches * 2)
    let (_, _, final_s) = get_stats(&app).await;
    assert_eq!(final_s["state_version"], "20");
    assert_eq!(final_s["node_count"], 20);
    assert_eq!(final_s["edge_count"], 0);
}

#[tokio::test]
async fn test_08_batch_boundary_validation_before_lock() {
    let app = router(knotrel_core::Graph::new());

    // 1. Empty operations array rejected with 422 invalid_batch_size
    let (status1, _, body1) = post_batch(&app, json!([])).await;
    assert_eq!(status1, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body1["code"], "invalid_batch_size");

    // 2. Oversized batch (> 1024 operations) rejected with 422 invalid_batch_size
    let mut too_large = Vec::with_capacity(1025);
    for i in 0..1025 {
        too_large.push(json!({"op": "add_node", "node": i.to_string()}));
    }
    let (status2, _, body2) = post_batch(&app, json!(too_large)).await;
    assert_eq!(status2, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body2["code"], "invalid_batch_size");

    // 3. Exactly 1024 operations accepted
    let mut exact_max = Vec::with_capacity(1024);
    for i in 0..1024 {
        exact_max.push(json!({"op": "add_node", "node": i.to_string()}));
    }
    let (status3, headers3, body3) = post_batch(&app, json!(exact_max)).await;
    assert_eq!(status3, StatusCode::OK);
    assert_eq!(body3["results"].as_array().unwrap().len(), 1024);
    assert_eq!(headers3.get("x-knotrel-state-version").unwrap(), "1024");

    // 4. Malformed operation within batch rejected before mutation
    let malformed = json!([
        {"op": "add_node", "node": "2000"},
        {"op": "bad_op"}
    ]);
    let (status4, _, body4) = post_batch(&app, malformed).await;
    assert_eq!(status4, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body4["code"], "invalid_request");

    // State version remains 1024; node 2000 was never added
    let (_, _, s) = get_stats(&app).await;
    assert_eq!(s["state_version"], "1024");
    assert_eq!(s["node_count"], 1024);
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn test_09_timeout_uncertainty_demonstration() {
    use knotrel_server::{Lifecycle, serve};
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };

    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_shutdown_timeout(Duration::from_secs(5))
        .unwrap();
    let lifecycle = Lifecycle::new();
    let app = knotrel_server::router_with_lifecycle(config.clone(), lifecycle.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let (stop_tx, stop_rx) = oneshot::channel();
    let srv_lifecycle = lifecycle.clone();
    let srv_task = tokio::spawn(async move {
        serve(listener, app, &config, &srv_lifecycle, async {
            let _ = stop_rx.await;
        })
        .await
    });

    // Pause next worker post lock acquisition
    let mut pause = lifecycle.pause_next_worker_after_lock();

    // Client connects and submits a batch
    let mut client = TcpStream::connect(address).await.unwrap();
    let batch_payload = json!({
        "operations": [
            {"op": "add_node", "node": "9999"}
        ]
    })
    .to_string();

    let request = format!(
        "POST /v1/batch HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        batch_payload.len(),
        batch_payload
    );
    client.write_all(request.as_bytes()).await.unwrap();

    // Wait until worker acquires lock and pauses
    pause.wait_started().await;

    // Simulate client timeout or disconnect by dropping client stream
    drop(client);

    // Unpause worker: worker completes in background
    pause.release();

    // Connect fresh client to verify that worker indeed completed despite client drop
    let mut verify_client = TcpStream::connect(address).await.unwrap();
    verify_client
        .write_all(b"GET /v1/stats HTTP/1.1\r\nhost: knotrel\r\n\r\n")
        .await
        .unwrap();

    let mut buf = vec![0u8; 1024];
    let n = verify_client.read(&mut buf).await.unwrap();
    let response_str = String::from_utf8_lossy(&buf[..n]);
    assert!(response_str.contains("200 OK"));
    assert!(
        response_str.contains("\"state_version\":\"1\""),
        "worker completed in background, confirming uncertain outcome after timeout/disconnect"
    );

    let _ = stop_tx.send(());
    srv_task.await.unwrap();
}
