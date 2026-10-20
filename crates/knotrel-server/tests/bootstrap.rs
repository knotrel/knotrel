//! Deterministic integration tests for Feature 0.2.5: Synchronization-Aware Bootstrap and Readiness.
//!
//! Validates:
//! - Complete bootstrap flow: initialization -> atomic baseline import -> ordered catch-up -> verified barrier -> Ready
//! - Decision table B1 through B9 under the graph serialization boundary
//! - Lock-free lease expiry detection in /ready without waiting behind graph mutex
//! - Heartbeat after expiry renewal extends deadline but maintains Stale status
//! - Session retries and takeovers cannot bypass synchronization recovery
//! - Delayed barrier handling across disconnections and takeovers
//! - Replacement construction read isolation: candidate off-lock, published graph remains active
//! - Precedence: Permanent Worker Failure > Draining > Starting > Uninitialized/Loading > Desynchronized > Expired Lease/Stale > CatchingUp > Ready
//! - Clean preservation of Standalone mode and Features 0.2.2-0.2.4

#![cfg(feature = "test-support")]

use knotrel_core::EngineConfig;
use knotrel_server::{
    Lifecycle, Phase, ServerConfig, ServerMode, ShutdownOutcome, advance_clock, reset_clock,
    router_with_lifecycle, serve,
};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
    task::JoinHandle,
    time::timeout,
};

static TEST_MUTEX: Mutex<()> = Mutex::const_new(());
const STEP: Duration = Duration::from_secs(5);

fn config_source_managed() -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged)
        .with_shutdown_timeout(Duration::from_secs(5))
        .unwrap()
}

fn config_standalone() -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::Standalone)
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
        reset_clock();
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

    async fn finished(self) -> ShutdownOutcome {
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
                "expected JSON body, got status {} with body: {}",
                self.status, self.body
            )
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim())
            } else {
                None
            }
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
    let mut header_str = String::new();
    for (k, v) in headers {
        header_str.push_str(&format!("{k}: {v}\r\n"));
    }
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: knotrel\r\ncontent-length: {}\r\n{header_str}\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
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
        &[("X-Knotrel-Instance-Id", &instance_id)],
        &body,
    )
    .await
}

