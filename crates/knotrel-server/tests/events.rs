//! Deterministic integration tests for Feature 0.2.4: Ordered Incremental Updates.
//!
//! Validates:
//! - Ordered delivery sequences and exact predecessor checking under mutex
//! - Dual-index bounded deduplication window (sequence ring buffer + event ID hash index)
//! - Exact duplicate acknowledgment vs. conflicting duplicate detection
//! - Non-mutating progress and no-op events advancing frontier without version increment
//! - Domain error atomicity (uncommitted attempts not recorded in window)
//! - Batch stop-on-first-error semantics and committed prefix reporting
//! - Bounded eviction horizons and fresh evaluation of evicted IDs
//! - Regression case: link then cut, then retry old link -> edge remains absent
//! - Race conditions: session takeover, lease expiry, replacement races, caller drop, worker panic
//! - Unrecoverable gap declarations (/v1/feeder/gap) and Desynchronized query rejection

#![cfg(feature = "test-support")]

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

fn config_source_managed() -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32)
        .unwrap()
        .with_mode(ServerMode::SourceManaged)
        .with_shutdown_timeout(Duration::from_secs(5))
        .unwrap()
}

fn config_source_managed_custom_dedup(capacity: usize) -> ServerConfig {
    config_source_managed()
        .with_dedup_window_size(capacity)
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
        &[("x-knotrel-instance-id", &instance_id)],
        &body,
    )
    .await
}

async fn import_baseline(
    running: &Running,
    instance_id: &str,
    session_id: &str,
    epoch: u64,
    source_pos: &str,
    nodes: &[&str],
    edges: &[(&str, &str)],
) -> Reply {
    let nodes_json = serde_json::to_string(nodes).unwrap();
    let edges_json: Vec<String> = edges
        .iter()
        .map(|(s, t)| format!(r#"{{"source":"{s}","target":"{t}"}}"#))
        .collect();
    let edges_str = format!("[{}]", edges_json.join(","));
    let body = format!(
        r#"{{"mode":"create_only","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"{source_pos}","feeder_session_id":"{session_id}","feeder_epoch":{epoch},"nodes":{nodes_json},"edges":{edges_str}}}"#
    );
    let mut stream = running.connect().await;
    post(
        &mut stream,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", instance_id)],
        &body,
    )
    .await
}

async fn post_events(
    running: &Running,
    instance_id: &str,
    session_id: &str,
    epoch: u64,
    events_json: &str,
) -> Reply {
    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"{session_id}","feeder_epoch":{epoch},"source_id":"pg-1","source_epoch":"ep-1","events":{events_json}}}"#
    );
    let mut stream = running.connect().await;
    post(
        &mut stream,
        "/v1/feeder/events",
        &[("x-knotrel-instance-id", instance_id)],
        &body,
    )
    .await
}

async fn query_connected(running: &Running, u: &str, v: &str) -> Reply {
    let mut stream = running.connect().await;
    let body = format!(r#"{{"op":"connected","source":"{u}","target":"{v}"}}"#);
    post(&mut stream, "/v1/operations", &[], &body).await
}

// ---------------------------------------------------------------------------
// 1. Successful ordered mutations & progress events (TR-01, TR-16)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_1_successful_ordered_mutations_and_progress_events() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );

    let imp = import_baseline(
        &running,
        &instance_id,
        "f-1",
        1,
        "pos-10",
        &["1", "2"],
        &[("1", "2")],
    )
    .await;
    assert_eq!(imp.status, 201);
    assert_eq!(imp.json()["state_version"], "0");

    // Event 1: link(2, 3) (creates node 3, edge 2-3). Effective mutation -> state_version increments to 1.
    let e1 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-11","prev_source_position":"pos-10","op":"link","source":"2","target":"3"}]"#,
    )
    .await;
    assert_eq!(e1.status, 200);
    let j1 = e1.json();
    assert_eq!(j1["result"], "applied");
    assert_eq!(j1["applied_delivery_sequence"], 1);
    assert_eq!(j1["applied_source_position"], "pos-11");
    assert_eq!(j1["state_version"], "1");
    assert_eq!(j1["events_applied"], 1);

    // Event 2: progress event. Non-mutating -> state_version stays 1, position advances to pos-12.
    let e2 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-12","prev_source_position":"pos-11","op":"progress"}]"#,
    )
    .await;
    assert_eq!(e2.status, 200);
    let j2 = e2.json();
    assert_eq!(j2["result"], "applied");
    assert_eq!(j2["applied_delivery_sequence"], 2);
    assert_eq!(j2["applied_source_position"], "pos-12");
    assert_eq!(j2["state_version"], "1"); // UNCHANGED

    // Event 3: no-op link(2, 3) (already connected). No topological change -> state_version stays 1.
    let e3 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-3","delivery_sequence":3,"prev_delivery_sequence":2,"source_position":"pos-13","prev_source_position":"pos-12","op":"link","source":"2","target":"3"}]"#,
    )
    .await;
    assert_eq!(e3.status, 200);
    let j3 = e3.json();
    assert_eq!(j3["applied_delivery_sequence"], 3);
    assert_eq!(j3["state_version"], "1"); // UNCHANGED

    // Event 4: cut(1, 2). Effective mutation -> state_version increments to 2.
    let e4 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-4","delivery_sequence":4,"prev_delivery_sequence":3,"source_position":"pos-14","prev_source_position":"pos-13","op":"cut","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(e4.status, 200);
    assert_eq!(e4.json()["state_version"], "2");

    // Event 5: add_node(9). Effective mutation -> state_version increments to 3.
    let e5 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-5","delivery_sequence":5,"prev_delivery_sequence":4,"source_position":"pos-15","prev_source_position":"pos-14","op":"add_node","node":"9"}]"#,
    )
    .await;
    assert_eq!(e5.status, 200);
    assert_eq!(e5.json()["state_version"], "3");

    // Verify queries reflect mutations
    assert_eq!(
        query_connected(&running, "2", "3").await.json()["connected"],
        true
    );
    assert_eq!(
        query_connected(&running, "1", "2").await.json()["connected"],
        false
    );
}

