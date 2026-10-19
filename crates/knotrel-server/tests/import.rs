//! Deterministic integration tests for Feature 0.2.2: Bounded Atomic HTTP Graph Import.
//!
//! These tests verify admission before body consumption, fresh server identity,
//! exact ownership fencing, contract-compliant replacement preconditions,
//! ceiling-at-one concurrency, deterministic lifecycle shutdown, and error rollback.

#![cfg(feature = "test-support")]

use knotrel_core::EngineConfig;
use knotrel_server::{
    Lifecycle, ServerConfig, ServerMode, ShutdownOutcome, router_with_lifecycle, serve,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

const STEP: Duration = Duration::from_secs(5);

fn config_source_managed() -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged)
        .with_shutdown_timeout(Duration::from_secs(5))
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
        TcpStream::connect(self.address).await.unwrap()
    }

    async fn instance_id(&self) -> String {
        let mut stream = self.connect().await;
        let reply = get(&mut stream, "/v1/info").await;
        assert_eq!(reply.status, 200);
        reply.json()["instance_id"]
            .as_str()
            .expect("instance_id in /v1/info")
            .to_string()
    }

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    async fn wait_for_jobs(&self, target: usize) {
        timeout(STEP, async {
            while self.lifecycle.outstanding_jobs() != target {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for outstanding jobs");
    }

    async fn finished(self) -> ShutdownOutcome {
        timeout(STEP, self.task).await.unwrap().unwrap()
    }
}

#[allow(dead_code)]
struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| {
            panic!(
                "expected JSON body, got status {} with body: {}",
                self.status, self.body
            )
        })
    }
}

async fn read_reply(stream: &mut TcpStream) -> Option<Reply> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = timeout(STEP, stream.read(&mut chunk))
            .await
            .unwrap()
            .unwrap_or(0);
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8(buffer[..head_end].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = timeout(STEP, stream.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(read, 0, "connection closed inside a response body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = String::from_utf8(buffer[head_end..head_end + length].to_vec()).unwrap();
    Some(Reply {
        status,
        headers,
        body,
    })
}

async fn get(stream: &mut TcpStream, path: &str) -> Reply {
    let request = format!("GET {path} HTTP/1.1\r\nhost: knotrel\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    read_reply(stream).await.expect("a response")
}

async fn raw_post(
    stream: &mut TcpStream,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    raw_post_bytes(stream, path, headers, body.as_bytes()).await
}

async fn raw_post_bytes(
    stream: &mut TcpStream,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut header_str = String::new();
    for (k, v) in headers {
        header_str.push_str(&format!("{k}: {v}\r\n"));
    }
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: knotrel\r\ncontent-length: {}\r\n{header_str}\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    read_reply(stream).await.expect("a response")
}

async fn post(stream: &mut TcpStream, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut all_headers = vec![("content-type", "application/json")];
    all_headers.extend_from_slice(headers);
    raw_post(stream, path, &all_headers, body).await
}

async fn register_session(running: &Running, session_id: &str, epoch: u64, lease_ms: u64) -> Reply {
    let instance_id = running.instance_id().await;
    let mut stream = running.connect().await;
    let body = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"{session_id}","feeder_epoch":{epoch},"lease_duration_ms":{lease_ms}}}"#
    );
    post(
        &mut stream,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &instance_id)],
        &body,
    )
    .await
}

// ---------------------------------------------------------------------------
// 1. Admission before body consumption & Concurrency ceiling at one
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_1_admission_before_body_consumption_and_ceiling_at_one() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream1 = running.connect().await;

    let import_body1 = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let request1 = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body1}",
        import_body1.len()
    );
    stream1.write_all(request1.as_bytes()).await.unwrap();

    pause.wait_started().await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Competing client connects with a huge Content-Length and sends ONLY the headers.
    // It does NOT send any body bytes at all.
    let mut stream2 = running.connect().await;
    let headers_only = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: 1000000\r\n\r\n"
    );
    stream2.write_all(headers_only.as_bytes()).await.unwrap();

    // Competing client receives immediate 503 import_in_progress without sending body bytes!
    let reply2 = read_reply(&mut stream2).await.expect("immediate response");
    assert_eq!(reply2.status, 503);
    assert_eq!(reply2.json()["code"], "import_in_progress");

    // Release Client 1 worker
    pause.release();
    running.wait_for_jobs(0).await;

    // After Client 1 finishes, Client 3 can now acquire permit and succeed
    let mut stream3 = running.connect().await;
    let import_body3 = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-11","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["10","20"],"edges":[{{"source":"10","target":"20"}}]}}"#
    );
    let reply3 = post(
        &mut stream3,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.pos-10\""),
        ],
        &import_body3,
    )
    .await;
    assert_eq!(reply3.status, 201);
    assert_eq!(reply3.json()["generation"], 2);
}