async fn heartbeat(running: &Running, session_id: &str, epoch: u64) -> Reply {
    let instance_id = running.instance_id().await;
    let mut stream = running.connect().await;
    let body = format!(
        r#"{{"instance_id":"{instance_id}","feeder_session_id":"{session_id}","feeder_epoch":{epoch}}}"#
    );
    post(
        &mut stream,
        "/v1/feeder/heartbeat",
        &[("X-Knotrel-Instance-Id", &instance_id)],
        &body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn import_graph(
    running: &Running,
    session_id: &str,
    epoch: u64,
    mode: &str,
    source_position: &str,
    nodes: &[u64],
    edges: &[(u64, u64)],
    if_match: Option<&str>,
) -> Reply {
    let instance_id = running.instance_id().await;
    let nodes_json = nodes
        .iter()
        .map(|n| format!(r#""{n}""#))
        .collect::<Vec<_>>()
        .join(",");
    let edges_json = edges
        .iter()
        .map(|(s, t)| format!(r#"{{"source":"{s}","target":"{t}"}}"#))
        .collect::<Vec<_>>()
        .join(",");

    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","mode":"{mode}","feeder_session_id":"{session_id}","feeder_epoch":{epoch},"source_id":"source-1","source_epoch":"epoch-1","source_position":"{source_position}","nodes":[{nodes_json}],"edges":[{edges_json}]}}"#
    );

    let mut headers = vec![
        ("X-Knotrel-Instance-Id", instance_id.as_str()),
        ("content-type", "application/json"),
    ];
    if let Some(etag) = if_match {
        headers.push(("If-Match", etag));
    }

    let mut stream = running.connect().await;
    raw_post(&mut stream, "/v1/feeder/import", &headers, &body).await
}

async fn declare_barrier(
    running: &Running,
    session_id: &str,
    epoch: u64,
    generation: u64,
    sequence: u64,
    source_position: Option<&str>,
) -> Reply {
    let instance_id = running.instance_id().await;
    let pos_json = match source_position {
        Some(pos) => format!(r#""{pos}""#),
        None => "null".to_string(),
    };
    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":{generation},"feeder_session_id":"{session_id}","feeder_epoch":{epoch},"barrier_delivery_sequence":{sequence},"barrier_source_position":{pos_json}}}"#
    );
    let mut stream = running.connect().await;
    post(
        &mut stream,
        "/v1/feeder/barrier",
        &[("X-Knotrel-Instance-Id", &instance_id)],
        &body,
    )
    .await
}

async fn apply_events(
    running: &Running,
    session_id: &str,
    epoch: u64,
    generation: u64,
    events_json: &str,
) -> Reply {
    let instance_id = running.instance_id().await;
    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":{generation},"feeder_session_id":"{session_id}","feeder_epoch":{epoch},"source_id":"source-1","source_epoch":"epoch-1","events":{events_json}}}"#
    );
    let mut stream = running.connect().await;
    post(
        &mut stream,
        "/v1/feeder/events",
        &[("X-Knotrel-Instance-Id", &instance_id)],
        &body,
    )
    .await
}

async fn declare_gap(
    running: &Running,
    session_id: &str,
    epoch: u64,
    generation: u64,
    failed_seq: u64,
) -> Reply {
    let instance_id = running.instance_id().await;
    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":{generation},"feeder_session_id":"{session_id}","feeder_epoch":{epoch},"failed_delivery_sequence":{failed_seq},"reason":"unrecoverable gap"}}"#
    );
    let mut stream = running.connect().await;
    post(
        &mut stream,
        "/v1/feeder/gap",
        &[("X-Knotrel-Instance-Id", &instance_id)],
        &body,
    )
    .await
}

async fn start_unfinished_request(running: &Running) -> TcpStream {
    let mut stream = running.connect().await;
    let body = r#"{"op":"connected","source":"1","target":"2"}"#;
    let head = format!(
        "POST /v1/operations HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream
        .write_all(&body.as_bytes()[..body.len() - 1])
        .await
        .unwrap();
    stream.flush().await.unwrap();
    stream
}

async fn wait_for_phase(lifecycle: &Lifecycle, target: Phase) {
    timeout(STEP, async {
        while lifecycle.phase() != target {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for lifecycle phase");
}

#[tokio::test]
async fn test_1_source_managed_startup_is_unready() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;

    // 1. Health probe is 200 ok
    let mut stream = running.connect().await;
    let health = get(&mut stream, "/health").await;
    assert_eq!(health.status, 200);
    assert_eq!(health.json()["status"], "ok");

    // 2. Readiness probe is 503 uninitialized
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "uninitialized");

    // 3. Info reports mode: source_managed and valid instance_id
    let info = get(&mut stream, "/v1/info").await;
    assert_eq!(info.status, 200);
    assert_eq!(info.json()["mode"], "source_managed");
    assert!(info.json()["instance_id"].as_str().is_some());

    // 4. Stats reports generation 0 and sync_status: uninitialized
    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["generation"], 0);
    assert_eq!(stats.json()["sync_status"], "uninitialized");
    assert_eq!(stats.header("X-Knotrel-Sync-Status"), Some("uninitialized"));

    // 5. Query execution rejected with 503 uninitialized
    let q_body = r#"{"op":"connected","source":"1","target":"2"}"#;
    let q_res = post(&mut stream, "/v1/operations", &[], q_body).await;
    assert_eq!(q_res.status, 503);
    assert_eq!(q_res.json()["code"], "uninitialized");

    // 6. Direct mutation rejected with 409 direct_mutation_disabled
    let mut_body = r#"{"op":"link","source":"1","target":"2"}"#;
    let mut_res = post(&mut stream, "/v1/operations", &[], mut_body).await;
    assert_eq!(mut_res.status, 409);
    assert_eq!(mut_res.json()["code"], "direct_mutation_disabled");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_2_initial_loading_cannot_be_promoted_by_a_barrier() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;

    // Case B3: Barrier while Uninitialized is rejected with 409 graph_uninitialized
    let b3 = declare_barrier(&running, "feeder-1", 1, 0, 0, None).await;
    assert_eq!(b3.status, 409);
    assert_eq!(b3.json()["code"], "graph_uninitialized");

    // Pause worker during initial baseline import
    let mut pause = running.lifecycle.pause_next_worker();
    let import_task = {
        let inst_id = running.instance_id().await;
        let address = running.address;
        tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            let body = format!(
                r#"{{"instance_id":"{inst_id}","graph_id":"default","mode":"create_only","feeder_session_id":"feeder-1","feeder_epoch":1,"source_id":"src-1","source_epoch":"ep-1","source_position":"pos-1","nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#
            );
            post(
                &mut stream,
                "/v1/feeder/import",
                &[("X-Knotrel-Instance-Id", &inst_id)],
                &body,
            )
            .await
        })
    };

    pause.wait_started().await;

    // While worker is building candidate off-lock:
    let mut stream = running.connect().await;
    let ready_during_loading = get(&mut stream, "/ready").await;
    assert_eq!(ready_during_loading.status, 503);
    assert_eq!(ready_during_loading.json()["status"], "loading");

    let stats_during_loading = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats_during_loading.status, 200);
    assert_eq!(stats_during_loading.json()["sync_status"], "loading");
    assert_eq!(stats_during_loading.json()["generation"], 0);

    // Case B4: Barrier while Loading is rejected with 409 graph_uninitialized
    let b4 = declare_barrier(&running, "feeder-1", 1, 0, 0, Some("pos-1")).await;
    assert_eq!(b4.status, 409);
    assert_eq!(b4.json()["code"], "graph_uninitialized");

    // Release worker and let import complete
    pause.release();
    let import_res = import_task.await.unwrap();
    assert_eq!(import_res.status, 201);
    assert_eq!(import_res.json()["result"], "published");
    assert_eq!(import_res.json()["status"], "catching_up");

    let ready_after = get(&mut stream, "/ready").await;
    assert_eq!(ready_after.status, 503);
    assert_eq!(ready_after.json()["status"], "catching_up");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_3_initial_import_failure_or_cancellation_reverts_loading_to_uninitialized() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;

    // Attempt import with self-loop edge (forbidden)
    let res = import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1],
        &[(1, 1)],
        None,
    )
    .await;
    assert_eq!(res.status, 422);
    assert_eq!(res.json()["code"], "self_loop");

    // Server must NOT remain permanently in Loading:
    let mut stream = running.connect().await;
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "uninitialized");

    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["sync_status"], "uninitialized");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_4_successful_import_remains_catching_up() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;

    let res = import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-10",
        &[1, 2, 3],
        &[(1, 2)],
        None,
    )
    .await;
    assert_eq!(res.status, 201);
    assert_eq!(res.json()["result"], "published");
    assert_eq!(res.json()["generation"], 1);
    assert_eq!(res.json()["status"], "catching_up");

    // Readiness remains 503 catching_up
    let mut stream = running.connect().await;
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "catching_up");

    // Queries are admitted and succeed
    let q_body = r#"{"op":"connected","source":"1","target":"2"}"#;
    let q_res = post(&mut stream, "/v1/operations", &[], q_body).await;
    assert_eq!(q_res.status, 200);
    assert_eq!(q_res.json()["connected"], true);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_5_exact_barrier_promotes_correct_published_generation() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;

    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-10",
        &[1, 2, 3],
        &[(1, 2)],
        None,
    )
    .await;

    // Apply incremental events: D=1, P="pos-11"
    let events = r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-11","prev_source_position":"pos-10","op":"link","source":"2","target":"3"}]"#;
    let ev_res = apply_events(&running, "feeder-1", 1, 1, events).await;
    assert_eq!(ev_res.status, 200);

    // Exact barrier Case B1: D=1, P="pos-11"
    let barrier = declare_barrier(&running, "feeder-1", 1, 1, 1, Some("pos-11")).await;
    assert_eq!(barrier.status, 200);
    assert_eq!(barrier.json()["result"], "synchronized");
    assert_eq!(barrier.json()["status"], "ready");
    assert_eq!(barrier.json()["generation"], 1);
    assert_eq!(barrier.json()["applied_source_position"], "pos-11");
    assert_eq!(barrier.header("X-Knotrel-Sync-Status"), Some("ready"));

    // Server is now Ready
    let mut stream = running.connect().await;
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 200);
    assert_eq!(ready.json()["status"], "ready");

    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["sync_status"], "ready");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_6_duplicate_exact_barrier_is_idempotent() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-10",
        &[1, 2],
        &[],
        None,
    )
    .await;

    // First barrier transitions to Ready (Case B1)
    let b1 = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-10")).await;
    assert_eq!(b1.status, 200);
    assert_eq!(b1.json()["result"], "synchronized");

    // Second barrier affirms Ready idempotently (Case B2)
    let b2 = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-10")).await;
    assert_eq!(b2.status, 200);
    assert_eq!(b2.json()["result"], "already_synchronized");
    assert_eq!(b2.json()["status"], "ready");

    let mut stream = running.connect().await;
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 200);
    assert_eq!(ready.json()["status"], "ready");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_7_equal_sequence_different_position_rejected_without_changing_progress() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "0/16B3800",
        &[1, 2],
        &[],
        None,
    )
    .await;

    // Case B7: D=0 matches applied D=0, but P="0/16B9999" != "0/16B3800"
    let b7 = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("0/16B9999")).await;
    assert_eq!(b7.status, 409);
    assert_eq!(b7.json()["code"], "barrier_position_mismatch");

    // State remains CatchingUp and cursor is untouched
    let mut stream = running.connect().await;
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "catching_up");

    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.json()["source_position"], "0/16B3800");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_8_premature_and_older_barriers_rejected_without_changing_progress() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-0",
        &[1, 2],
        &[],
        None,
    )
    .await;

    // Case B8: Premature sequence (D_B = 5 > 0)
    let b8 = declare_barrier(&running, "feeder-1", 1, 1, 5, Some("pos-5")).await;
    assert_eq!(b8.status, 409);
    assert_eq!(b8.json()["code"], "barrier_not_reached");

    // Apply events to advance sequence to 2
    let events = r#"[
        {"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"},
        {"event_id":"e-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"progress"}
    ]"#;
    let ev_res = apply_events(&running, "feeder-1", 1, 1, events).await;
    assert_eq!(ev_res.status, 200);

    // Case B9: Older sequence (D_B = 1 < 2)
    let b9 = declare_barrier(&running, "feeder-1", 1, 1, 1, Some("pos-1")).await;
    assert_eq!(b9.status, 409);
    assert_eq!(b9.json()["code"], "superseded_barrier");

    // Progress strictly preserved at seq 2, pos "pos-2"
    let mut stream = running.connect().await;
    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.json()["source_position"], "pos-2");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_9_rejected_barriers_preserve_existing_synchronization_state() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-0",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-0")).await;

    let mut stream = running.connect().await;
    // 1. In Ready: premature barrier rejected -> state remains Ready
    assert_eq!(get(&mut stream, "/ready").await.status, 200);
    let b_prem = declare_barrier(&running, "feeder-1", 1, 1, 50, Some("pos-50")).await;
    assert_eq!(b_prem.status, 409);
    assert_eq!(b_prem.json()["code"], "barrier_not_reached");
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // 2. Expire lease -> state becomes Stale
    advance_clock(Duration::from_millis(31_000));
    assert_eq!(get(&mut stream, "/ready").await.status, 503);
    assert_eq!(get(&mut stream, "/ready").await.json()["status"], "stale");

    // Renew lease via heartbeat so session is active, but state is Stale
    heartbeat(&running, "feeder-1", 1).await;
    assert_eq!(get(&mut stream, "/ready").await.json()["status"], "stale");

    // Send mismatched position barrier -> rejected, remains Stale
    let b_mismatch = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("bad-pos")).await;
    assert_eq!(b_mismatch.status, 409);
    assert_eq!(b_mismatch.json()["code"], "barrier_position_mismatch");
    assert_eq!(get(&mut stream, "/ready").await.json()["status"], "stale");

    // 3. Declare gap -> state becomes Desynchronized
    declare_gap(&running, "feeder-1", 1, 1, 1).await;
    assert_eq!(get(&mut stream, "/ready").await.status, 503);
    assert_eq!(
        get(&mut stream, "/ready").await.json()["status"],
        "desynchronized"
    );

    // In Desynchronized: exact barrier is rejected, remains Desynchronized
    let b_desync = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-0")).await;
    assert_eq!(b_desync.status, 409);
    assert_eq!(b_desync.json()["code"], "graph_desynchronized");
    assert_eq!(
        get(&mut stream, "/ready").await.json()["status"],
        "desynchronized"
    );

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}