// ---------------------------------------------------------------------------
// 2. Exact and conflicting duplicate retries (TR-03, TR-04)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_2_exact_and_conflicting_retries() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(
            &running,
            &instance_id,
            "f-1",
            1,
            "pos-10",
            &["1", "2"],
            &[("1", "2")]
        )
        .await
        .status,
        201
    );

    // Apply seq 1: link(2, 3)
    let e1 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-11","prev_source_position":"pos-10","op":"link","source":"2","target":"3"}]"#,
    )
    .await;
    assert_eq!(e1.status, 200);

    // Apply seq 2: link(3, 4)
    let e2 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-12","prev_source_position":"pos-11","op":"link","source":"3","target":"4"}]"#,
    )
    .await;
    assert_eq!(e2.status, 200);
    assert_eq!(e2.json()["state_version"], "2");

    // Exact retry of seq 1
    let retry = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-11","prev_source_position":"pos-10","op":"link","source":"2","target":"3"}]"#,
    )
    .await;
    assert_eq!(retry.status, 200);
    let rj = retry.json();
    assert_eq!(rj["result"], "duplicate");
    assert_eq!(rj["status"], "already_applied");
    assert_eq!(rj["event_id"], "evt-1");
    assert_eq!(rj["delivery_sequence"], 1);
    assert_eq!(rj["historical_outcome"]["changed"], true);
    assert_eq!(rj["current_applied_delivery_sequence"], 2);
    assert_eq!(rj["current_state_version"], "2");

    // Conflicting retry: seq 1 with altered content (cut 2-3 instead of link)
    let conflict = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-11","prev_source_position":"pos-10","op":"cut","source":"2","target":"3"}]"#,
    )
    .await;
    assert_eq!(conflict.status, 409);
    assert_eq!(conflict.json()["code"], "conflicting_duplicate");

    // Conflict must transition server to Desynchronized -> queries return 503 desynchronized
    let query = query_connected(&running, "2", "3").await;
    assert_eq!(query.status, 503);
    assert_eq!(query.json()["code"], "desynchronized");
}

// ---------------------------------------------------------------------------
// 3. Critical regression case: link then cut, then retry old link (edge absent!)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_3_critical_regression_old_link_retry_after_newer_cut() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1", "2"], &[])
            .await
            .status,
        201
    );

    // 1. link(1, 2) at seq 1
    let e1 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-link","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(e1.status, 200);
    assert_eq!(
        query_connected(&running, "1", "2").await.json()["connected"],
        true
    );

    // 2. cut(1, 2) at seq 2
    let e2 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-cut","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"cut","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(e2.status, 200);
    assert_eq!(
        query_connected(&running, "1", "2").await.json()["connected"],
        false
    );

    // 3. Retry old link event: seq 1, evt-link
    let retry_link = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-link","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(retry_link.status, 200);
    assert_eq!(retry_link.json()["status"], "already_applied");

    // CRITICAL: edge 1-2 MUST REMAIN ABSENT!
    let q_after = query_connected(&running, "1", "2").await;
    assert_eq!(q_after.status, 200);
    assert_eq!(q_after.json()["connected"], false);
}