// ---------------------------------------------------------------------------
// 2. Fresh server identity and isolation across restarts and cloned configs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_2_fresh_server_identity_and_isolation() {
    let config = config_source_managed();
    let running1 = Running::start(config.clone()).await;
    let running2 = Running::start(config.clone()).await;

    let id1 = running1.instance_id().await;
    let id2 = running2.instance_id().await;
    assert_ne!(id1, id2, "cloned config must produce distinct instance IDs");

    // Restart server 1 (create running3 from same config)
    let running3 = Running::start(config).await;
    let id3 = running3.instance_id().await;
    assert_ne!(
        id1, id3,
        "restarted server must produce a fresh instance ID"
    );

    // Sending stale instance ID returns 409 stale_instance_id
    let mut stream = running3.connect().await;
    let body = format!(
        r#"{{"instance_id":"{id1}","feeder_session_id":"feeder-1","feeder_epoch":1,"lease_duration_ms":10000}}"#
    );
    let stale_reply = post(
        &mut stream,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &id1)],
        &body,
    )
    .await;
    assert_eq!(stale_reply.status, 409);
    assert_eq!(stale_reply.json()["code"], "stale_instance_id");

    // Missing header returns 400 invalid_instance_id
    let mut stream_missing = running3.connect().await;
    let missing_reply = post(&mut stream_missing, "/v1/feeder/session", &[], &body).await;
    assert_eq!(missing_reply.status, 400);
    assert_eq!(missing_reply.json()["code"], "invalid_instance_id");

    // Mismatched body instance_id returns 409 stale_instance_id
    let mut stream_body_mismatch = running3.connect().await;
    let mismatch_reply = post(
        &mut stream_body_mismatch,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &id3)],
        &body,
    )
    .await;
    assert_eq!(mismatch_reply.status, 409);
    assert_eq!(mismatch_reply.json()["code"], "stale_instance_id");
}

// ---------------------------------------------------------------------------
// 3. Exact ownership fencing in both directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_3_exact_ownership_fencing_both_directions() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;

    // Register session at epoch 2
    assert_eq!(
        register_session(&running, "feeder-active", 2, 10000)
            .await
            .status,
        200
    );

    // Heartbeat tests:
    // Stale lower epoch (1) -> 409 fenced_feeder_session
    let mut s1 = running.connect().await;
    let hb_low = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"feeder-active","feeder_epoch":1}}"#
    );
    let r1 = post(
        &mut s1,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_low,
    )
    .await;
    assert_eq!(r1.status, 409);
    assert_eq!(r1.json()["code"], "fenced_feeder_session");

    // Unregistered higher epoch (3) -> 409 fenced_feeder_session
    let mut s2 = running.connect().await;
    let hb_high = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"feeder-active","feeder_epoch":3}}"#
    );
    let r2 = post(
        &mut s2,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_high,
    )
    .await;
    assert_eq!(r2.status, 409);
    assert_eq!(r2.json()["code"], "fenced_feeder_session");

    // Different session ID -> 409 fenced_feeder_session
    let mut s3 = running.connect().await;
    let hb_wrong_session = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"feeder-other","feeder_epoch":2}}"#
    );
    let r3 = post(
        &mut s3,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_wrong_session,
    )
    .await;
    assert_eq!(r3.status, 409);
    assert_eq!(r3.json()["code"], "fenced_feeder_session");

    // Matching session & epoch -> 200 OK
    let mut s4 = running.connect().await;
    let hb_ok = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"feeder-active","feeder_epoch":2}}"#
    );
    let r4 = post(
        &mut s4,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_ok,
    )
    .await;
    assert_eq!(r4.status, 200);

    // Import tests:
    // Stale lower epoch (1) -> 409 fenced_feeder_session
    let mut s5 = running.connect().await;
    let import_low = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"feeder-active","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#
    );
    let r5 = post(
        &mut s5,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &import_low,
    )
    .await;
    assert_eq!(r5.status, 409);
    assert_eq!(r5.json()["code"], "fenced_feeder_session");

    // Unregistered higher epoch (3) -> 409 fenced_feeder_session
    let mut s6 = running.connect().await;
    let import_high = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"feeder-active","feeder_epoch":3,"nodes":["1"],"edges":[]}}"#
    );
    let r6 = post(
        &mut s6,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &import_high,
    )
    .await;
    assert_eq!(r6.status, 409);
    assert_eq!(r6.json()["code"], "fenced_feeder_session");

    // Matching epoch (2) -> 201 Created
    let mut s7 = running.connect().await;
    let import_ok = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"feeder-active","feeder_epoch":2,"nodes":["1"],"edges":[]}}"#
    );
    let r7 = post(
        &mut s7,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &import_ok,
    )
    .await;
    assert_eq!(r7.status, 201);
}

