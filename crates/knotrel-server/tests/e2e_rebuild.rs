//! Automated end-to-end restart, rebuild, and recovery integration tests for Feature 0.2.6.
//!
//! Validates:
//! - Complete bootstrap reaches Ready with correct topology and metadata (verified vs independent oracle).
//! - Source mutations occurring during snapshot loading are caught up without loss.
//! - No-op and progress records advance source frontier without falsely incrementing revision.
//! - Committed event with lost acknowledgment deterministically tested with local fault proxy and feeder recovery.
//! - Import with lost acknowledgment and reconciliation before further publication.
//! - Old link retry after a newer cut does not recreate the edge (critical regression).
//! - Partial batch failure with committed-prefix inspection.
//! - Unverifiable duplicate requiring safe recovery (bounded deduplication window eviction).
//! - Observable heartbeat lease management, fencing detection, and clean shutdown.
//! - Graceful restart (SIGTERM) and abrupt process crash (SIGKILL) require rediscovery and authoritative rebuilding.
//! - Newly started server remains unready (503 `uninitialized`) until synchronization is completed.
//! - Requests carrying previous instance identity are rejected (409 `stale_instance_id`).
//! - Lease expiry cannot be repaired by heartbeat alone (requires epoch bump + barrier).
//! - Declared gap followed by authoritative rebuild (`mode: "replace"`) restores correct serving.

mod common;