// ---------------------------------------------------------------------------
// 4. Retained sequence with different event ID & retained ID at new sequence (TR-32, TR-33)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_4_retained_sequence_and_event_id_conflicts() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1", "2"], &[])
            .await
            .status,
        201
    );

    // Commit seq 1 with evt-01
    assert_eq!(
        post_events(
            &running,
            &instance_id,
            "f-1",
            1,
            r#"[{"event_id":"evt-01","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
        )
        .await
        .status,
        200
    );

    // TR-32: Retained sequence 1 with different event ID evt-99
    let tr32 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-99","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(tr32.status, 409);
    assert_eq!(tr32.json()["code"], "conflicting_duplicate");
    assert_eq!(query_connected(&running, "1", "2").await.status, 503);

    // Start fresh server for TR-33
    let running2 = Running::start(config_source_managed()).await;
    let instance_id2 = running2.instance_id().await;
    assert_eq!(
        register_session(&running2, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(
            &running2,
            &instance_id2,
            "f-1",
            1,
            "pos-0",
            &["1", "2"],
            &[]
        )
        .await
        .status,
        201
    );

    // Commit seq 1 with evt-01
    assert_eq!(
        post_events(
            &running2,
            &instance_id2,
            "f-1",
            1,
            r#"[{"event_id":"evt-01","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
        )
        .await
        .status,
        200
    );

    // TR-33: Retained event ID evt-01 reused at new sequence 2
    let tr33 = post_events(
        &running2,
        &instance_id2,
        "f-1",
        1,
        r#"[{"event_id":"evt-01","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"add_node","node":"5"}]"#,
    )
    .await;
    assert_eq!(tr33.status, 409);
    assert_eq!(tr33.json()["code"], "conflicting_duplicate");
    assert_eq!(query_connected(&running2, "1", "2").await.status, 503);
}

// ---------------------------------------------------------------------------
// 5. Bounded window eviction boundaries and fresh evaluation of evicted IDs (TR-34)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_5_bounded_window_eviction_boundary() {
    // Dedup capacity = 5
    let running = Running::start(config_source_managed_custom_dedup(5)).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1", "2"], &[])
            .await
            .status,
        201
    );

    // Apply seq 1..7 (N=5; sequences 1 and 2 are evicted, sequences 3..7 retained)
    for i in 1..=7 {
        let prev_pos = format!("pos-{}", i - 1);
        let curr_pos = format!("pos-{}", i);
        let evt_id = format!("evt-{}", i);
        let res = post_events(
            &running,
            &instance_id,
            "f-1",
            1,
            &format!(
                r#"[{{ "event_id":"{evt_id}","delivery_sequence":{i},"prev_delivery_sequence":{},"source_position":"{curr_pos}","prev_source_position":"{prev_pos}","op":"add_node","node":"{i}" }}]"#,
                i - 1
            ),
        )
        .await;
        assert_eq!(res.status, 200);
    }

    // Seq 2 is evicted (min_seq = 7 - 5 + 1 = 3) -> 409 unverifiable_duplicate
    let evicted = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"add_node","node":"2"}]"#,
    )
    .await;
    assert_eq!(evicted.status, 409);
    assert_eq!(evicted.json()["code"], "unverifiable_duplicate");

    // Unverifiable duplicate does NOT desynchronize server!
    let q = query_connected(&running, "1", "1").await;
    assert_eq!(q.status, 200);

    // Seq 3 is in window -> 200 already_applied
    let in_window = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-3","delivery_sequence":3,"prev_delivery_sequence":2,"source_position":"pos-3","prev_source_position":"pos-2","op":"add_node","node":"3"}]"#,
    )
    .await;
    assert_eq!(in_window.status, 200);
    assert_eq!(in_window.json()["status"], "already_applied");

    // TR-34: Reusing evicted event ID "evt-1" at new sequence 8:
    // Evicted from bounded window -> evaluated fresh at D_applied + 1 -> succeeds!
    let fresh_evicted_id = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":8,"prev_delivery_sequence":7,"source_position":"pos-8","prev_source_position":"pos-7","op":"add_node","node":"88"}]"#,
    )
    .await;
    assert_eq!(fresh_evicted_id.status, 200);
    assert_eq!(fresh_evicted_id.json()["applied_delivery_sequence"], 8);
}

// ---------------------------------------------------------------------------
// 6. Hash collision / canonical comparison with normalized endpoints & 256-byte cursor (TR-21)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_6_canonical_safety_and_256_byte_cursor() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );

    let cursor_256 = "x".repeat(256);
    assert_eq!(
        import_baseline(
            &running,
            &instance_id,
            "f-1",
            1,
            "pos-0",
            &["10", "20"],
            &[]
        )
        .await
        .status,
        201
    );

    // Apply link(10, 20) with 256-byte cursor
    let e1 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        &format!(
            r#"[{{ "event_id":"evt-256","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"{cursor_256}","prev_source_position":"pos-0","op":"link","source":"10","target":"20" }}]"#
        ),
    )
    .await;
    assert_eq!(e1.status, 200);
    let etag = e1.header("etag").expect("etag header");
    assert!(etag.starts_with("\"1.1."));

    // Retry with reversed endpoints: link(20, 10). Normalized undirected edge matches canonically!
    let retry_reversed = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        &format!(
            r#"[{{ "event_id":"evt-256","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"{cursor_256}","prev_source_position":"pos-0","op":"link","source":"20","target":"10" }}]"#
        ),
    )
    .await;
    assert_eq!(retry_reversed.status, 200);
    assert_eq!(retry_reversed.json()["status"], "already_applied");
}