// ---------------------------------------------------------------------------
// 4. Contract-compliant replacement preconditions and percent encoding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_4_contract_compliant_replacement_preconditions_and_percent_encoding() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Initial load at generation 1 with complex cursor containing slashes, %, and dots
    let complex_pos = "pos/with/slashes%and.dots";
    let mut s1 = running.connect().await;
    let initial_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"{complex_pos}","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let r1 = post(
        &mut s1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &initial_body,
    )
    .await;
    assert_eq!(r1.status, 201);

    // Verify stats returns raw position and generation 1
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["source_position"], complex_pos);
    assert_eq!(stats.json()["generation"], 1);

    // Calling create_only on generation > 0 returns 409 graph_already_initialized
    let mut s2 = running.connect().await;
    let r2 = post(
        &mut s2,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &initial_body,
    )
    .await;
    assert_eq!(r2.status, 409);
    assert_eq!(r2.json()["code"], "graph_already_initialized");

    // Direct mutation in source-managed mode returns 409 direct_mutation_disabled
    let mut s_mut = running.connect().await;
    let r_mut = post(
        &mut s_mut,
        "/v1/operations",
        &[],
        r#"{"op":"add_node","node":"99"}"#,
    )
    .await;
    assert_eq!(r_mut.status, 409);
    assert_eq!(r_mut.json()["code"], "direct_mutation_disabled");

    // Replacement with malformed ETag (not 3 dot parts) returns 412 precondition_failed
    let mut s3 = running.connect().await;
    let replace_body = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-next","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#
    );
    let r3 = post(
        &mut s3,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"malformed-etag\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(r3.status, 412);
    assert_eq!(r3.json()["code"], "precondition_failed");

    // Replacement with genuine generation mismatch returns 412 precondition_failed
    let mut s4 = running.connect().await;
    let r4 = post(
        &mut s4,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"99.0.pos%2Fwith%2Fslashes%25and%2Edots\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(r4.status, 412);
    assert_eq!(r4.json()["code"], "precondition_failed");

    // Replacement with genuine position mismatch returns 412 precondition_failed
    let mut s5 = running.connect().await;
    let r5 = post(
        &mut s5,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.wrong%2Dpos\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(r5.status, 412);
    assert_eq!(r5.json()["code"], "precondition_failed");

    // Replacement with properly percent-encoded ETag matches and succeeds!
    // pos/with/slashes%and.dots -> pos%2Fwith%2Fslashes%25and%2Edots
    let mut s6 = running.connect().await;
    let r6 = post(
        &mut s6,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.pos%2Fwith%2Fslashes%25and%2Edots\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(r6.status, 201);
    assert_eq!(r6.json()["generation"], 2);
}

// ---------------------------------------------------------------------------
// 5. Deterministic shutdown with zero HTTP connections and active import
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_5_shutdown_with_no_http_connections_deterministic_drain() {
    let mut running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let (drain_tx, mut drain_rx) = mpsc::channel(1);
    let drain_tx = Arc::new(std::sync::Mutex::new(Some(drain_tx)));

    running
        .lifecycle
        .on_drain_loop(move |draining, tasks_empty, active_jobs| {
            if draining
                && tasks_empty
                && active_jobs == 1
                && let Ok(mut lock) = drain_tx.lock()
                && let Some(sender) = lock.take()
            {
                let _ = sender.try_send(());
            }
        });

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
    );
    let request = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body}",
        import_body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Client drops stream; all HTTP connection sockets are closed
    drop(stream);

    // Trigger graceful shutdown
    running.shutdown();

    // Deterministically wait for serve() to enter Draining phase with tasks empty and 1 job active
    timeout(STEP, drain_rx.recv())
        .await
        .expect("timed out waiting for drain loop hook")
        .expect("drain hook channel closed");

    // Verify serve() is still running because active_jobs == 1
    assert!(
        !running.task.is_finished(),
        "serve must wait for outstanding import worker"
    );

    // Release worker
    pause.release();

    // serve() completes cleanly with Drained
    let lifecycle = running.lifecycle.clone();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    assert_eq!(lifecycle.outstanding_jobs(), 0);
}

// ---------------------------------------------------------------------------
// 6. Off-lock candidate construction and read isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_6_off_lock_candidate_construction_and_read_isolation() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Establish Generation 1
    let mut stream1 = running.connect().await;
    let initial_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let r1 = post(
        &mut stream1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &initial_body,
    )
    .await;
    assert_eq!(r1.status, 201);

    // Start replacement import and pause worker off-lock during candidate build
    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream_replace = running.connect().await;

    let replace_body = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-20","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#
    );
    let request = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\nif-match: \"1.0.pos-10\"\r\ncontent-length: {}\r\n\r\n{replace_body}",
        replace_body.len()
    );
    stream_replace.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;

    // While candidate construction is paused off-lock, concurrent queries must complete against old graph!
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["generation"], 1);
    assert_eq!(stats.json()["node_count"], 2);

    let mut s_query = running.connect().await;
    let q_res = post(
        &mut s_query,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(q_res.status, 200);
    assert_eq!(q_res.json()["connected"], true);

    // Concurrent heartbeat must also succeed
    let mut s_hb = running.connect().await;
    let hb_body = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"feeder-1","feeder_epoch":1}}"#
    );
    let hb_res = post(
        &mut s_hb,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_body,
    )
    .await;
    assert_eq!(hb_res.status, 200);

    // Now release worker and observe generation 2 publication
    pause.release();
    let replace_reply = read_reply(&mut stream_replace).await.unwrap();
    assert_eq!(replace_reply.status, 201);
    assert_eq!(replace_reply.json()["generation"], 2);
}