#[tokio::test]
async fn test_10_replacement_builds_preserve_old_published_state_and_queries() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[(1, 2)],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Get current ETag for replace mode
    let stats = get(&mut stream, "/v1/stats").await;
    let etag = stats.header("ETag").unwrap().to_string();

    // Pause worker during replacement candidate build
    let mut pause = running.lifecycle.pause_next_worker();
    let replace_task = {
        let inst_id = running.instance_id().await;
        let address = running.address;
        let etag_clone = etag.clone();
        tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            let body = format!(
                r#"{{"instance_id":"{inst_id}","graph_id":"default","mode":"replace","feeder_session_id":"feeder-1","feeder_epoch":1,"source_id":"src-1","source_epoch":"ep-1","source_position":"pos-2","nodes":["3","4"],"edges":[{{"source":"3","target":"4"}}]}}"#
            );
            raw_post(
                &mut stream,
                "/v1/feeder/import",
                &[
                    ("X-Knotrel-Instance-Id", &inst_id),
                    ("content-type", "application/json"),
                    ("If-Match", &etag_clone),
                ],
                &body,
            )
            .await
        })
    };

    pause.wait_started().await;

    // While candidate is being built:
    // 1. Currently published graph generation 1 remains Ready
    assert_eq!(get(&mut stream, "/ready").await.status, 200);
    let stats_during = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats_during.json()["generation"], 1);
    assert_eq!(stats_during.json()["sync_status"], "ready");

    // 2. Queries on generation 1 continue serving
    let q_body = r#"{"op":"connected","source":"1","target":"2"}"#;
    let q_res = post(&mut stream, "/v1/operations", &[], q_body).await;
    assert_eq!(q_res.status, 200);
    assert_eq!(q_res.json()["connected"], true);

    // 3. Barrier for generation 1 is evaluated against generation 1 (affirming Ready)
    let b_gen1 = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(b_gen1.status, 200);
    assert_eq!(b_gen1.json()["result"], "already_synchronized");

    // 4. Barrier for candidate generation 2 before publication is rejected
    let b_gen2 = declare_barrier(&running, "feeder-1", 1, 2, 0, Some("pos-2")).await;
    assert_eq!(b_gen2.status, 409);
    assert_eq!(b_gen2.json()["code"], "stale_generation");

    // Release worker and let replacement complete publication
    pause.release();
    let rep_res = replace_task.await.unwrap();
    assert_eq!(rep_res.status, 201);
    assert_eq!(rep_res.json()["result"], "published");
    assert_eq!(rep_res.json()["generation"], 2);
    assert_eq!(rep_res.json()["status"], "catching_up");

    // Upon publication, server transitions to CatchingUp at generation 2
    let ready_after = get(&mut stream, "/ready").await;
    assert_eq!(ready_after.status, 503);
    assert_eq!(ready_after.json()["status"], "catching_up");

    let stats_after = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats_after.json()["generation"], 2);
    assert_eq!(stats_after.json()["sync_status"], "catching_up");

    // New graph active (nodes 3, 4 connected)
    let q_new = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"3","target":"4"}"#,
    )
    .await;
    assert_eq!(q_new.status, 200);
    assert_eq!(q_new.json()["connected"], true);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_11_queued_old_generation_barriers_racing_replacement_are_rejected() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;

    let mut stream = running.connect().await;
    let etag = get(&mut stream, "/v1/stats")
        .await
        .header("ETag")
        .unwrap()
        .to_string();

    // Replace graph with generation 2
    import_graph(
        &running,
        "feeder-1",
        1,
        "replace",
        "pos-2",
        &[3, 4],
        &[],
        Some(&etag),
    )
    .await;

    // A delayed barrier from generation 1 arriving after generation 2 is published is rejected
    let b_old = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(b_old.status, 409);
    assert_eq!(b_old.json()["code"], "stale_generation");

    // Generation 2 remains CatchingUp
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "catching_up");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_12_lease_expiry_observed_while_graph_worker_holds_mutex() {
    let _guard = TEST_MUTEX.lock().await;
    reset_clock();
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 10_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[(1, 2)],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Pause next worker inside mutex
    let mut pause = running.lifecycle.pause_next_worker();
    let query_task = {
        let address = running.address;
        tokio::spawn(async move {
            let mut s = TcpStream::connect(address).await.unwrap();
            post(
                &mut s,
                "/v1/operations",
                &[],
                r#"{"op":"connected","source":"1","target":"2"}"#,
            )
            .await
        })
    };

    pause.wait_started().await;

    // Worker now holds the mutex! Advance virtual monotonic clock by 11,000ms
    advance_clock(Duration::from_millis(11_000));

    // GET /ready must observe lease expiry lock-free without blocking behind the worker!
    let ready_res = timeout(Duration::from_millis(500), async {
        let mut s = running.connect().await;
        get(&mut s, "/ready").await
    })
    .await
    .expect("readiness probe must not block on graph mutex!");

    assert_eq!(ready_res.status, 503);
    assert_eq!(ready_res.json()["status"], "stale");

    // Release worker
    pause.release();
    assert_eq!(query_task.await.unwrap().status, 200);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}