// ---------------------------------------------------------------------------
// 7. Future sequence transport gap followed by ordered recovery (TR-05)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_7_future_sequence_rejection_and_ordered_recovery() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    // Send future sequence 5 (expected 1)
    let gap = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-5","delivery_sequence":5,"prev_delivery_sequence":4,"source_position":"pos-5","prev_source_position":"pos-4","op":"add_node","node":"5"}]"#,
    )
    .await;
    assert_eq!(gap.status, 409);
    assert_eq!(gap.json()["code"], "out_of_order_sequence");
    assert_eq!(gap.json()["expected_sequence"], 1);

    // Server state preserved in CatchingUp; queries still functional
    assert_eq!(query_connected(&running, "1", "1").await.status, 200);

    // Feeder recovers and sends expected sequence 1
    let recovered = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2"}]"#,
    )
    .await;
    assert_eq!(recovered.status, 200);
    assert_eq!(recovered.json()["applied_delivery_sequence"], 1);
}

// ---------------------------------------------------------------------------
// 8. Incorrect predecessors and source epoch mismatch (TR-10, TR-20)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_8_incorrect_predecessors_and_source_epoch_mismatch() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    // TR-20: Mismatched prev_source_position
    let tr20 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-WRONG","op":"add_node","node":"2"}]"#,
    )
    .await;
    assert_eq!(tr20.status, 409);
    assert_eq!(tr20.json()["code"], "source_predecessor_mismatch");
    assert_eq!(query_connected(&running, "1", "1").await.status, 503);

    // TR-10: Source epoch mismatch
    let running2 = Running::start(config_source_managed()).await;
    let instance_id2 = running2.instance_id().await;
    assert_eq!(
        register_session(&running2, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running2, &instance_id2, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    let body_bad_epoch = format!(
        r#"{{"instance_id":"{instance_id2}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-NEW-EPOCH","events":[{{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2"}}]}}"#
    );
    let mut s = running2.connect().await;
    let tr10 = post(
        &mut s,
        "/v1/feeder/events",
        &[("x-knotrel-instance-id", &instance_id2)],
        &body_bad_epoch,
    )
    .await;
    assert_eq!(tr10.status, 409);
    assert_eq!(tr10.json()["code"], "source_epoch_mismatch");
    assert_eq!(query_connected(&running2, "1", "1").await.status, 503);
}

// ---------------------------------------------------------------------------
// 9. Domain errors preserve state and frontier, allowing payload correction (TR-26)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_9_domain_errors_preserve_frontier_and_allow_correction() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    // 1. Event 1 (self-loop link 1-1) fails with 422
    let self_loop = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"1"}]"#,
    )
    .await;
    assert_eq!(self_loop.status, 422);
    assert_eq!(self_loop.json()["code"], "self_loop");

    // Uncommitted rejected event is NOT recorded in window.
    // 2. Feeder retries sequence 1 with corrected payload (link 1-2)
    let corrected = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(corrected.status, 200);
    assert_eq!(corrected.json()["applied_delivery_sequence"], 1);

    // 3. Now committed: retrying evt-1 with altered payload is rejected as conflicting duplicate
    let altered = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"evt-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"cut","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(altered.status, 409);
    assert_eq!(altered.json()["code"], "conflicting_duplicate");
}