// ---------------------------------------------------------------------------
// 7. Worker panic, cancellation, and rollback isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_7_caller_cancellation_after_blocking_worker_start() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let request = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body}",
        import_body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Client abruptly cancels request by dropping stream
    drop(stream);
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Release worker: worker finishes build, acquires lock, publishes candidate
    pause.release();

    // Await worker completion
    running.wait_for_jobs(0).await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 0);

    // Verify publication took effect in memory
    let mut stats_stream = running.connect().await;
    let stats = get(&mut stats_stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    let json = stats.json();
    assert_eq!(json["generation"], 1);
    assert_eq!(json["node_count"], 2);
    assert_eq!(json["edge_count"], 1);
}

#[tokio::test]
async fn test_8_worker_panic_after_caller_cancellation() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
    );
    let request = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body}",
        import_body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    drop(stream);

    // Instruct worker to panic
    pause.panic_on_release();

    running.wait_for_jobs(0).await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 0);

    // Check that service is marked unavailable
    let mut s1 = running.connect().await;
    let ready = get(&mut s1, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "unavailable");

    let mut s2 = running.connect().await;
    let stats = get(&mut s2, "/v1/stats").await;
    assert_eq!(stats.status, 503);
    assert_eq!(stats.json()["code"], "unavailable");

    // Liveness probe (/health) remains 200
    let mut s3 = running.connect().await;
    let health = get(&mut s3, "/health").await;
    assert_eq!(health.status, 200);
}