#[tokio::test]
async fn test_13_heartbeat_after_expiry_does_not_restore_ready() {
    let _guard = TEST_MUTEX.lock().await;
    reset_clock();
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 10_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Advance clock past 10s lease
    advance_clock(Duration::from_millis(10_500));

    // Probing /ready transitions it to stale
    let r1 = get(&mut stream, "/ready").await;
    assert_eq!(r1.status, 503);
    assert_eq!(r1.json()["status"], "stale");

    // Heartbeat renewal extends lease deadline but reports status: "stale"
    let hb = heartbeat(&running, "feeder-1", 1).await;
    assert_eq!(hb.status, 200);
    assert_eq!(hb.json()["result"], "heartbeat_acknowledged");
    assert_eq!(hb.json()["status"], "stale");

    // /ready probe MUST REMAIN 503 stale!
    let r2 = get(&mut stream, "/ready").await;
    assert_eq!(r2.status, 503);
    assert_eq!(r2.json()["status"], "stale");

    // Attempting synchronization under expired epoch 1 is fenced
    let fenced = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(fenced.status, 409);
    assert_eq!(fenced.json()["code"], "fenced_feeder_session");

    // Feeder registers new epoch 2 for synchronization recovery
    let sess2 = register_session(&running, "feeder-1", 2, 10_000).await;
    assert_eq!(sess2.status, 200);

    // Only verifying synchronization barrier under new epoch restores Ready
    let b = declare_barrier(&running, "feeder-1", 2, 1, 0, Some("pos-1")).await;
    assert_eq!(b.status, 200);
    assert_eq!(b.json()["result"], "synchronized");
    assert_eq!(b.json()["status"], "ready");

    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}