// ---------------------------------------------------------------------------
// 10. Partial batch failures at nontrivial global sequence (TR-22)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_10_partial_batch_failures_at_nontrivial_sequence() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(
            &running,
            &instance_id,
            "f-1",
            1,
            "pos-0",
            &["1", "2", "3"],
            &[]
        )
        .await
        .status,
        201
    );

    // Send batch of 3 events starting at seq 1:
    // Index 0: seq 1, link 1-2 (valid)
    // Index 1: seq 2, progress (valid)
    // Index 2: seq 3, self-loop 3-3 (invalid)
    let batch_res = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[
            {"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"},
            {"event_id":"e-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"progress"},
            {"event_id":"e-3","delivery_sequence":3,"prev_delivery_sequence":2,"source_position":"pos-3","prev_source_position":"pos-2","op":"link","source":"3","target":"3"}
        ]"#,
    )
    .await;
    assert_eq!(batch_res.status, 422);
    let bj = batch_res.json();
    assert_eq!(bj["result"], "partial_failure");
    assert_eq!(bj["failed_batch_index"], 2);
    assert_eq!(bj["failed_delivery_sequence"], 3);
    assert_eq!(bj["failed_event_id"], "e-3");
    assert_eq!(bj["applied_delivery_sequence"], 2);
    assert_eq!(bj["applied_source_position"], "pos-2");
    assert_eq!(bj["error"]["code"], "self_loop");

    // Events 1 and 2 are committed: edge 1-2 exists
    assert_eq!(
        query_connected(&running, "1", "2").await.json()["connected"],
        true
    );

    // Retry batch with corrected index 2 (link 3-4):
    // Prefix [1, 2] recognized as already applied in window; seq 3 applied!
    let retry_batch = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[
            {"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"},
            {"event_id":"e-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"progress"},
            {"event_id":"e-3","delivery_sequence":3,"prev_delivery_sequence":2,"source_position":"pos-3","prev_source_position":"pos-2","op":"link","source":"3","target":"4"}
        ]"#,
    )
    .await;
    assert_eq!(retry_batch.status, 200);
    assert_eq!(retry_batch.json()["applied_delivery_sequence"], 3);
    assert_eq!(retry_batch.json()["events_applied"], 1);
    assert_eq!(
        query_connected(&running, "3", "4").await.json()["connected"],
        true
    );
}

// ---------------------------------------------------------------------------
// 11. Lost acknowledgment / caller cancellation followed by retry (TR-07)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_11_caller_cancellation_followed_by_retry() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1", "2"], &[])
            .await
            .status,
        201
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}}]}}"#
    );
    let request = format!(
        "POST /v1/feeder/events HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Caller drops connection (lost ACK / client timeout)
    drop(stream);
    assert_eq!(running.lifecycle.outstanding_jobs(), 1);

    // Release worker: completes mutation under mutex
    pause.release();
    running.wait_for_jobs(0).await;

    // Retry event e-1: hits deduplication window -> 200 already_applied
    let retry = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
    )
    .await;
    assert_eq!(retry.status, 200);
    assert_eq!(retry.json()["status"], "already_applied");
}

// ---------------------------------------------------------------------------
// 12. Queued events racing baseline replacement (TR-08)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_12_queued_events_racing_baseline_replacement() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1", "2"], &[])
            .await
            .status,
        201
    );

    // Replacement candidate is built off-lock with If-Match token for generation 1, revision 0, pos-0
    // But before replacement publishes, an event arrives and increments revision to 1
    assert_eq!(
        post_events(
            &running,
            &instance_id,
            "f-1",
            1,
            r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"link","source":"1","target":"2"}]"#,
        )
        .await
        .status,
        200
    );

    // Replacement now attempts atomic publication with now-stale If-Match "1.0.pos%2D0"
    let mut s = running.connect().await;
    let replace_body = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-new","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#
    );
    let res = post(
        &mut s,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", "\"1.0.pos%2D0\""),
        ],
        &replace_body,
    )
    .await;
    assert_eq!(res.status, 412);
    assert_eq!(res.json()["code"], "precondition_failed");
}

// ---------------------------------------------------------------------------
// 13. Session takeover and lease expiry before execution (TR-09, TR-11)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_13_session_takeover_and_lease_expiry_before_execution() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "feeder-A", 1, 10000)
            .await
            .status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "feeder-A", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    // Part A: Session Takeover
    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream_a = running.connect().await;

    let body_a = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"feeder-A","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2"}}]}}"#
    );
    let request_a = format!(
        "POST /v1/feeder/events HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{body_a}",
        body_a.len()
    );
    stream_a.write_all(request_a.as_bytes()).await.unwrap();

    pause.wait_started().await;

    // Feeder B preempts Feeder A with higher epoch
    assert_eq!(
        register_session(&running, "feeder-B", 2, 1000).await.status,
        200
    );

    // Release Feeder A: recheck under lock catches fenced session
    pause.release();
    let reply_a = read_reply(&mut stream_a).await.unwrap();
    assert_eq!(reply_a.status, 409);
    assert_eq!(reply_a.json()["code"], "fenced_feeder_session");

    // Part B: Lease Expiry
    let mut pause_b = running.lifecycle.pause_next_worker();
    let mut stream_b = running.connect().await;

    let body_b = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"feeder-B","feeder_epoch":2,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2"}}]}}"#
    );
    let request_b = format!(
        "POST /v1/feeder/events HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{body_b}",
        body_b.len()
    );
    stream_b.write_all(request_b.as_bytes()).await.unwrap();

    pause_b.wait_started().await;
    // Let 1000ms lease expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    pause_b.release();
    let reply_b = read_reply(&mut stream_b).await.unwrap();
    assert_eq!(reply_b.status, 409);
    assert_eq!(reply_b.json()["code"], "lease_expired");
}