#[tokio::test]
async fn test_9_session_takeover_and_lease_expiry_before_publication() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;

    // Part A: Session Takeover
    assert_eq!(
        register_session(&running, "feeder-A", 1, 10000)
            .await
            .status,
        200
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream_a = running.connect().await;

    let import_body_a = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-A","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
    );
    let request_a = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body_a}",
        import_body_a.len()
    );
    stream_a.write_all(request_a.as_bytes()).await.unwrap();

    pause.wait_started().await;

    // Feeder B preempts Feeder A with higher epoch
    let takeover_res = register_session(&running, "feeder-B", 2, 1000).await;
    assert_eq!(takeover_res.status, 200);

    // Release Feeder A worker
    pause.release();

    let reply_a = read_reply(&mut stream_a).await.unwrap();
    assert_eq!(reply_a.status, 409);
    assert_eq!(reply_a.json()["code"], "fenced_feeder_session");

    let mut s = running.connect().await;
    let stats = get(&mut s, "/v1/stats").await;
    assert_eq!(stats.json()["generation"], 0); // Candidate was discarded

    // Part B: Lease Expiry
    // Feeder B has a 1000ms lease. We start import, pause, wait past lease duration, and release.
    let mut pause_b = running.lifecycle.pause_next_worker();
    let mut stream_b = running.connect().await;

    let import_body_b = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-B","feeder_epoch":2,"nodes":["1","2"],"edges":[]}}"#
    );
    let request_b = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body_b}",
        import_body_b.len()
    );
    stream_b.write_all(request_b.as_bytes()).await.unwrap();

    pause_b.wait_started().await;

    // Wait past the 1000ms lease duration without heartbeats
    tokio::time::sleep(Duration::from_millis(1100)).await;

    pause_b.release();

    let reply_b = read_reply(&mut stream_b).await.unwrap();
    assert_eq!(reply_b.status, 409);
    assert_eq!(reply_b.json()["code"], "lease_expired");

    let mut s2 = running.connect().await;
    let stats2 = get(&mut s2, "/v1/stats").await;
    assert_eq!(stats2.json()["generation"], 0); // Candidate discarded
}

#[tokio::test]
async fn test_10_deadline_expiry_with_outstanding_work() {
    let config = config_source_managed()
        .with_shutdown_timeout(Duration::from_millis(150))
        .unwrap();
    let mut running = Running::start(config).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
    );
    let request = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{import_body}",
        import_body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    drop(stream);

    // Initiate shutdown
    running.shutdown();

    // Do NOT release pause; wait for deadline to expire in serve()
    let outcome = running.finished().await;
    assert!(
        matches!(outcome, ShutdownOutcome::DeadlineExceeded { abandoned } if abandoned >= 1),
        "expected DeadlineExceeded with at least 1 abandoned job, got: {outcome:?}"
    );

    // Clean up worker thread
    pause.release();
}

#[tokio::test]
async fn test_11_failed_publication_preserving_old_graph_and_metadata() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Initial successful load at generation 1
    let mut stream = running.connect().await;
    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let reply = post(
        &mut stream,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &import_body,
    )
    .await;
    assert_eq!(reply.status, 201);
    assert_eq!(reply.json()["generation"], 1);

    // Failure: Replacement containing a self-loop (domain validation error)
    let mut s_bad1 = running.connect().await;
    let body_self_loop = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-11","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1"],"edges":[{{"source":"1","target":"1"}}]}}"#
    );
    let reply_bad1 = post(
        &mut s_bad1,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.pos-10\""),
        ],
        &body_self_loop,
    )
    .await;
    assert_eq!(reply_bad1.status, 422);
    assert_eq!(reply_bad1.json()["code"], "self_loop");

    // Final verification: baseline graph G_0 and metadata are completely untouched
    let mut s_final = running.connect().await;
    let stats_final = get(&mut s_final, "/v1/stats").await;
    let json_final = stats_final.json();
    assert_eq!(json_final["generation"], 1);
    assert_eq!(json_final["node_count"], 2);
    assert_eq!(json_final["edge_count"], 1);
    assert_eq!(json_final["source_position"], "pos-10");
    assert_eq!(json_final["state_version"], "0");

    // Connected query on 1-2 verifies topology is preserved
    let mut query_stream = running.connect().await;
    let query_reply = post(
        &mut query_stream,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(query_reply.status, 200);
    assert_eq!(query_reply.json()["connected"], true);
}