#[tokio::test]
async fn test_14_session_retries_and_takeovers_cannot_bypass_synchronization_recovery() {
    let _guard = TEST_MUTEX.lock().await;
    reset_clock();
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 10_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // 1. Expire lease
    advance_clock(Duration::from_millis(10_500));
    assert_eq!(get(&mut stream, "/ready").await.status, 503);

    // 2. Exact session retry after expiry returns status: "stale"
    let retry = register_session(&running, "feeder-1", 1, 10_000).await;
    assert_eq!(retry.status, 200);
    assert_eq!(retry.json()["status"], "stale");
    assert_eq!(get(&mut stream, "/ready").await.status, 503);

    // Barrier under expired epoch 1 is fenced
    let fenced_b1 = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(fenced_b1.status, 409);
    assert_eq!(fenced_b1.json()["code"], "fenced_feeder_session");

    // Restore ready via newly registered epoch 2 and barrier
    let sess2 = register_session(&running, "feeder-1", 2, 10_000).await;
    assert_eq!(sess2.status, 200);
    let b = declare_barrier(&running, "feeder-1", 2, 1, 0, Some("pos-1")).await;
    assert_eq!(b.status, 200);
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // 3. Session takeover by feeder-2 with epoch 3 transitions Ready to Stale
    let takeover = register_session(&running, "feeder-2", 3, 10_000).await;
    assert_eq!(takeover.status, 200);
    assert_eq!(takeover.json()["status"], "stale");
    assert_eq!(get(&mut stream, "/ready").await.status, 503);

    // 4. Delayed barrier from feeder-1 with epoch 2 is fenced
    let fenced_b = declare_barrier(&running, "feeder-1", 2, 1, 0, Some("pos-1")).await;
    assert_eq!(fenced_b.status, 409);
    assert_eq!(fenced_b.json()["code"], "fenced_feeder_session");

    // 5. Valid barrier from authoritative feeder-2 restores Ready
    let valid_b = declare_barrier(&running, "feeder-2", 3, 1, 0, Some("pos-1")).await;
    assert_eq!(valid_b.status, 200);
    assert_eq!(valid_b.json()["result"], "synchronized");
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}