// ---------------------------------------------------------------------------
// 14. Worker panic after caller cancellation marks service unavailable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_14_worker_panic_after_caller_cancellation() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    let mut pause = running.lifecycle.pause_next_worker();
    let mut stream = running.connect().await;

    let body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2"}}]}}"#
    );
    let request = format!(
        "POST /v1/feeder/events HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {instance_id}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    pause.wait_started().await;
    drop(stream);

    pause.panic_on_release();
    running.wait_for_jobs(0).await;

    let mut s1 = running.connect().await;
    let ready = get(&mut s1, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "unavailable");
}

// ---------------------------------------------------------------------------
// 15. Restart rejecting previous instance requests (TR-02, TR-12)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_15_restart_rejects_previous_instance_requests() {
    let mut running1 = Running::start(config_source_managed()).await;
    let instance_id1 = running1.instance_id().await;
    running1.shutdown();
    running1.finished().await;

    let running2 = Running::start(config_source_managed()).await;
    let instance_id2 = running2.instance_id().await;
    assert_ne!(instance_id1, instance_id2);

    // Event with old instance ID header is rejected
    let mut stream = running2.connect().await;
    let body = format!(
        r#"{{"instance_id":"{instance_id2}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[]}}"#
    );
    let r = post(
        &mut stream,
        "/v1/feeder/events",
        &[("x-knotrel-instance-id", &instance_id1)],
        &body,
    )
    .await;
    assert_eq!(r.status, 409);
    assert_eq!(r.json()["code"], "stale_instance_id");
}

// ---------------------------------------------------------------------------
// 16. Compatibility with standalone mode and direct mutation rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_16_compatibility_with_standalone_and_direct_mutations() {
    // 1. Standalone server rejects feeder endpoints with mode_conflict
    let running_standalone = Running::start(
        ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32).unwrap(),
    )
    .await;
    let mut s_std = running_standalone.connect().await;
    let r_std = post(&mut s_std, "/v1/feeder/events", &[], "{}").await;
    assert_eq!(r_std.status, 409);
    assert_eq!(r_std.json()["code"], "mode_conflict");

    let r_gap_std = post(&mut s_std, "/v1/feeder/gap", &[], "{}").await;
    assert_eq!(r_gap_std.status, 409);
    assert_eq!(r_gap_std.json()["code"], "mode_conflict");

    // 2. Source-managed server rejects direct mutations with direct_mutation_disabled
    let running_sm = Running::start(config_source_managed()).await;
    let mut s_sm = running_sm.connect().await;
    let r_dir = post(
        &mut s_sm,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(r_dir.status, 409);
    assert_eq!(r_dir.json()["code"], "direct_mutation_disabled");
}

// ---------------------------------------------------------------------------
// 17. Declared gap endpoint (/v1/feeder/gap) and Desynchronized query rejection (TR-18, TR-23)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_17_declared_gap_and_desynchronized_state() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(
            &running,
            &instance_id,
            "f-1",
            1,
            "pos-0",
            &["1", "2"],
            &[("1", "2")]
        )
        .await
        .status,
        201
    );

    // Declare gap
    let gap_body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"failed_delivery_sequence":5,"reason":"source history lost"}}"#
    );
    let mut s = running.connect().await;
    let gap_res = post(
        &mut s,
        "/v1/feeder/gap",
        &[("x-knotrel-instance-id", &instance_id)],
        &gap_body,
    )
    .await;
    assert_eq!(gap_res.status, 200);
    let gj = gap_res.json();
    assert_eq!(gj["result"], "gap_acknowledged");
    assert_eq!(gj["status"], "desynchronized");
    assert_eq!(gj["action_required"], "full_rebuild_required");

    // All queries return 503 desynchronized
    let q = query_connected(&running, "1", "2").await;
    assert_eq!(q.status, 503);
    assert_eq!(q.json()["code"], "desynchronized");

    // Readiness probe returns 503 desynchronized
    let mut s_ready = running.connect().await;
    let ready = get(&mut s_ready, "/ready").await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["status"], "desynchronized");

    // TR-18: Heartbeat renews lease but probe remains 503 desynchronized
    let mut s_hb = running.connect().await;
    let hb_body =
        format!(r#"{{"instance_id":"{instance_id}","feeder_session_id":"f-1","feeder_epoch":1}}"#);
    let hb_res = post(
        &mut s_hb,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_body,
    )
    .await;
    assert_eq!(hb_res.status, 200);
    assert_eq!(hb_res.json()["status"], "desynchronized");

    let mut s_ready2 = running.connect().await;
    let ready2 = get(&mut s_ready2, "/ready").await;
    assert_eq!(ready2.status, 503);
    assert_eq!(ready2.json()["status"], "desynchronized");
}

