//! Integration tests for Feature M3.1:
//! Compatible Batch Contract and Result/Version Reporting.
//!
//! - `test_01`: one-to-one positional outcomes and ordered execution.
//! - `test_02`: revision advances once per effective mutation; no-ops and
//!   rejected operations do not advance it.
//! - `test_03`: multi-digit decimal revision carry across batches.
//! - `test_04`: queries observe preceding mutations in the same batch.
//! - `test_05`: source-managed mode rejects mutating batches and admits query-only ones.
//! - `test_06`: continue-on-error at capacity limits without rollback.
//! - `test_07`: batch isolation under concurrent batches and statistics reads.
//!   This is a scheduling-dependent stress smoke test; see its documentation.
//! - `test_08`: batch size bounds and malformed operations rejected before execution.
//! - `test_09`: client disconnect after commit preserves the applied batch.
//! - `test_10`: the source-managed mode check precedes the batch size check.
//! - `test_11`: request timeout before execution returns 503 `request_timeout`;
//!   the admitted worker applies the batch later.
//! - `test_12`: a ready source-managed query-only batch held across a concurrent
//!   replacement keeps its original results and version metadata.
//!
//! `test_09`, `test_11` and `test_12` require `test-support` and force their
//! boundaries with the worker pause hooks. Every test except `test_07` is
//! deterministic.

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