use common::feeder::{
    ControlledFaultProxy, ExternalFeederClient, ExternalSourceFixture, ExternalSourceOracle,
    FeederClientError, HeartbeatStatus, SourceEvent, SourceSnapshot,
};
use std::{
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::time::sleep;

/// RAII guard managing a running knotrel-server child process.
/// Ensures that child processes are always cleaned up on test completion or panic.
struct ChildServerGuard {
    child: Option<Child>,
    address: SocketAddr,
}

impl ChildServerGuard {
    fn spawn(address: SocketAddr) -> Self {
        Self::spawn_with_window_size(address, None)
    }

    fn spawn_with_window_size(address: SocketAddr, window_size: Option<usize>) -> Self {
        let binary = env!("CARGO_BIN_EXE_knotrel-server");
        let mut cmd = Command::new(binary);
        // Remove Knotrel-specific environment variables to prevent host contamination,
        // while preserving essential OS variables (SystemRoot, PATH, etc. required on Windows).
        for (key, _) in std::env::vars() {
            if key.starts_with("KNOTREL_") {
                cmd.env_remove(key);
            }
        }
        cmd.env("KNOTREL_ADDR", address.to_string())
            .env("KNOTREL_MODE", "source-managed");
        if let Some(w) = window_size {
            cmd.env("KNOTREL_DEDUP_WINDOW_SIZE", w.to_string());
        }
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn knotrel-server");

        // Immediately guarded before any startup check can fail
        Self {
            child: Some(child),
            address,
        }
    }

    async fn wait_listening(&self, timeout_dur: Duration) -> bool {
        let actual_timeout = timeout_dur.max(Duration::from_secs(10));
        let deadline = tokio::time::Instant::now() + actual_timeout;
        while tokio::time::Instant::now() < deadline {
            if let Ok(mut stream) = tokio::net::TcpStream::connect(self.address).await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let req = format!(
                    "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    self.address
                );
                let mut buf = [0_u8; 256];
                if stream.write_all(req.as_bytes()).await.is_ok()
                    && matches!(
                        stream.read(&mut buf).await,
                        Ok(n) if n > 0 && String::from_utf8_lossy(&buf[..n]).contains("200 OK")
                    )
                {
                    return true;
                }
            }
            sleep(Duration::from_millis(20)).await;
        }
        false
    }

    fn signal(&mut self, signame: &str) {
        #[cfg(unix)]
        if let Some(child) = &self.child {
            let status = Command::new("kill")
                .arg(format!("-{signame}"))
                .arg(child.id().to_string())
                .status()
                .expect("signal child process");
            assert!(status.success());
        }
        #[cfg(not(unix))]
        {
            let _ = signame;
            self.kill_abruptly();
        }
    }

    fn kill_abruptly(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildServerGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn get_free_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr")
}

#[tokio::test]
async fn test_01_initial_bootstrap_oracle_verification() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-01");
    let fixture = ExternalSourceFixture::standard();
    let mut oracle = ExternalSourceOracle::new();

    // 1. Discover instance
    let inst_id = client.discover().await.unwrap();
    assert!(!inst_id.is_empty());

    // Server starts unready
    let ready_init = client.get_ready().await.unwrap();
    assert_eq!(ready_init.status, 503);
    assert_eq!(ready_init.json()["status"], "uninitialized");

    // 2. Register session
    let sess = client.register_session(10_000).await.unwrap();
    assert_eq!(sess["result"], "session_acquired");
    client.start_heartbeat(Duration::from_millis(1_000));

    // 3. Import baseline snapshot
    let imp = client
        .import(
            "create_only",
            &fixture.source_id,
            &fixture.source_epoch,
            &fixture.snapshot,
            None,
        )
        .await
        .unwrap();
    assert_eq!(imp.status, 201);
    oracle.load_snapshot(&fixture.snapshot);

    // 4. Catch up events
    let evts = client.apply_events(&fixture.events).await.unwrap();
    assert_eq!(evts.status, 200);
    for e in &fixture.events {
        oracle.apply_event(e);
    }

    // 5. Submit barrier
    let (b_seq, b_pos) = &fixture.high_water_mark;
    let barrier = client.declare_barrier(*b_seq, b_pos).await.unwrap();
    assert_eq!(barrier.status, 200);
    assert_eq!(barrier.json()["status"], "ready");

    // 6. Verify readiness is 200 ready
    let ready = client.get_ready().await.unwrap();
    assert_eq!(ready.status, 200);
    assert_eq!(ready.json()["status"], "ready");

    // 7. Verify all connectivity pairs against independent oracle
    let check_pairs = [
        (1, 2),
        (1, 10),
        (10, 20),
        (2, 3),
        (3, 99),
        (100, 101),
        (1, 100),
        (4, 99),
        (99, 100),
    ];
    for &(u, v) in &check_pairs {
        let expected = oracle.is_connected(u, v);
        let q = client.query_connected(u, v).await.unwrap();
        assert_eq!(q.status, 200);
        assert_eq!(
            q.json()["connected"].as_bool().unwrap(),
            expected,
            "mismatch for connected({u}, {v})"
        );
        assert_eq!(q.header("x-knotrel-sync-status"), Some("ready"));
    }

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_02_noop_and_progress_records_preserve_revision() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-02");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2, 3],
        edges: vec![(1, 2)],
    };
    client
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();
    assert_eq!(client.state_version, "0");

    // Event 1: Topological link(2, 3) -> revision increments 0 -> 1
    let e1 = SourceEvent {
        event_id: "e1".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-01".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(2),
        target: Some(3),
        node: None,
    };
    client.apply_events(&[e1]).await.unwrap();
    assert_eq!(client.state_version, "1");

    // Event 2: No-op duplicate link(1, 2) -> revision remains "1"
    let e2_noop = SourceEvent {
        event_id: "e2".to_string(),
        delivery_sequence: 2,
        prev_delivery_sequence: 1,
        source_position: "pos-02".to_string(),
        prev_source_position: "pos-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };
    client.apply_events(&[e2_noop]).await.unwrap();
    assert_eq!(
        client.state_version, "1",
        "no-op link must not increment revision"
    );
    assert_eq!(
        client.applied_source_position, "pos-02",
        "frontier must advance"
    );

    // Event 3: Progress-only record -> revision remains "1"
    let e3_prog = SourceEvent {
        event_id: "e3".to_string(),
        delivery_sequence: 3,
        prev_delivery_sequence: 2,
        source_position: "pos-03".to_string(),
        prev_source_position: "pos-02".to_string(),
        op: "progress".to_string(),
        source: None,
        target: None,
        node: None,
    };
    client.apply_events(&[e3_prog]).await.unwrap();
    assert_eq!(
        client.state_version, "1",
        "progress event must not increment revision"
    );
    assert_eq!(
        client.applied_source_position, "pos-03",
        "frontier must advance"
    );

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_03_committed_event_with_lost_acknowledgment_and_feeder_recovery() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    // Controlled fault proxy between feeder and server
    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-test-03");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();

    let event = SourceEvent {
        event_id: "evt-link-1-2".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-1".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };

    // Arm the fault proxy to drop the server's HTTP response (simulating lost ACK)
    proxy.arm_drop_next_response();

    // Feeder applies event with automatic retry and reconciliation
    let res = client
        .apply_events_with_recovery(std::slice::from_ref(&event), 3)
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(proxy.dropped_response_count(), 1);

    // Verify the mutation took effect on the server despite the lost initial ACK
    client.declare_barrier(1, "pos-1").await.unwrap();
    let q = client.query_connected(1, 2).await.unwrap();
    assert_eq!(q.json()["connected"], true);

    // Subsequent exact duplicate call returns duplicate / already_applied
    let dup_res = client
        .apply_events(std::slice::from_ref(&event))
        .await
        .unwrap();
    assert_eq!(dup_res.status, 200);
    assert_eq!(dup_res.json()["result"], "duplicate");
    assert_eq!(dup_res.json()["status"], "already_applied");

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_04_old_link_retry_after_newer_cut_preserves_cut() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-04");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();

    // 1. Link 1-2 at sequence 1
    let link_evt = SourceEvent {
        event_id: "evt-link-1".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-1".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };
    client
        .apply_events(std::slice::from_ref(&link_evt))
        .await
        .unwrap();

    // 2. Cut 1-2 at sequence 2
    let cut_evt = SourceEvent {
        event_id: "evt-cut-2".to_string(),
        delivery_sequence: 2,
        prev_delivery_sequence: 1,
        source_position: "pos-2".to_string(),
        prev_source_position: "pos-1".to_string(),
        op: "cut".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };
    client.apply_events(&[cut_evt]).await.unwrap();

    // 3. Declare barrier to promote to Ready
    client.declare_barrier(2, "pos-2").await.unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // Verify 1 and 2 are disconnected
    let q1 = client.query_connected(1, 2).await.unwrap();
    assert_eq!(q1.json()["connected"], false);

    // 4. Retry old link event 1 (simulating reordered delivery)
    let retry = client.apply_events(&[link_evt]).await.unwrap();
    assert_eq!(retry.status, 200);
    assert_eq!(retry.json()["status"], "already_applied");

    // Critical assertion: edge 1-2 must STRICTLY remain absent!
    let q2 = client.query_connected(1, 2).await.unwrap();
    assert_eq!(
        q2.json()["connected"],
        false,
        "old link retry must not recreate cut edge"
    );

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_05_graceful_restart_and_abrupt_termination_require_rebuild() {
    let address = get_free_port();
    let mut server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-05");
    let fixture = ExternalSourceFixture::standard();

    // 1. Initial build to Ready
    client.sync_from_fixture(&fixture, false).await.unwrap();
    let original_inst = client.instance_id.clone();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // 2. Graceful shutdown via SIGTERM
    server.signal("TERM");
    sleep(Duration::from_millis(500)).await;

    // 3. Restart server process on same address
    server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    // 4. Discover new instance
    let new_inst = client.discover().await.unwrap();
    assert_ne!(
        new_inst, original_inst,
        "restart must produce fresh instance ID"
    );

    // 5. Newly started server starts unready (503 uninitialized)
    let ready_restart = client.get_ready().await.unwrap();
    assert_eq!(ready_restart.status, 503);
    assert_eq!(ready_restart.json()["status"], "uninitialized");

    // 6. Old instance ID requests must be rejected (409 stale_instance_id)
    let stale_req = serde_json::json!({
        "instance_id": original_inst,
        "feeder_session_id": "feeder-test-05",
        "feeder_epoch": 1,
        "lease_duration_ms": 10000
    })
    .to_string();
    let stale_res = client
        .raw_request(
            "POST",
            "/v1/feeder/session",
            &[("X-Knotrel-Instance-Id", original_inst.as_str())],
            &stale_req,
        )
        .await
        .unwrap();
    assert_eq!(stale_res.status, 409);
    assert_eq!(stale_res.json()["code"], "stale_instance_id");

    // 7. Authoritative Rebuild from external source
    client.sync_from_fixture(&fixture, false).await.unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // 8. Abrupt crash simulation via SIGKILL
    server.kill_abruptly();
    sleep(Duration::from_millis(200)).await;

    // 9. Restart after crash
    server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let crash_inst = client.discover().await.unwrap();
    assert_ne!(crash_inst, new_inst);
    assert_eq!(client.get_ready().await.unwrap().status, 503);

    // 10. Rebuild again from authoritative source
    client.sync_from_fixture(&fixture, false).await.unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    let q = client.query_connected(1, 2).await.unwrap();
    assert_eq!(q.json()["connected"], true);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_06_declared_gap_followed_by_authoritative_rebuild() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-06");
    let fixture = ExternalSourceFixture::standard();

    // 1. Initial build to Ready
    client.sync_from_fixture(&fixture, false).await.unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // 2. Feeder encounters irreparable source gap and declares gap
    let gap_res = client
        .declare_gap(50, "kafka_retention_window_expired")
        .await
        .unwrap();
    assert_eq!(gap_res.status, 200);
    assert_eq!(gap_res.json()["result"], "gap_acknowledged");
    assert_eq!(gap_res.json()["status"], "desynchronized");

    // 3. Server transitions to desynchronized; queries and events are rejected
    let ready_gap = client.get_ready().await.unwrap();
    assert_eq!(ready_gap.status, 503);
    assert_eq!(ready_gap.json()["status"], "desynchronized");

    let event_after_gap = SourceEvent {
        event_id: "evt-after-gap".to_string(),
        delivery_sequence: 8,
        prev_delivery_sequence: 7,
        source_position: "pos-08".to_string(),
        prev_source_position: "pos-0107-b1".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(20),
        node: None,
    };
    let evt_res = client.apply_events(&[event_after_gap]).await.unwrap();
    assert_eq!(evt_res.status, 409);
    assert_eq!(evt_res.json()["code"], "graph_desynchronized");

    // 4. Authoritative Rebuild via replacement import (mode: "replace")
    let new_snap = SourceSnapshot {
        position: "snap-rebuild-01".to_string(),
        nodes: vec![1, 2, 3, 4],
        edges: vec![(1, 2), (3, 4)],
    };

    let stats = client.get_stats().await.unwrap();
    let raw_etag = stats.header("ETag").expect("stats etag header");
    let etag = if raw_etag.starts_with('"') && raw_etag.ends_with('"') {
        raw_etag.to_string()
    } else {
        format!("\"{raw_etag}\"")
    };

    let rep_res = client
        .import(
            "replace",
            "src-rebuild",
            "ep-2",
            &new_snap,
            Some(etag.as_str()),
        )
        .await
        .unwrap();
    assert_eq!(rep_res.status, 201);
    assert_eq!(rep_res.json()["generation"], 2);

    // 5. Declare barrier to restore Ready
    let b = client.declare_barrier(0, "snap-rebuild-01").await.unwrap();
    assert_eq!(b.status, 200);
    assert_eq!(b.json()["status"], "ready");

    assert_eq!(client.get_ready().await.unwrap().status, 200);
    let q = client.query_connected(1, 2).await.unwrap();
    assert_eq!(q.json()["connected"], true);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_07_lease_expiry_cannot_be_repaired_by_heartbeat_alone() {
    let address = get_free_port();
    let _server = ChildServerGuard::spawn(address);
    assert!(_server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-07");

    // 1. Discover instance
    client.discover().await.unwrap();

    // 2. Register session with valid minimum lease (1000ms)
    client.register_session(1000).await.unwrap();

    // 3. Import baseline snapshot
    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![(1, 2)],
    };
    let imp = client
        .import("create_only", "src-01", "ep-01", &snap, None)
        .await
        .unwrap();
    assert_eq!(imp.status, 201);

    // 4. Declare barrier to reach Ready
    let b1 = client.declare_barrier(0, "snap-01").await.unwrap();
    assert_eq!(b1.status, 200);
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // 5. Let lease expire without sending heartbeats (> 1000ms)
    sleep(Duration::from_millis(1100)).await;

    // 6. Sending heartbeat after expiry renews deadline but returns status "stale"
    let hb = client.send_heartbeat().await.unwrap();
    assert_eq!(hb.status, 200);
    assert_eq!(hb.json()["status"], "stale");

    // Server /ready probe must report 503 (stale)
    assert_eq!(client.get_ready().await.unwrap().status, 503);

    // 7. Barrier attempt with expired epoch 1 is fenced (cannot repair ready)
    let b_fenced = client.declare_barrier(0, "snap-01").await.unwrap();
    assert_eq!(b_fenced.status, 409);
    assert_eq!(b_fenced.json()["code"], "fenced_feeder_session");

    // 8. Recovery requires bumping feeder epoch (epoch 2)
    client.feeder_epoch = 2;
    let s2 = client.register_session(10_000).await.unwrap();
    assert_eq!(s2["result"], "session_acquired");

    // Now barrier with bumped epoch 2 succeeds and restores Ready
    let b_ok = client.declare_barrier(0, "snap-01").await.unwrap();
    assert_eq!(b_ok.status, 200);
    assert_eq!(b_ok.json()["status"], "ready");
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_08_failed_domain_mutation_stops_batch_without_false_progress() {
    let address = get_free_port();
    let _server = ChildServerGuard::spawn(address);
    assert!(_server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-08");
    let mut oracle = ExternalSourceOracle::new();

    // 1. Discover and establish session
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(2_000));

    // 2. Import baseline
    let snap = SourceSnapshot {
        position: "snap-base".to_string(),
        nodes: vec![1, 2, 3, 4],
        edges: vec![(1, 2)],
    };
    oracle.load_snapshot(&snap);
    let imp = client
        .import("create_only", "src-01", "ep-01", &snap, None)
        .await
        .unwrap();
    assert_eq!(imp.status, 201);

    // 3. Send batch containing:
    //    - valid link (2, 3) (seq 1)
    //    - valid progress record (seq 2)
    //    - invalid domain mutation: self-loop link(4, 4) (seq 3)
    let events = vec![
        SourceEvent {
            event_id: "evt-01".to_string(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "pos-01".to_string(),
            prev_source_position: "snap-base".to_string(),
            op: "link".to_string(),
            source: Some(2),
            target: Some(3),
            node: None,
        },
        SourceEvent {
            event_id: "evt-02".to_string(),
            delivery_sequence: 2,
            prev_delivery_sequence: 1,
            source_position: "pos-02".to_string(),
            prev_source_position: "pos-01".to_string(),
            op: "progress".to_string(),
            source: None,
            target: None,
            node: None,
        },
        SourceEvent {
            event_id: "evt-03-bad".to_string(),
            delivery_sequence: 3,
            prev_delivery_sequence: 2,
            source_position: "pos-03".to_string(),
            prev_source_position: "pos-02".to_string(),
            op: "link".to_string(),
            source: Some(4),
            target: Some(4), // Self-loop!
            node: None,
        },
    ];

    let batch_res = client.apply_events(&events).await.unwrap();
    assert_eq!(batch_res.status, 422);

    // Parse structured partial failure diagnostics
    let diag = ExternalFeederClient::parse_partial_failure(&batch_res).unwrap();
    assert_eq!(diag.failed_batch_index, 2);
    assert_eq!(diag.failed_delivery_sequence, 3);
    assert_eq!(diag.failed_event_id, "evt-03-bad");
    assert_eq!(diag.applied_delivery_sequence, 2);
    assert_eq!(diag.applied_source_position, "pos-02");
    assert_eq!(diag.error_code, "self_loop");

    // Update oracle for events that actually succeeded (evt-01 and evt-02)
    oracle.apply_event(&events[0]);
    oracle.apply_event(&events[1]);

    // Check stats: source_position stopped at pos-02 (did NOT falsely advance to pos-03)
    let stats = client.get_stats().await.unwrap();
    assert_eq!(stats.json()["source_position"], "pos-02");

    // Edge (2, 3) was committed, self-loop was not
    let q_2_3 = client.query_connected(2, 3).await.unwrap();
    assert_eq!(q_2_3.json()["connected"], true);

    // 4. Corrected retry: resend evt-01, evt-02 (deduped as already applied), and valid evt-03 (link 3, 4)
    let corrected_events = vec![
        events[0].clone(),
        events[1].clone(),
        SourceEvent {
            event_id: "evt-03-fixed".to_string(),
            delivery_sequence: 3,
            prev_delivery_sequence: 2,
            source_position: "pos-03".to_string(),
            prev_source_position: "pos-02".to_string(),
            op: "link".to_string(),
            source: Some(3),
            target: Some(4),
            node: None,
        },
    ];

    let retry_res = client.apply_events(&corrected_events).await.unwrap();
    assert_eq!(retry_res.status, 200);
    assert_eq!(retry_res.json()["applied_delivery_sequence"], 3);
    assert_eq!(retry_res.json()["applied_source_position"], "pos-03");

    oracle.apply_event(&corrected_events[2]);

    // Declare barrier and verify against oracle
    let b = client.declare_barrier(3, "pos-03").await.unwrap();
    assert_eq!(b.status, 200);
    assert_eq!(b.json()["status"], "ready");

    let q_1_4 = client.query_connected(1, 4).await.unwrap();
    assert_eq!(q_1_4.json()["connected"], oracle.is_connected(1, 4));
    assert_eq!(q_1_4.json()["connected"], true);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_09_import_with_lost_acknowledgment_and_reconciliation() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-test-09");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-reconcile-01".to_string(),
        nodes: vec![10, 20, 30],
        edges: vec![(10, 20)],
    };

    // Arm fault proxy to drop the HTTP response for snapshot import
    proxy.arm_drop_response_matching("/v1/feeder/import").await;

    // Import with reconciliation returns explicit UncertainOutcome because counts and
    // cursor equality do not prove graph identity or attribution to this import.
    let imp_res = client
        .import_with_reconciliation("create_only", "src-09", "ep-01", &snap, None, 3)
        .await;

    assert_eq!(proxy.dropped_response_count(), 1);
    match imp_res {
        Err(FeederClientError::UncertainOutcome(msg)) => {
            assert!(
                msg.contains("uncertain") || msg.contains("attribution"),
                "expected uncertain outcome due to unverified attribution, got: {msg}"
            );
        }
        other => panic!("expected UncertainOutcome without synthesizing 201, got: {other:?}"),
    }

    // Exercise documented recovery workflow:
    // Feeder safely recovers via conditional rebuild:
    // 1. Validates ownership (heartbeat healthy).
    // 2. Obtains coherent current metadata from /v1/stats.
    // 3. Atomically replaces graph using mandatory composite If-Match precondition.
    let rebuild_res = client
        .conditional_rebuild_snapshot("src-09", "ep-01", &snap)
        .await
        .unwrap();
    assert_eq!(rebuild_res.status, 201);
    assert_eq!(rebuild_res.json()["generation"], 2);

    // Barrier succeeds and brings server to Ready
    let b = client
        .declare_barrier(0, "snap-reconcile-01")
        .await
        .unwrap();
    assert_eq!(b.status, 200);
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    let q = client.query_connected(10, 20).await.unwrap();
    assert_eq!(q.json()["connected"], true);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_10_authoritative_changes_during_snapshot_loading_catch_up_to_barrier_b() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-test-10");
    let mut oracle = ExternalSourceOracle::new();

    client.discover().await.unwrap();
    client.register_session(15_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    // Authoritative store state at snapshot time S:
    let snapshot_s = SourceSnapshot {
        position: "pos-0100-S".to_string(),
        nodes: vec![1, 2, 3],
        edges: vec![],
    };
    oracle.load_snapshot(&snapshot_s);

    // Arm proxy to pause snapshot upload while in flight
    let (started, release) = proxy.arm_pause_before_forwarding("/v1/feeder/import").await;

    // Upload snapshot S to Knotrel in a background task
    let client_mutex = Arc::new(tokio::sync::Mutex::new(client));
    let client_clone = client_mutex.clone();
    let snap_clone = snapshot_s.clone();

    let import_handle = tokio::spawn(async move {
        let mut cl = client_clone.lock().await;
        cl.import("create_only", "src-stream", "ep-10", &snap_clone, None)
            .await
    });

    // Wait until snapshot upload is in-flight and paused at the proxy
    started.notified().await;
    assert!(
        !import_handle.is_finished(),
        "import must be actively in-flight"
    );

    // While loading is actively in-flight, authoritative store commits mutations 1..5:
    let mutations_after_s = vec![
        SourceEvent {
            event_id: "evt-0101".to_string(),
            delivery_sequence: 1,
            prev_delivery_sequence: 0,
            source_position: "pos-0101".to_string(),
            prev_source_position: "pos-0100-S".to_string(),
            op: "link".to_string(),
            source: Some(1),
            target: Some(2),
            node: None,
        },
        SourceEvent {
            event_id: "evt-0102".to_string(),
            delivery_sequence: 2,
            prev_delivery_sequence: 1,
            source_position: "pos-0102".to_string(),
            prev_source_position: "pos-0101".to_string(),
            op: "link".to_string(),
            source: Some(2),
            target: Some(3),
            node: None,
        },
        SourceEvent {
            event_id: "evt-0103".to_string(),
            delivery_sequence: 3,
            prev_delivery_sequence: 2,
            source_position: "pos-0103".to_string(),
            prev_source_position: "pos-0102".to_string(),
            op: "cut".to_string(),
            source: Some(1),
            target: Some(2),
            node: None,
        },
        SourceEvent {
            event_id: "evt-0104".to_string(),
            delivery_sequence: 4,
            prev_delivery_sequence: 3,
            source_position: "pos-0104".to_string(),
            prev_source_position: "pos-0103".to_string(),
            op: "add_node".to_string(),
            source: None,
            target: None,
            node: Some(4),
        },
        SourceEvent {
            event_id: "evt-0105".to_string(),
            delivery_sequence: 5,
            prev_delivery_sequence: 4,
            source_position: "pos-0105-barrier-B".to_string(),
            prev_source_position: "pos-0104".to_string(),
            op: "link".to_string(),
            source: Some(3),
            target: Some(4),
            node: None,
        },
    ];

    for m in &mutations_after_s {
        oracle.apply_event(m);
    }

    // Verify import is STILL in-flight while authoritative mutations were committed
    assert!(
        !import_handle.is_finished(),
        "import must remain in-flight while mutations committed to authoritative oracle"
    );

    // Release the proxy to forward the snapshot import to the server and complete upload
    release.notify_one();

    let imp = import_handle.await.unwrap().unwrap();
    assert_eq!(imp.status, 201);

    let mut client = match Arc::try_unwrap(client_mutex) {
        Ok(m) => m.into_inner(),
        Err(_) => panic!("Arc references remain"),
    };

    // Server must be unready while catching up
    let r_catch = client.get_ready().await.unwrap();
    assert_eq!(r_catch.status, 503);
    assert_eq!(r_catch.json()["status"], "catching_up");

    // Catch up all mutations in order
    let res = client.apply_events(&mutations_after_s).await.unwrap();
    assert_eq!(res.status, 200);

    // Declare barrier at high-water mark B (sequence 5, pos-0105-barrier-B)
    let b = client
        .declare_barrier(5, "pos-0105-barrier-B")
        .await
        .unwrap();
    assert_eq!(b.status, 200);
    assert_eq!(b.json()["status"], "ready");

    // Verify Ready probe
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // Verify all topological pairs against independent oracle
    let pairs = [(1, 2), (2, 3), (3, 4), (2, 4), (1, 3), (1, 4)];
    for &(u, v) in &pairs {
        let expected = oracle.is_connected(u, v);
        let q = client.query_connected(u, v).await.unwrap();
        assert_eq!(q.status, 200);
        assert_eq!(
            q.json()["connected"].as_bool().unwrap(),
            expected,
            "mismatch for ({u}, {v})"
        );
    }

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_11_unverifiable_duplicate_requiring_safe_recovery() {
    let address = get_free_port();
    // Bounded deduplication window of size 5
    let server = ChildServerGuard::spawn_with_window_size(address, Some(5));
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let mut client = ExternalFeederClient::new(address, "feeder-test-11");
    client.discover().await.unwrap();
    client.register_session(15_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2, 3, 4, 5, 6, 7, 8],
        edges: vec![],
    };
    client
        .import("create_only", "src-11", "ep-01", &snap, None)
        .await
        .unwrap();

    // Stream sequences 1 through 7
    let mut events = Vec::new();
    let mut prev_pos = "snap-01".to_string();
    for seq in 1..=7_u64 {
        let cur_pos = format!("pos-{seq:02}");
        events.push(SourceEvent {
            event_id: format!("evt-{seq:02}"),
            delivery_sequence: seq,
            prev_delivery_sequence: seq - 1,
            source_position: cur_pos.clone(),
            prev_source_position: prev_pos,
            op: "link".to_string(),
            source: Some(seq),
            target: Some(seq + 1),
            node: None,
        });
        prev_pos = cur_pos;
    }

    client.apply_events(&events).await.unwrap();
    client.declare_barrier(7, "pos-07").await.unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // Sequence 1 is now evicted from the 5-entry window (7 - 5 + 1 = 3 is window min)
    let evicted_retry = events[0].clone();
    let evicted_res = client.apply_events(&[evicted_retry]).await.unwrap();
    assert_eq!(evicted_res.status, 409);
    assert_eq!(evicted_res.json()["code"], "unverifiable_duplicate");

    // Server remains Ready despite unverifiable duplicate rejection
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    // Feeder safely recovers from unverifiable duplicate via authoritative rebuild (mode: "replace")
    let rebuild_fixture = ExternalSourceFixture::standard();
    client
        .sync_from_fixture(&rebuild_fixture, true)
        .await
        .unwrap();
    assert_eq!(client.get_ready().await.unwrap().status, 200);

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_12_heartbeat_observability_fencing_and_clean_shutdown() {
    let address = get_free_port();
    let _server = ChildServerGuard::spawn(address);
    assert!(_server.wait_listening(Duration::from_secs(5)).await);

    let mut client1 = ExternalFeederClient::new(address, "feeder-primary");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();
    client1.start_heartbeat(Duration::from_millis(300));

    // Verify heartbeat is healthy
    sleep(Duration::from_millis(400)).await;
    assert!(client1.is_heartbeat_healthy());
    assert_eq!(client1.current_heartbeat_status(), HeartbeatStatus::Healthy);

    // Second competing feeder acquires session with higher epoch
    let mut client2 = ExternalFeederClient::new(address, "feeder-secondary");
    client2.discover().await.unwrap();
    client2.feeder_epoch = 2;
    let s2 = client2.register_session(10_000).await.unwrap();
    assert_eq!(s2["result"], "session_acquired");

    // Wait for client1's background heartbeat to encounter 409 fencing
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut was_fenced = false;
    while tokio::time::Instant::now() < deadline {
        match client1.current_heartbeat_status() {
            HeartbeatStatus::Fenced(_) => {
                was_fenced = true;
                break;
            }
            _ => sleep(Duration::from_millis(50)).await,
        }
    }

    assert!(was_fenced, "client1 should observe HeartbeatStatus::Fenced");
    assert!(!client1.is_heartbeat_healthy());

    // Clean shutdown is awaited without hanging
    client1.stop_heartbeat().await;
    assert_eq!(client1.current_heartbeat_status(), HeartbeatStatus::Stopped);
}

#[tokio::test]
async fn test_13_lost_ack_combined_with_replacement_before_retry() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client1 = ExternalFeederClient::new(proxy_addr, "feeder-client-01");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();
    client1.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client1
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();

    let event = SourceEvent {
        event_id: "evt-link-1-2".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-1".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };

    // Arm proxy to drop response of the first attempt
    proxy.arm_drop_next_response();

    let direct_err = client1.apply_events(std::slice::from_ref(&event)).await;
    assert!(matches!(direct_err, Err(FeederClientError::Io(_))));
    assert_eq!(proxy.dropped_response_count(), 1);

    // Before client1 retries, client2 connects and replaces graph with generation 2
    let mut client2 = ExternalFeederClient::new(address, "feeder-client-02");
    client2.discover().await.unwrap();
    client2.feeder_epoch = 2;
    client2.register_session(10_000).await.unwrap();

    let stats = client2.get_stats().await.unwrap();
    let raw_etag = stats.header("ETag").unwrap();
    let etag = if raw_etag.starts_with('"') && raw_etag.ends_with('"') {
        raw_etag.to_string()
    } else {
        format!("\"{raw_etag}\"")
    };

    let snap2 = SourceSnapshot {
        position: "snap-02".to_string(),
        nodes: vec![10, 20],
        edges: vec![],
    };
    let rep_res = client2
        .import("replace", "src-2", "ep-2", &snap2, Some(etag.as_str()))
        .await
        .unwrap();
    assert_eq!(rep_res.status, 201);
    assert_eq!(rep_res.json()["generation"], 2);

    // Now client1 attempts recovery retry under generation 1
    // Server has generation 2 and rejects with 409 stale_generation (or fenced session)
    let rec_res = client1
        .apply_events_with_recovery(std::slice::from_ref(&event), 2)
        .await;

    // Must return an explicit error (RecoveryRequired or Fenced), NEVER false success or synthesized already_applied!
    match rec_res {
        Err(FeederClientError::RecoveryRequired(_)) | Err(FeederClientError::Fenced(_)) => {}
        other => panic!("expected RecoveryRequired or Fenced on replacement, got: {other:?}"),
    }

    client1.stop_heartbeat().await;
}

#[tokio::test]
async fn test_14_lost_ack_combined_with_restart() {
    let address = get_free_port();
    let mut server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-client-14");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();
    client.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();

    let event = SourceEvent {
        event_id: "evt-link-1-2".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-1".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };

    // Proxy drops response
    proxy.arm_drop_next_response();
    let direct_err = client.apply_events(std::slice::from_ref(&event)).await;
    assert!(matches!(direct_err, Err(FeederClientError::Io(_))));

    // Server abruptly killed and restarted
    server.kill_abruptly();
    sleep(Duration::from_millis(200)).await;

    server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    // Feeder retries with old instance ID -> server responds 409 stale_instance_id
    let rec_res = client
        .apply_events_with_recovery(std::slice::from_ref(&event), 2)
        .await;

    match rec_res {
        Err(FeederClientError::StaleInstance(_)) => {}
        other => panic!("expected StaleInstance on restart, got: {other:?}"),
    }

    client.stop_heartbeat().await;
}

#[tokio::test]
async fn test_15_lost_ack_combined_with_competing_ownership_takeover() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client1 = ExternalFeederClient::new(proxy_addr, "feeder-client-15");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();
    client1.start_heartbeat(Duration::from_millis(1_000));

    let snap = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client1
        .import("create_only", "src-1", "ep-1", &snap, None)
        .await
        .unwrap();

    let event = SourceEvent {
        event_id: "evt-link-1-2".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "pos-1".to_string(),
        prev_source_position: "snap-01".to_string(),
        op: "link".to_string(),
        source: Some(1),
        target: Some(2),
        node: None,
    };

    // Proxy drops response
    proxy.arm_drop_next_response();
    let direct_err = client1.apply_events(std::slice::from_ref(&event)).await;
    assert!(matches!(direct_err, Err(FeederClientError::Io(_))));

    // Competing client2 takes over session with higher epoch (epoch 2)
    let mut client2 = ExternalFeederClient::new(address, "feeder-competing");
    client2.discover().await.unwrap();
    client2.feeder_epoch = 2;
    client2.register_session(10_000).await.unwrap();

    // Client1 retries with epoch 1 -> server returns 409 fenced_feeder_session
    let rec_res = client1
        .apply_events_with_recovery(std::slice::from_ref(&event), 2)
        .await;

    match rec_res {
        Err(FeederClientError::Fenced(_)) => {}
        other => panic!("expected Fenced on takeover, got: {other:?}"),
    }

    client1.stop_heartbeat().await;
}

#[tokio::test]
async fn test_16_import_reconciliation_existing_graph_same_cursor_different_topology() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    // Client 1 initializes server at generation 0
    let mut client1 = ExternalFeederClient::new(proxy_addr, "feeder-client-16");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();

    let snap_requested = SourceSnapshot {
        position: "snap-same-pos".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };

    // Proxy drops client1's create_only request before delivery
    proxy.arm_drop_request_matching("/v1/feeder/import").await;

    // Meanwhile, a concurrent publication commits on the server with the SAME cursor but DIFFERENT topology
    let mut concurrent_client = ExternalFeederClient::new(address, "feeder-client-concurrent");
    concurrent_client.discover().await.unwrap();
    concurrent_client.feeder_epoch = 2;
    concurrent_client.register_session(10_000).await.unwrap();

    let snap_different_topology = SourceSnapshot {
        position: "snap-same-pos".to_string(),
        nodes: vec![10, 20, 30, 40, 50],
        edges: vec![(10, 20), (20, 30), (30, 40), (40, 50)],
    };
    concurrent_client
        .import(
            "create_only",
            "src-16",
            "ep-16",
            &snap_different_topology,
            None,
        )
        .await
        .unwrap();

    // Client 1 attempts import_with_reconciliation:
    // Its request was dropped; it queries /v1/stats.
    // In stats: generation == 1 and source_position == "snap-same-pos".
    // Buggy code would synthesize success!
    // Conservative reconciliation verifies node_count (5 vs expected 2) and fails safely.
    let rec_res = client1
        .import_with_reconciliation("create_only", "src-16", "ep-16", &snap_requested, None, 1)
        .await;

    match rec_res {
        Err(FeederClientError::UncertainOutcome(msg)) => {
            assert!(
                msg.contains("topology/metadata does not match requested snapshot")
                    || msg.contains("competing session")
                    || msg.contains("publication not observed on server")
                    || msg.contains("uncertain"),
                "expected uncertain outcome with topology/session/commit mismatch, got: {msg}"
            );
        }
        Err(FeederClientError::Protocol(msg)) => {
            assert!(
                msg.contains("server generation is already 1"),
                "expected protocol rejection due to existing graph generation, got: {msg}"
            );
        }
        Err(FeederClientError::Fenced(_)) => {}
        other => panic!("expected UncertainOutcome, Protocol, or Fenced, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_17_import_reconciliation_import_never_committed() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-client-17");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();

    let snap = SourceSnapshot {
        position: "snap-uncommitted".to_string(),
        nodes: vec![1, 2, 3],
        edges: vec![(1, 2)],
    };

    // Proxy drops request before delivery to server
    proxy.arm_drop_request_matching("/v1/feeder/import").await;

    let res = client
        .import_with_reconciliation("create_only", "src-17", "ep-17", &snap, None, 1)
        .await;

    match res {
        Err(FeederClientError::UncertainOutcome(msg)) => {
            assert!(
                msg.contains("publication not observed on server"),
                "expected 'publication not observed on server' in error message, got: {msg}"
            );
        }
        other => panic!("expected UncertainOutcome, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_18_import_reconciliation_replaced_by_another_feeder_before_reconciliation() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    // Client 1 initializes graph to generation 1
    let mut client1 = ExternalFeederClient::new(proxy_addr, "feeder-client-18-a");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();

    let snap1 = SourceSnapshot {
        position: "snap-01".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };
    client1
        .import("create_only", "src-18", "ep-01", &snap1, None)
        .await
        .unwrap();

    let stats = client1.get_stats().await.unwrap();
    let raw_etag = stats.header("ETag").unwrap();
    let etag = if raw_etag.starts_with('"') && raw_etag.ends_with('"') {
        raw_etag.to_string()
    } else {
        format!("\"{raw_etag}\"")
    };

    let snap2 = SourceSnapshot {
        position: "snap-02".to_string(),
        nodes: vec![3, 4],
        edges: vec![],
    };

    // Proxy drops client1's replace request
    proxy.arm_drop_request_matching("/v1/feeder/import").await;

    // In the meantime, client2 takes over session and replaces to generation 2
    let mut client2 = ExternalFeederClient::new(address, "feeder-client-18-b");
    client2.discover().await.unwrap();
    client2.feeder_epoch = 2;
    client2.register_session(10_000).await.unwrap();

    let snap_competing = SourceSnapshot {
        position: "snap-competing".to_string(),
        nodes: vec![100, 200],
        edges: vec![],
    };
    client2
        .import(
            "replace",
            "src-18",
            "ep-02",
            &snap_competing,
            Some(etag.as_str()),
        )
        .await
        .unwrap();

    // Client 1 reconciles: sees active session belongs to client2 or generation > expected
    let res = client1
        .import_with_reconciliation("replace", "src-18", "ep-01", &snap2, Some(etag.as_str()), 1)
        .await;

    match res {
        Err(FeederClientError::Fenced(_)) | Err(FeederClientError::UncertainOutcome(_)) => {}
        other => panic!("expected Fenced or UncertainOutcome, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_19_import_reconciliation_restart_during_uncertain_outcome() {
    let address = get_free_port();
    let mut server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-client-19");
    client.discover().await.unwrap();
    client.register_session(10_000).await.unwrap();

    let snap = SourceSnapshot {
        position: "snap-restart".to_string(),
        nodes: vec![1, 2],
        edges: vec![],
    };

    // Proxy drops response for /v1/feeder/import
    proxy.arm_drop_response_matching("/v1/feeder/import").await;

    let direct_err = client
        .import("create_only", "src-19", "ep-19", &snap, None)
        .await;
    assert!(matches!(direct_err, Err(FeederClientError::Io(_))));

    server.kill_abruptly();
    sleep(Duration::from_millis(200)).await;

    server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    // Now client queries stats or calls import_with_reconciliation
    let res = client
        .import_with_reconciliation("create_only", "src-19", "ep-19", &snap, None, 1)
        .await;

    match res {
        Err(FeederClientError::RecoveryRequired(msg))
        | Err(FeederClientError::StaleInstance(msg)) => {
            assert!(
                msg.contains("server restarted") || msg.contains("stale_instance_id"),
                "expected server restart / stale instance message, got: {msg}"
            );
        }
        Err(FeederClientError::Protocol(_)) => {}
        other => panic!("expected RecoveryRequired or StaleInstance, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_20_heartbeat_failure_halts_feeder_control_flow() {
    let address = get_free_port();
    let _server = ChildServerGuard::spawn(address);
    assert!(_server.wait_listening(Duration::from_secs(5)).await);

    let mut client1 = ExternalFeederClient::new(address, "feeder-halt-01");
    client1.discover().await.unwrap();
    client1.register_session(10_000).await.unwrap();
    client1.start_heartbeat(Duration::from_millis(200));

    // Competing client takes over session with epoch 2
    let mut client2 = ExternalFeederClient::new(address, "feeder-halt-02");
    client2.discover().await.unwrap();
    client2.feeder_epoch = 2;
    client2.register_session(10_000).await.unwrap();

    // Wait for client1's heartbeat to observe 409 fencing and update status
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if matches!(
            client1.current_heartbeat_status(),
            HeartbeatStatus::Fenced(_)
        ) {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(matches!(
        client1.current_heartbeat_status(),
        HeartbeatStatus::Fenced(_)
    ));

    // Verify all control-flow operations immediately halt on fencing and refuse to continue under lost ownership:
    let snap = SourceSnapshot {
        position: "pos".to_string(),
        nodes: vec![1],
        edges: vec![],
    };
    let imp_res = client1.import("create_only", "s", "e", &snap, None).await;
    assert!(matches!(imp_res, Err(FeederClientError::Fenced(_))));

    let evt = SourceEvent {
        event_id: "e1".to_string(),
        delivery_sequence: 1,
        prev_delivery_sequence: 0,
        source_position: "p1".to_string(),
        prev_source_position: "p0".to_string(),
        op: "progress".to_string(),
        source: None,
        target: None,
        node: None,
    };
    let apply_res = client1.apply_events(std::slice::from_ref(&evt)).await;
    assert!(matches!(apply_res, Err(FeederClientError::Fenced(_))));

    let rec_res = client1.apply_events_with_recovery(&[evt], 2).await;
    assert!(matches!(rec_res, Err(FeederClientError::Fenced(_))));

    let bar_res = client1.declare_barrier(1, "p1").await;
    assert!(matches!(bar_res, Err(FeederClientError::Fenced(_))));

    client1.stop_heartbeat().await;
}

#[tokio::test]
async fn test_21_import_reconciliation_same_count_same_cursor_different_topology() {
    let address = get_free_port();
    let server = ChildServerGuard::spawn(address);
    assert!(server.wait_listening(Duration::from_secs(5)).await);

    let proxy = ControlledFaultProxy::start(address).await.unwrap();
    let proxy_addr = proxy.listen_addr();

    // Feeder A connects through the proxy at generation 0.
    // Note: Feeder A does NOT start background heartbeat so heartbeat failure detection
    // cannot mask the reconciliation defect (as required by regression specification).
    let mut client_a = ExternalFeederClient::new(proxy_addr, "feeder-client-21-a");
    client_a.discover().await.unwrap();
    client_a.register_session(10_000).await.unwrap();

    // Feeder A prepares snapshot G with nodes [1, 2, 3] and edge (1, 2).
    let snap_a = SourceSnapshot {
        position: "snap-cursor-competing".to_string(),
        nodes: vec![1, 2, 3],
        edges: vec![(1, 2)],
    };

    // Explicit test gate cleanup guard: ensures that if any assertion or timeout fails,
    // the proxy release signal is notified so background tasks do not deadlock.
    struct ReleaseOnDrop(Arc<tokio::sync::Notify>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.notify_waiters();
        }
    }

    // Atomically arm proxy to:
    // 1. Drop Feeder A's import request before delivery to server.
    // 2. Atomically gate Feeder A's subsequent reconciliation /v1/stats request so it cannot
    //    execute until explicitly released.
    // Note: Feeder A's initial pre-import stats request is NOT paused or dropped because the
    // pause gate is only installed upon intercepting the import request.
    let (dropped_notify, pause_started, pause_release) = proxy
        .arm_drop_request_and_pause_subsequent("/v1/feeder/import", "/v1/stats")
        .await;
    let _release_guard = ReleaseOnDrop(pause_release.clone());

    // Launch Feeder A's import with reconciliation in a separate task.
    let client_a_handle = tokio::spawn(async move {
        client_a
            .import_with_reconciliation("create_only", "src-21-a", "ep-21-a", &snap_a, None, 1)
            .await
    });

    // 1. A's import is intercepted and never forwarded.
    tokio::time::timeout(Duration::from_secs(5), dropped_notify.notified())
        .await
        .expect("timed out waiting for Feeder A's import request to be intercepted and dropped");
    assert_eq!(proxy.dropped_request_count(), 1);

    // 2. A cannot perform its reconciliation stats read yet.
    tokio::time::timeout(Duration::from_secs(5), pause_started.notified())
        .await
        .expect(
            "timed out waiting for Feeder A's reconciliation stats request to be held at the gate",
        );

    // 3. B publishes the competing snapshot successfully.
    // Feeder B connects directly, acquires ownership with higher epoch, and publishes snapshot H.
    // Snapshot H has the SAME cursor, SAME node count (3), SAME edge count (1), but DIFFERENT topology: (2, 3) vs (1, 2).
    let mut client_b = ExternalFeederClient::new(address, "feeder-client-21-b");
    client_b.discover().await.unwrap();
    client_b.feeder_epoch = 2;
    client_b.register_session(10_000).await.unwrap();

    let snap_b = SourceSnapshot {
        position: "snap-cursor-competing".to_string(),
        nodes: vec![1, 2, 3],
        edges: vec![(2, 3)],
    };
    let b_res = client_b
        .import("create_only", "src-21-b", "ep-21-b", &snap_b, None)
        .await
        .unwrap();
    assert_eq!(b_res.status, 201);
    assert_eq!(b_res.json()["generation"], 1);

    // 4. Assert generation 1, the shared cursor, and matching node/edge counts.
    let b_stats = client_b.get_stats().await.unwrap();
    assert_eq!(b_stats.status, 200);
    let b_stats_json = b_stats.json();
    assert_eq!(b_stats_json["generation"], 1);
    assert_eq!(b_stats_json["source_position"], "snap-cursor-competing");
    assert_eq!(b_stats_json["node_count"], 3);
    assert_eq!(b_stats_json["edge_count"], 1);

    // 5. Release A.
    pause_release.notify_waiters();

    // 6. Assert A returns UncertainOutcome specifically because publication attribution cannot be established,
    // and verify B's different topology remains live.
    let res_a = tokio::time::timeout(Duration::from_secs(5), client_a_handle)
        .await
        .expect("timed out awaiting Feeder A's task completion")
        .unwrap();
    match res_a {
        Err(FeederClientError::UncertainOutcome(msg)) => {
            assert!(
                msg.contains("cannot establish attribution"),
                "expected UncertainOutcome specifically because publication attribution cannot be established, got: {msg}"
            );
        }
        Ok(resp) => {
            panic!(
                "FALSE SUCCESS: import_with_reconciliation synthesized 201 published based on counts/cursor: {resp:?}"
            );
        }
        other => panic!(
            "expected UncertainOutcome specifically due to unverified attribution, got: {other:?}"
        ),
    }

    // Verify server actually contains snapshot H published by Feeder B, not snapshot G.
    let b = client_b
        .declare_barrier(0, "snap-cursor-competing")
        .await
        .unwrap();
    assert_eq!(b.status, 200);
    assert_eq!(client_b.get_ready().await.unwrap().status, 200);

    let q_1_2 = client_b.query_connected(1, 2).await.unwrap();
    assert_eq!(
        q_1_2.json()["connected"],
        false,
        "edge (1, 2) from uncommitted snapshot G must not exist"
    );

    let q_2_3 = client_b.query_connected(2, 3).await.unwrap();
    assert_eq!(
        q_2_3.json()["connected"],
        true,
        "edge (2, 3) from published snapshot H must exist"
    );
}