// ---------------------------------------------------------------------------
// 18. Payload schema and bounds validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_18_payload_schema_and_bounds_validation() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(&running, &instance_id, "f-1", 1, "pos-0", &["1"], &[])
            .await
            .status,
        201
    );

    // Empty events array -> 422 invalid_request
    let empty = post_events(&running, &instance_id, "f-1", 1, "[]").await;
    assert_eq!(empty.status, 422);
    assert_eq!(empty.json()["code"], "invalid_request");

    // Invalid op -> 422 invalid_request
    let bad_op = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"unsupported_op"}]"#,
    )
    .await;
    assert_eq!(bad_op.status, 422);

    // Oversized event_id (> 64) -> 422 invalid_request
    let long_id = "i".repeat(65);
    let bad_id = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        &format!(
            r#"[{{ "event_id":"{long_id}","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"2" }}]"#
        ),
    )
    .await;
    assert_eq!(bad_id.status, 422);

    // Oversized source_position (> 256) -> 422 invalid_request
    let long_pos = "p".repeat(257);
    let bad_pos = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        &format!(
            r#"[{{ "event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"{long_pos}","prev_source_position":"pos-0","op":"add_node","node":"2" }}]"#
        ),
    )
    .await;
    assert_eq!(bad_pos.status, 422);
}