/// Stress smoke test for batch isolation; it is scheduling-dependent.
///
/// No test hook can pause a worker between two operations of one batch, so
/// this test cannot force a reader into the middle of a batch. Isolation
/// follows structurally from `Service::execute` running every operation in a
/// single closure under one graph-lock acquisition. Passing runs show only
/// that no intermediate state was observed in the interleavings that occurred;
/// they do not prove determinism.
#[tokio::test]
async fn test_07_concurrent_batches_and_snapshots_stress_smoke() {
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

#[tokio::test]
async fn test_10_source_managed_mode_check_precedes_batch_size_check() {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged);
    let app = router_with_config(config);

    // An oversized batch containing a mutation fails the mode check first.
    let oversized_mutating: Vec<Value> = (0..1025)
        .map(|i| json!({"op": "add_node", "node": i.to_string()}))
        .collect();
    let (status, headers, body) = post_batch(&app, json!(oversized_mutating)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "direct_mutation_disabled");
    assert!(headers.get("x-knotrel-state-version").is_none());

    // Query-only batches pass the mode check and then fail the size check.
    let oversized_query: Vec<Value> = (0..1025)
        .map(|_| json!({"op": "connected", "source": "1", "target": "2"}))
        .collect();
    let (status, _, body) = post_batch(&app, json!(oversized_query)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_batch_size");

    let (status, _, body) = post_batch(&app, json!([])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_batch_size");
}

/// Loopback tests that use the `test-support` worker pause hooks to force
/// specific scheduling boundaries. Every network wait is bounded by `STEP`;
/// a pause handle dropped during assertion unwinding releases its worker.
#[cfg(feature = "test-support")]
mod paused {
    use knotrel_core::EngineConfig;
    use knotrel_server::{
        Lifecycle, ServerConfig, ServerMode, ShutdownOutcome, router_with_lifecycle, serve,
    };
    use std::{net::SocketAddr, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
        time::timeout,
    };

    const STEP: Duration = Duration::from_secs(5);

    fn base_config(mode: ServerMode) -> ServerConfig {
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
            .unwrap()
            .with_mode(mode)
            .with_shutdown_timeout(STEP)
            .unwrap()
    }

    struct Running {
        address: SocketAddr,
        lifecycle: Lifecycle,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<ShutdownOutcome>,
    }

    impl Running {
        async fn start(config: ServerConfig) -> Self {
            let lifecycle = Lifecycle::new();
            let app = router_with_lifecycle(config.clone(), lifecycle.clone());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, signal) = oneshot::channel::<()>();
            let task_lifecycle = lifecycle.clone();
            let task = tokio::spawn(async move {
                serve(listener, app, &config, &task_lifecycle, async {
                    let _ = signal.await;
                })
                .await
            });
            Self {
                address,
                lifecycle,
                stop: Some(stop),
                task,
            }
        }

        async fn connect(&self) -> TcpStream {
            timeout(STEP, TcpStream::connect(self.address))
                .await
                .expect("connect timed out")
                .unwrap()
        }

        async fn instance_id(&self) -> String {
            let mut stream = self.connect().await;
            let reply = get(&mut stream, "/v1/info").await;
            assert_eq!(reply.status, 200);
            reply.json()["instance_id"].as_str().unwrap().to_string()
        }

        async fn stats(&self) -> serde_json::Value {
            let mut stream = self.connect().await;
            let reply = get(&mut stream, "/v1/stats").await;
            assert_eq!(reply.status, 200);
            reply.json()
        }

        async fn wait_for_jobs(&self, target: usize) {
            timeout(STEP, async {
                while self.lifecycle.outstanding_jobs() != target {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("outstanding jobs did not reach target");
        }

        async fn finish(mut self) -> ShutdownOutcome {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            timeout(STEP, self.task).await.unwrap().unwrap()
        }
    }

    #[derive(Debug)]
    struct Reply {
        status: u16,
        headers: String,
        body: String,
    }

    impl Reply {
        fn json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).unwrap_or_else(|_| {
                panic!(
                    "expected JSON, got {} with body: {}",
                    self.status, self.body
                )
            })
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
            })
        }
    }

    async fn read_reply(stream: &mut TcpStream) -> Option<Reply> {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let head_end = loop {
            if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let headers = String::from_utf8(buffer[..head_end].to_vec()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut body = buffer[head_end..].to_vec();
        while body.len() < length {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        let status = headers.split_whitespace().nth(1)?.parse().ok()?;
        Some(Reply {
            status,
            headers,
            body: String::from_utf8(body).unwrap(),
        })
    }

    async fn bounded_reply(stream: &mut TcpStream) -> Reply {
        timeout(STEP, read_reply(stream))
            .await
            .expect("response timed out")
            .expect("connection closed without a response")
    }

    async fn send_post(stream: &mut TcpStream, path: &str, headers: &[(&str, &str)], body: &str) {
        let mut head =
            format!("POST {path} HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("content-length: {}\r\n\r\n", body.len()));
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    }

    async fn post(
        stream: &mut TcpStream,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Reply {
        send_post(stream, path, headers, body).await;
        bounded_reply(stream).await
    }

    async fn get(stream: &mut TcpStream, path: &str) -> Reply {
        let request = format!("GET {path} HTTP/1.1\r\nhost: knotrel\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        bounded_reply(stream).await
    }

    const MIXED_BATCH: &str = r#"{"operations":[
        {"op":"add_node","node":"1"},
        {"op":"link","source":"5","target":"5"},
        {"op":"add_node","node":"2"},
        {"op":"link","source":"1","target":"2"}
    ]}"#;

    #[tokio::test]
    async fn test_09_client_disconnect_after_commit_preserves_applied_batch() {
        let running = Running::start(base_config(ServerMode::Standalone)).await;

        // The worker pauses after it has applied the batch and released the
        // graph lock, but before the response can be delivered.
        let mut pause = running.lifecycle.pause_next_worker_after_lock();
        let mut stream = running.connect().await;
        send_post(&mut stream, "/v1/batch", &[], MIXED_BATCH).await;
        timeout(STEP, pause.wait_started())
            .await
            .expect("batch worker did not reach the post-lock pause");

        // The client abandons the request; the committed mutations are already visible.
        drop(stream);
        let stats = running.stats().await;
        assert_eq!(stats["state_version"], "3");
        assert_eq!(stats["node_count"], 2);
        assert_eq!(stats["edge_count"], 1);
        assert_eq!(running.lifecycle.outstanding_jobs(), 1);

        pause.release();
        running.wait_for_jobs(0).await;
        let stats = running.stats().await;
        assert_eq!(stats["state_version"], "3");

        assert_eq!(running.finish().await, ShutdownOutcome::Drained);
    }

    #[tokio::test]
    async fn test_11_request_timeout_before_execution_has_unknown_effect() {
        let config = base_config(ServerMode::Standalone)
            .with_request_timeout(Duration::from_millis(100))
            .unwrap();
        let running = Running::start(config).await;

        // The worker is admitted and pauses before acquiring the graph lock.
        let mut pause = running.lifecycle.pause_next_worker();
        let mut stream = running.connect().await;
        send_post(&mut stream, "/v1/batch", &[], MIXED_BATCH).await;
        timeout(STEP, pause.wait_started())
            .await
            .expect("batch worker did not reach the pre-lock pause");

        let reply = bounded_reply(&mut stream).await;
        assert_eq!(reply.status, 503);
        assert_eq!(reply.json()["code"], "request_timeout");
        assert!(reply.header("x-knotrel-state-version").is_none());

        // At timeout the batch has not executed, yet its worker remains admitted.
        let stats = running.stats().await;
        assert_eq!(stats["state_version"], "0");
        assert_eq!(stats["node_count"], 0);
        assert_eq!(running.lifecycle.outstanding_jobs(), 1);

        // Once released, the worker finishes later and applies the whole batch.
        pause.release();
        running.wait_for_jobs(0).await;
        let stats = running.stats().await;
        assert_eq!(stats["state_version"], "3");
        assert_eq!(stats["node_count"], 2);
        assert_eq!(stats["edge_count"], 1);

        assert_eq!(running.finish().await, ShutdownOutcome::Drained);
    }

    #[tokio::test]
    async fn test_12_ready_query_batch_keeps_original_state_across_replacement() {
        let running = Running::start(base_config(ServerMode::SourceManaged)).await;
        let inst = running.instance_id().await;
        let id_header = [("x-knotrel-instance-id", inst.as_str())];
        let mut feeder = running.connect().await;

        let session = post(
            &mut feeder,
            "/v1/feeder/session",
            &id_header,
            &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-batch","feeder_epoch":1,"lease_duration_ms":30000}}"#),
        )
        .await;
        assert_eq!(session.status, 200);

        let import = post(
            &mut feeder,
            "/v1/feeder/import",
            &id_header,
            &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"src-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-batch","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#),
        )
        .await;
        assert_eq!(import.status, 201);
        let etag_g1 = import.header("etag").unwrap().to_string();
        assert_eq!(etag_g1, "\"1.0.pos-1\"");

        let barrier = post(
            &mut feeder,
            "/v1/feeder/barrier",
            &id_header,
            &format!(r#"{{"instance_id":"{inst}","graph_id":"default","generation":1,"feeder_session_id":"f-batch","feeder_epoch":1,"barrier_delivery_sequence":0,"barrier_source_position":"pos-1"}}"#),
        )
        .await;
        assert_eq!(barrier.status, 200);
        assert_eq!(barrier.json()["status"], "ready");

        let batch = r#"{"operations":[
            {"op":"connected","source":"1","target":"2"},
            {"op":"connected","source":"3","target":"4"}
        ]}"#;

        // Hold the batch response after its results and metadata were captured
        // under the graph lock and the lock was released.
        let mut pause = running.lifecycle.pause_next_worker_after_lock();
        let mut held = running.connect().await;
        send_post(&mut held, "/v1/batch", &[], batch).await;
        timeout(STEP, pause.wait_started())
            .await
            .expect("batch worker did not reach the post-lock pause");

        let replace = post(
            &mut feeder,
            "/v1/feeder/import",
            &[id_header[0], ("if-match", etag_g1.as_str())],
            &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"src-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-batch","feeder_epoch":1,"nodes":["1","2","3","4"],"edges":[{{"source":"3","target":"4"}}]}}"#),
        )
        .await;
        assert_eq!(replace.status, 201);
        assert_eq!(replace.header("x-knotrel-generation"), Some("2"));

        pause.release();
        let reply = bounded_reply(&mut held).await;
        assert_eq!(reply.status, 200);
        let results = reply.json()["results"].as_array().unwrap().clone();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["result"], "connected");
        assert_eq!(results[0]["connected"], true);
        assert_eq!(results[1]["result"], "error");
        assert_eq!(results[1]["code"], "unknown_node");
        assert_eq!(reply.header("x-knotrel-instance-id"), Some(inst.as_str()));
        assert_eq!(reply.header("x-knotrel-graph-id"), Some("default"));
        assert_eq!(reply.header("x-knotrel-generation"), Some("1"));
        assert_eq!(reply.header("x-knotrel-state-version"), Some("0"));
        assert_eq!(reply.header("x-knotrel-source-position"), Some("pos-1"));
        assert_eq!(reply.header("x-knotrel-sync-status"), Some("ready"));
        assert_eq!(reply.header("etag"), Some(etag_g1.as_str()));

        // A batch issued after publication observes the replacement.
        let fresh = post(&mut feeder, "/v1/batch", &[], batch).await;
        assert_eq!(fresh.status, 200);
        let results = fresh.json()["results"].as_array().unwrap().clone();
        assert_eq!(results[0]["connected"], false);
        assert_eq!(results[1]["connected"], true);
        assert_eq!(fresh.header("x-knotrel-generation"), Some("2"));
        assert_eq!(fresh.header("x-knotrel-source-position"), Some("pos-2"));
        assert_eq!(fresh.header("etag"), Some("\"2.0.pos-2\""));

        assert_eq!(running.finish().await, ShutdownOutcome::Drained);
    }
}