// ---------------------------------------------------------------------------
// 8. Graph Identity Validation (graph_id == "default")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_12_graph_identity_validation_and_isolation() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Initial valid import with graph_id "default" establishes baseline graph
    let mut s0 = running.connect().await;
    let initial_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let r0 = post(
        &mut s0,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &initial_body,
    )
    .await;
    assert_eq!(r0.status, 201);
    assert_eq!(r0.json()["graph_id"], "default");

    // Rejected non-default graph_ids
    for invalid_graph_id in ["custom", "graph_1", "default_other", "tenant-42", ""] {
        let mut s = running.connect().await;
        let replace_body = format!(
            r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"{invalid_graph_id}","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-11","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#
        );
        let reply = post(
            &mut s,
            "/v1/feeder/import",
            &[
                ("x-knotrel-instance-id", &instance_id),
                ("if-match", "\"1.0.pos-10\""),
            ],
            &replace_body,
        )
        .await;
        assert_eq!(reply.status, 400, "rejected graph_id: {invalid_graph_id}");
        assert_eq!(reply.json()["code"], "invalid_graph_id");
        // Verify server never echoes the arbitrary graph_id as a successful publication
        assert_ne!(reply.json()["result"], "published");
    }

    // Verify baseline graph G_0 topology and metadata are completely untouched
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    let json = stats.json();
    assert_eq!(json["generation"], 1);
    assert_eq!(json["node_count"], 2);
    assert_eq!(json["edge_count"], 1);
    assert_eq!(json["source_position"], "pos-10");
    assert_eq!(json["state_version"], "0");

    let mut s_q = running.connect().await;
    let q = post(
        &mut s_q,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(q.status, 200);
    assert_eq!(q.json()["connected"], true);
}

// ---------------------------------------------------------------------------
// 9. HTTP Content-Type Validation and No Publication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_13_content_type_validation_and_no_publication() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    let import_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#
    );

    // 1. Missing Content-Type header -> 415 Unsupported Media Type
    let mut s1 = running.connect().await;
    let r1 = raw_post(
        &mut s1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &import_body,
    )
    .await;
    assert_eq!(r1.status, 415);
    assert_eq!(r1.json()["code"], "invalid_request");
    assert_eq!(
        r1.json()["message"],
        "Expected request with `Content-Type: application/json`"
    );

    // 2. Unsupported Content-Type: text/plain -> 415 Unsupported Media Type
    let mut s2 = running.connect().await;
    let r2 = raw_post(
        &mut s2,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("content-type", "text/plain"),
        ],
        &import_body,
    )
    .await;
    assert_eq!(r2.status, 415);
    assert_eq!(r2.json()["code"], "invalid_request");

    // 3. Unsupported Content-Type: application/xml -> 415 Unsupported Media Type
    let mut s3 = running.connect().await;
    let r3 = raw_post(
        &mut s3,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("content-type", "application/xml"),
        ],
        &import_body,
    )
    .await;
    assert_eq!(r3.status, 415);

    // 4. Verify no publication occurred
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["generation"], 0);

    // 5. Valid Content-Type succeeds
    let mut s4 = running.connect().await;
    let r4 = raw_post(
        &mut s4,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("content-type", "application/json; charset=utf-8"),
        ],
        &import_body,
    )
    .await;
    assert_eq!(r4.status, 201);
    assert_eq!(r4.json()["generation"], 1);
}

// ---------------------------------------------------------------------------
// 10. Strict Replacement Precondition Negative Cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_14_strict_replacement_preconditions_negative_cases() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Initial import establishes generation 1 with position "pos-10"
    let mut s0 = running.connect().await;
    let initial_body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
    );
    let r0 = post(
        &mut s0,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &instance_id)],
        &initial_body,
    )
    .await;
    assert_eq!(r0.status, 201);

    let replace_body = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-20","feeder_session_id":"feeder-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#
    );

    // Negative precondition cases that permissive parsers would wrongly accept
    let invalid_etags = [
        ("unquoted", "1.0.pos-10"),
        ("missing_leading_quote", "1.0.pos-10\""),
        ("missing_trailing_quote", "\"1.0.pos-10"),
        ("repeated_quotes", "\"\"1.0.pos-10\"\""),
        ("weak_tag", "W/\"1.0.pos-10\""),
        ("lowercase_weak_tag", "w/\"1.0.pos-10\""),
        ("wildcard", "*"),
        ("quoted_wildcard", "\"*\""),
        ("list_form", "\"1.0.pos-10\", \"2.0.pos-10\""),
        ("empty_quotes", "\"\""),
        ("single_quote", "\""),
        ("missing_dots", "\"1_0_pos-10\""),
        ("two_parts_only", "\"1.0\""),
        ("four_parts", "\"1.0.pos.extra\""),
        ("non_numeric_gen", "\"gen.0.pos-10\""),
        ("non_decimal_ver", "\"1.ver.pos-10\""),
        ("numeric_mismatch", "\"2.0.pos-10\""),
        ("position_mismatch", "\"1.0.other-pos\""),
    ];

    for (label, etag) in invalid_etags {
        let mut s = running.connect().await;
        let reply = post(
            &mut s,
            "/v1/feeder/import",
            &[("x-knotrel-instance-id", &instance_id), ("if-match", etag)],
            &replace_body,
        )
        .await;
        assert_eq!(
            reply.status, 412,
            "case '{label}' with etag '{etag}' must return 412"
        );
        assert_eq!(reply.json()["code"], "precondition_failed");
    }

    // Verify baseline graph is untouched after all failed preconditions
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["generation"], 1);
    assert_eq!(stats.json()["node_count"], 2);
    assert_eq!(stats.json()["edge_count"], 1);

    // Valid properly quoted If-Match succeeds!
    let mut s_ok = running.connect().await;
    let ok_reply = post(
        &mut s_ok,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.pos-10\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(ok_reply.status, 201);
    assert_eq!(ok_reply.json()["generation"], 2);
}