// ---------------------------------------------------------------------------
// 19. Stop event application after desynchronization and rebuild recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_19_stop_event_application_after_desynchronization_and_rebuild_recovery() {
    let running = Running::start(config_source_managed()).await;
    let instance_id = running.instance_id().await;
    assert_eq!(
        register_session(&running, "f-1", 1, 10000).await.status,
        200
    );
    assert_eq!(
        import_baseline(
            &running,
            &instance_id,
            "f-1",
            1,
            "pos-0",
            &["1", "2"],
            &[("1", "2")]
        )
        .await
        .status,
        201
    );

    // Apply initial event at D=1
    let ev1 = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-1","prev_source_position":"pos-0","op":"add_node","node":"3"}]"#,
    )
    .await;
    assert_eq!(ev1.status, 200);
    assert_eq!(ev1.json()["applied_delivery_sequence"], 1);

    // Verify initial state via /v1/stats
    let mut s_stats = running.connect().await;
    let stats1 = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats1.status, 200);
    let sj1 = stats1.json();
    assert_eq!(sj1["generation"], 1);
    assert_eq!(sj1["source_position"], "pos-1");
    assert_eq!(sj1["state_version"], "1");
    assert_eq!(sj1["node_count"], 3);
    assert_eq!(sj1["edge_count"], 1);
    assert_eq!(sj1["sync_status"], "catching_up");
    let etag_gen1 = stats1.header("etag").expect("etag header").to_string();

    // Trigger desynchronization via declared gap (POST /v1/feeder/gap)
    let gap_body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":1,"feeder_session_id":"f-1","feeder_epoch":1,"failed_delivery_sequence":5,"reason":"unrecoverable gap test"}}"#
    );
    let mut s_gap = running.connect().await;
    let gap_res = post(
        &mut s_gap,
        "/v1/feeder/gap",
        &[("x-knotrel-instance-id", &instance_id)],
        &gap_body,
    )
    .await;
    assert_eq!(gap_res.status, 200);
    assert_eq!(gap_res.json()["status"], "desynchronized");

    // /stats remains available for diagnosis
    let mut s_stats2 = running.connect().await;
    let stats2 = get(&mut s_stats2, "/v1/stats").await;
    assert_eq!(stats2.status, 200);
    let sj2 = stats2.json();
    assert_eq!(sj2["sync_status"], "desynchronized");
    assert_eq!(sj2["source_position"], "pos-1");
    assert_eq!(sj2["state_version"], "1");
    assert_eq!(sj2["node_count"], 3);

    // Queries return 503 desynchronized
    let q = query_connected(&running, "1", "2").await;
    assert_eq!(q.status, 503);
    assert_eq!(q.json()["code"], "desynchronized");

    // Heartbeat extends lease and reports desynchronized
    let mut s_hb = running.connect().await;
    let hb_body =
        format!(r#"{{"instance_id":"{instance_id}","feeder_session_id":"f-1","feeder_epoch":1}}"#);
    let hb_res = post(
        &mut s_hb,
        "/v1/feeder/heartbeat",
        &[("x-knotrel-instance-id", &instance_id)],
        &hb_body,
    )
    .await;
    assert_eq!(hb_res.status, 200);
    assert_eq!(hb_res.json()["status"], "desynchronized");

    // Submit an otherwise valid next mutation event at D=2 -> rejected with 409 graph_desynchronized
    let ev2_mutation = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"add_node","node":"4"}]"#,
    )
    .await;
    assert_eq!(ev2_mutation.status, 409);
    assert_eq!(ev2_mutation.json()["code"], "graph_desynchronized");

    // Submit an otherwise valid next progress event at D=2 -> rejected with 409 graph_desynchronized
    let ev2_progress = post_events(
        &running,
        &instance_id,
        "f-1",
        1,
        r#"[{"event_id":"e-prog","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-2","prev_source_position":"pos-1","op":"progress"}]"#,
    )
    .await;
    assert_eq!(ev2_progress.status, 409);
    assert_eq!(ev2_progress.json()["code"], "graph_desynchronized");

    // Verify rejection and preservation of the entire state
    let mut s_stats3 = running.connect().await;
    let stats3 = get(&mut s_stats3, "/v1/stats").await;
    assert_eq!(stats3.status, 200);
    let sj3 = stats3.json();
    assert_eq!(sj3["generation"], 1);
    assert_eq!(
        sj3["source_position"], "pos-1",
        "source position must not change"
    );
    assert_eq!(sj3["state_version"], "1", "state version must not advance");
    assert_eq!(sj3["node_count"], 3, "node 4 must not have been added");
    assert_eq!(sj3["edge_count"], 1);
    assert_eq!(sj3["sync_status"], "desynchronized");
    assert_eq!(
        stats3.header("etag").unwrap(),
        etag_gen1,
        "etag must remain identical"
    );

    // Rebuild through the real import path (POST /v1/feeder/import mode replace)
    let rebuild_body = format!(
        r#"{{"mode":"replace","instance_id":"{instance_id}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-rebuild-0","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["10","20"],"edges":[{{"source":"10","target":"20"}}]}}"#
    );
    let mut s_rebuild = running.connect().await;
    let rebuild_res = post(
        &mut s_rebuild,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &instance_id),
            ("if-match", &etag_gen1),
        ],
        &rebuild_body,
    )
    .await;
    assert_eq!(rebuild_res.status, 201);
    let rj = rebuild_res.json();
    assert_eq!(rj["generation"], 2);
    assert_eq!(rj["status"], "catching_up");
    assert_eq!(rj["state_version"], "0");
    assert_eq!(rj["node_count"], 2);
    assert_eq!(rj["edge_count"], 1);

    // Verify that ordered ingestion resumes for the new generation
    // 1. Submit valid mutation event in generation 2
    let gen2_ev1_body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":2,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"g2-e1","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"pos-g2-1","prev_source_position":"pos-rebuild-0","op":"add_node","node":"30"}}]}}"#
    );
    let mut s_g2_1 = running.connect().await;
    let g2_res1 = post(
        &mut s_g2_1,
        "/v1/feeder/events",
        &[("x-knotrel-instance-id", &instance_id)],
        &gen2_ev1_body,
    )
    .await;
    assert_eq!(g2_res1.status, 200);
    assert_eq!(g2_res1.json()["generation"], 2);
    assert_eq!(g2_res1.json()["applied_delivery_sequence"], 1);

    // 2. Submit valid progress event in generation 2
    let gen2_ev2_body = format!(
        r#"{{"instance_id":"{instance_id}","graph_id":"default","generation":2,"feeder_session_id":"f-1","feeder_epoch":1,"source_id":"pg-1","source_epoch":"ep-1","events":[{{"event_id":"g2-e2","delivery_sequence":2,"prev_delivery_sequence":1,"source_position":"pos-g2-2","prev_source_position":"pos-g2-1","op":"progress"}}]}}"#
    );
    let mut s_g2_2 = running.connect().await;
    let g2_res2 = post(
        &mut s_g2_2,
        "/v1/feeder/events",
        &[("x-knotrel-instance-id", &instance_id)],
        &gen2_ev2_body,
    )
    .await;
    assert_eq!(g2_res2.status, 200);
    assert_eq!(g2_res2.json()["generation"], 2);
    assert_eq!(g2_res2.json()["applied_delivery_sequence"], 2);

    // Final stats verification on generation 2
    let mut s_stats_g2 = running.connect().await;
    let stats_g2 = get(&mut s_stats_g2, "/v1/stats").await;
    assert_eq!(stats_g2.status, 200);
    let sj_g2 = stats_g2.json();
    assert_eq!(sj_g2["generation"], 2);
    assert_eq!(sj_g2["source_position"], "pos-g2-2");
    assert_eq!(sj_g2["node_count"], 3);
    assert_eq!(sj_g2["sync_status"], "catching_up");
}