#[tokio::test]
async fn test_15_desynchronized_and_permanently_failed_states_cannot_be_cleared() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;

    // Declare unrecoverable gap
    let gap = declare_gap(&running, "feeder-1", 1, 1, 1).await;
    assert_eq!(gap.status, 200);
    assert_eq!(gap.json()["status"], "desynchronized");

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 503);
    assert_eq!(
        get(&mut stream, "/ready").await.json()["status"],
        "desynchronized"
    );

    // Neither heartbeat nor barrier can clear Desynchronized
    let hb = heartbeat(&running, "feeder-1", 1).await;
    assert_eq!(hb.status, 200);
    assert_eq!(hb.json()["status"], "desynchronized");

    let b = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(b.status, 409);
    assert_eq!(b.json()["code"], "graph_desynchronized");

    assert_eq!(
        get(&mut stream, "/ready").await.json()["status"],
        "desynchronized"
    );

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_16_draining_cannot_be_reversed_by_feeder_requests() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let inst_id = running.instance_id().await;
    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    let (gate_tx, mut gate_rx) = tokio::sync::mpsc::unbounded_channel();
    running.lifecycle.on_gate(move |path, decision| {
        let _ = gate_tx.send((path.to_string(), decision));
    });

    // Keep an admitted request open while starting drain
    let mut admitted = start_unfinished_request(&running).await;
    let (path, decision) = gate_rx.recv().await.expect("gate decision");
    assert_eq!(path, "/v1/operations");
    assert_eq!(decision, knotrel_server::GateDecision::Admitted);

    // Initiate draining via shutdown signal
    running.shutdown();
    wait_for_phase(&running.lifecycle, Phase::Draining).await;

    // Probes opened during drain are answered once with 503 draining
    let mut stream_probe = running.connect().await;
    let ready = get(&mut stream_probe, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "draining");

    // Feeder requests rejected with 503 shutting_down
    let mut s2 = running.connect().await;
    let hb_body =
        format!(r#"{{"instance_id":"{inst_id}","feeder_session_id":"feeder-1","feeder_epoch":1}}"#);
    let hb = post(
        &mut s2,
        "/v1/feeder/heartbeat",
        &[("X-Knotrel-Instance-Id", &inst_id)],
        &hb_body,
    )
    .await;
    assert_eq!(hb.status, 503);
    assert_eq!(hb.json()["code"], "shutting_down");

    let mut s3 = running.connect().await;
    let b_body = format!(
        r#"{{"instance_id":"{inst_id}","graph_id":"default","generation":1,"feeder_session_id":"feeder-1","feeder_epoch":1,"barrier_delivery_sequence":0,"barrier_source_position":"pos-1"}}"#
    );
    let b = post(
        &mut s3,
        "/v1/feeder/barrier",
        &[("X-Knotrel-Instance-Id", &inst_id)],
        &b_body,
    )
    .await;
    assert_eq!(b.status, 503);
    assert_eq!(b.json()["code"], "shutting_down");

    // Complete admitted request so server finishes cleanly
    admitted.write_all(b"}").await.unwrap();
    let _ = read_reply(&mut admitted).await;

    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_17_caller_cancellation_and_worker_panic_preserve_lifecycle() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 30_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Pause worker and instruct it to panic
    let mut pause = running.lifecycle.pause_next_worker();
    let query_task = {
        let address = running.address;
        tokio::spawn(async move {
            let mut s = TcpStream::connect(address).await.unwrap();
            post(
                &mut s,
                "/v1/operations",
                &[],
                r#"{"op":"connected","source":"1","target":"2"}"#,
            )
            .await
        })
    };

    pause.wait_started().await;
    pause.panic_on_release();
    let _ = query_task.await;

    // Server must permanently be unavailable
    let ready = get(&mut stream, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "unavailable");

    // Barrier rejected with 503 unavailable
    let b = declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;
    assert_eq!(b.status, 503);
    assert_eq!(b.json()["code"], "unavailable");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_18_standalone_behavior_and_features_remain_compatible() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running = Running::start(config_standalone()).await;

    let mut stream = running.connect().await;
    // Standalone is ready immediately
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Direct mutations work as expected
    let link_res = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(link_res.status, 200);
    assert_eq!(link_res.json()["changed"], true);

    // Feeder endpoints return 409 mode_conflict
    let barrier = post(&mut stream, "/v1/feeder/barrier", &[], "{}").await;
    assert_eq!(barrier.status, 409);
    assert_eq!(barrier.json()["code"], "mode_conflict");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_19_restart_requires_fresh_instance_identity_and_rebuilding() {
    let _guard = TEST_MUTEX.lock().await;
    let mut running_1 = Running::start(config_source_managed()).await;
    let inst_1 = running_1.instance_id().await;
    register_session(&running_1, "feeder-1", 1, 30_000).await;
    import_graph(
        &running_1,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[],
        None,
    )
    .await;
    declare_barrier(&running_1, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream_1 = running_1.connect().await;
    assert_eq!(get(&mut stream_1, "/ready").await.status, 200);

    running_1.shutdown();
    assert_eq!(running_1.finished().await, ShutdownOutcome::Drained);

    // Start a new process instance
    let mut running_2 = Running::start(config_source_managed()).await;
    let inst_2 = running_2.instance_id().await;
    assert_ne!(
        inst_1, inst_2,
        "restart must generate a fresh ephemeral instance ID"
    );

    let mut stream_2 = running_2.connect().await;
    // New server is unready
    assert_eq!(get(&mut stream_2, "/ready").await.status, 503);
    assert_eq!(
        get(&mut stream_2, "/ready").await.json()["status"],
        "uninitialized"
    );

    // Barrier targeting old instance_id is rejected with 409 stale_instance_id
    let body = format!(
        r#"{{"instance_id":"{inst_1}","graph_id":"default","generation":1,"feeder_session_id":"feeder-1","feeder_epoch":1,"barrier_delivery_sequence":0,"barrier_source_position":"pos-1"}}"#
    );
    let stale_b = post(
        &mut stream_2,
        "/v1/feeder/barrier",
        &[("X-Knotrel-Instance-Id", &inst_1)],
        &body,
    )
    .await;
    assert_eq!(stale_b.status, 409);
    assert_eq!(stale_b.json()["code"], "stale_instance_id");

    running_2.shutdown();
    assert_eq!(running_2.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_20_delayed_barrier_fenced_across_lease_expiry() {
    let _guard = TEST_MUTEX.lock().await;
    reset_clock();
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 10_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[(1, 2)],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // Assert stats metadata and query serving are ready
    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["sync_status"], "ready");

    let query_res = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(query_res.status, 200);
    assert_eq!(query_res.header("x-knotrel-sync-status"), Some("ready"));
    assert_eq!(query_res.json()["connected"], true);

    // 1. Prepare barrier under epoch 1, but pause worker before lock acquisition
    let mut pause = running.lifecycle.pause_next_worker();
    let address = running.address;
    let inst_id = running.instance_id().await;
    let barrier_task = tokio::spawn(async move {
        let mut s = TcpStream::connect(address).await.unwrap();
        let body = format!(
            r#"{{"instance_id":"{inst_id}","graph_id":"default","generation":1,"feeder_session_id":"feeder-1","feeder_epoch":1,"barrier_delivery_sequence":0,"barrier_source_position":"pos-1"}}"#
        );
        post(
            &mut s,
            "/v1/feeder/barrier",
            &[("X-Knotrel-Instance-Id", &inst_id)],
            &body,
        )
        .await
    });

    pause.wait_started().await;

    // 2. Lease expires while barrier is delayed in worker queue
    advance_clock(Duration::from_millis(10_500));

    // 3. Heartbeat renews the same session/epoch; extends deadline but reports Stale
    let hb = heartbeat(&running, "feeder-1", 1).await;
    assert_eq!(hb.status, 200);
    assert_eq!(hb.json()["result"], "heartbeat_acknowledged");
    assert_eq!(hb.json()["status"], "stale");

    // 4. Release the delayed barrier
    pause.release();
    let barrier_reply = barrier_task.await.unwrap();

    // 5. Assert: delayed pre-expiry barrier CANNOT promote Stale to Ready; it is fenced!
    assert_eq!(barrier_reply.status, 409);
    assert_eq!(barrier_reply.json()["code"], "fenced_feeder_session");

    // 6. Readiness remains 503 stale
    let r = get(&mut stream, "/ready").await;
    assert_eq!(r.status, 503);
    assert_eq!(r.json()["status"], "stale");

    // 7. /stats metadata reports stale
    let s = get(&mut stream, "/v1/stats").await;
    assert_eq!(s.json()["sync_status"], "stale");

    // 8. Query serving carries stale metadata
    let q = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(q.header("x-knotrel-sync-status"), Some("stale"));

    // 9. Legitimate recovery: feeder registers a new epoch
    let sess2 = register_session(&running, "feeder-1", 2, 10_000).await;
    assert_eq!(sess2.status, 200);

    // 10. Fresh barrier under newly registered epoch 2 restores Ready
    let fresh_b = declare_barrier(&running, "feeder-1", 2, 1, 0, Some("pos-1")).await;
    assert_eq!(fresh_b.status, 200);
    assert_eq!(fresh_b.json()["result"], "synchronized");
    assert_eq!(fresh_b.json()["status"], "ready");

    assert_eq!(get(&mut stream, "/ready").await.status, 200);
    let final_stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(final_stats.json()["sync_status"], "ready");

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}

#[tokio::test]
async fn test_21_probe_paused_at_expiry_does_not_overwrite_newer_publication() {
    let _guard = TEST_MUTEX.lock().await;
    reset_clock();
    let mut running = Running::start(config_source_managed()).await;
    register_session(&running, "feeder-1", 1, 10_000).await;
    import_graph(
        &running,
        "feeder-1",
        1,
        "create_only",
        "pos-1",
        &[1, 2],
        &[(1, 2)],
        None,
    )
    .await;
    declare_barrier(&running, "feeder-1", 1, 1, 0, Some("pos-1")).await;

    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/ready").await.status, 200);

    // 1. Advance clock past 10s lease so lease expires
    advance_clock(Duration::from_millis(10_500));

    // 2. Install pause hook for /ready probe after observing lease expiry
    let mut probe_pause = running.lifecycle.pause_next_ready_probe();
    let address = running.address;
    let probe_task = tokio::spawn(async move {
        let mut s = TcpStream::connect(address).await.unwrap();
        get(&mut s, "/ready").await
    });

    probe_pause.wait_started().await;

    // 3. While probe is paused having observed old expiry, feeder registers epoch 2 and synchronizes
    let sess2 = register_session(&running, "feeder-1", 2, 10_000).await;
    assert_eq!(sess2.status, 200);

    let b = declare_barrier(&running, "feeder-1", 2, 1, 0, Some("pos-1")).await;
    assert_eq!(b.status, 200);
    assert_eq!(b.json()["result"], "synchronized");
    assert_eq!(b.json()["status"], "ready");

    // 4. Release paused probe. Its CAS against the old publication must fail without overwriting newer state!
    probe_pause.release();
    let probe_reply = probe_task.await.unwrap();

    // 5. Probe observes and preserves Ready
    assert_eq!(probe_reply.status, 200);
    assert_eq!(probe_reply.json()["status"], "ready");

    // 6. Subsequent probes also remain 200 ready (not clobbered to Stale!)
    let r2 = get(&mut stream, "/ready").await;
    assert_eq!(r2.status, 200);
    assert_eq!(r2.json()["status"], "ready");

    // 7. /stats metadata is ready
    let s = get(&mut stream, "/v1/stats").await;
    assert_eq!(s.json()["sync_status"], "ready");

    // 8. Query serving succeeds and carries ready status
    let q = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(q.status, 200);
    assert_eq!(q.header("x-knotrel-sync-status"), Some("ready"));
    assert_eq!(q.json()["connected"], true);

    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
    reset_clock();
}