// ---------------------------------------------------------------------------
// 11. Error Classification: Malformed Syntax vs Schema/Data Errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_15_error_classification_malformed_syntax_vs_schema_data() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-1", 1, 10000)
            .await
            .status,
        200
    );

    // Group A: Malformed JSON syntax -> 400 Bad Request, message "request body is not valid JSON"
    let syntax_errors: [&[u8]; 4] = [
        b"{",
        b"{\"mode\":",
        b"{\"mode\": \"create_only\" trailing",
        b"\xff\xfe",
    ];

    for body in syntax_errors {
        let mut s = running.connect().await;
        let reply = raw_post_bytes(
            &mut s,
            "/v1/feeder/import",
            &[
                ("x-knotrel-instance-id", &instance_id),
                ("content-type", "application/json"),
            ],
            body,
        )
        .await;
        assert_eq!(reply.status, 400, "body {:?} must be 400 Bad Request", body);
        assert_eq!(reply.json()["code"], "invalid_request");
        assert_eq!(reply.json()["message"], "request body is not valid JSON");
    }

    // Group B: Schema / Data errors -> 422 Unprocessable Entity
    let data_errors: [(&str, &str); 5] = [
        // Missing required field
        ("{}", "missing field `mode`"),
        // Non-numeric node ID string
        (
            r#"{"mode":"create_only","instance_id":"INST","graph_id":"default","source_id":"s","source_epoch":"e","source_position":"p","feeder_session_id":"f","feeder_epoch":1,"nodes":["abc"],"edges":[]}"#,
            "node IDs must be nonempty strings of ASCII decimal digits",
        ),
        // Empty string node ID
        (
            r#"{"mode":"create_only","instance_id":"INST","graph_id":"default","source_id":"s","source_epoch":"e","source_position":"p","feeder_session_id":"f","feeder_epoch":1,"nodes":[""],"edges":[]}"#,
            "node IDs must be nonempty strings of ASCII decimal digits",
        ),
        // Invalid edge node ID
        (
            r#"{"mode":"create_only","instance_id":"INST","graph_id":"default","source_id":"s","source_epoch":"e","source_position":"p","feeder_session_id":"f","feeder_epoch":1,"nodes":["1"],"edges":[{"source":"bad","target":"1"}]}"#,
            "node IDs must be nonempty strings of ASCII decimal digits",
        ),
        // Wrong type for nodes array
        (
            r#"{"mode":"create_only","instance_id":"INST","graph_id":"default","source_id":"s","source_epoch":"e","source_position":"p","feeder_session_id":"f","feeder_epoch":1,"nodes":123,"edges":[]}"#,
            "request body does not match the expected shape",
        ),
    ];

    for (body_template, expected_msg) in data_errors {
        let body = body_template.replace("INST", &instance_id);
        let mut s = running.connect().await;
        let reply = post(
            &mut s,
            "/v1/feeder/import",
            &[("x-knotrel-instance-id", &instance_id)],
            &body,
        )
        .await;
        assert_eq!(
            reply.status, 422,
            "data error body {:?} must be 422 Unprocessable Entity",
            body
        );
        assert_eq!(reply.json()["code"], "invalid_request");
        assert_eq!(reply.json()["message"], expected_msg);
    }

    // Verify no publication occurred
    let mut s_stats = running.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["generation"], 0);
}
