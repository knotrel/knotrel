//! Deterministic integration tests for Feature 0.2.3:
//! Identity, generation, revision, and source position.
//!
//! Covers:
//! - equal numeric revisions in distinct instances and generations
//! - independent servers constructed from cloned configuration
//! - revision carry boundaries and preservation of arbitrary precision
//! - metadata coherence when queries race atomic replacement
//! - stale replacement tokens after publication
//! - old instance tokens rejected by a new server
//! - failed operations and imports preserving metadata
//! - cursor encoding and token round trips
//! - standalone compatibility and batch metadata scope
//! - generation exhaustion handling

#![cfg(feature = "test-support")]

use knotrel_core::EngineConfig;
use knotrel_server::{
    Lifecycle, ServerConfig, ServerMode, ShutdownOutcome, router_with_lifecycle,
    router_with_seeded_version, serve,
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

    async fn start_with_seeded_version(config: ServerConfig, version: &str) -> Self {
        let lifecycle = Lifecycle::new();
        let app = router_with_seeded_version(config.clone(), lifecycle.clone(), version);
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
        let name_lower = name.to_ascii_lowercase();
        for line in self.headers.lines() {
            if let Some((k, v)) = line.split_once(':')
                && k.trim().to_ascii_lowercase() == name_lower
            {
                return Some(v.trim());
            }
        }
        None
    }

    fn etag(&self) -> Option<&str> {
        self.header("etag")
    }

    fn generation(&self) -> Option<&str> {
        self.header("x-knotrel-generation")
    }

    fn state_version(&self) -> Option<&str> {
        self.header("x-knotrel-state-version")
    }

    fn source_position(&self) -> Option<&str> {
        self.header("x-knotrel-source-position")
    }

    fn sync_status(&self) -> Option<&str> {
        self.header("x-knotrel-sync-status")
    }
}

async fn read_reply(stream: &mut TcpStream) -> Option<Reply> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            if buffer.is_empty() {
                return None;
            }
            break buffer.len();
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let headers = String::from_utf8(buffer[..head_end].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-length:") {
                lower.split(':').nth(1)?.trim().parse::<usize>().ok()
            } else {
                None
            }
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

    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    Some(Reply {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    })
}

async fn get(stream: &mut TcpStream, path: &str) -> Reply {
    let request = format!("GET {path} HTTP/1.1\r\nhost: knotrel\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    read_reply(stream).await.unwrap()
}

async fn post(stream: &mut TcpStream, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut all_headers = vec![("content-type", "application/json")];
    all_headers.extend_from_slice(headers);
    let mut head = format!("POST {path} HTTP/1.1\r\nhost: knotrel\r\n");
    for (k, v) in &all_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    read_reply(stream).await.unwrap()
}

// ---------------------------------------------------------------------------
// 1. Equal numeric revisions in distinct instances and generations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_1_equal_numeric_revisions_in_distinct_instances_and_generations() {
    let mut server1 = Running::start(config_source_managed()).await;
    let inst1 = server1.instance_id().await;

    // Register session on server 1
    let mut s1_reg = server1.connect().await;
    let reg1 = post(
        &mut s1_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst1)],
        &format!(r#"{{"instance_id":"{inst1}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;
    assert_eq!(reg1.status, 200);

    // Import baseline generation 1
    let mut s1_imp1 = server1.connect().await;
    let imp1 = post(
        &mut s1_imp1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst1)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp1.status, 201);
    assert_eq!(imp1.generation(), Some("1"));
    assert_eq!(imp1.state_version(), Some("0"));
    let etag_gen1 = imp1.etag().unwrap().to_string();
    assert_eq!(etag_gen1, "\"1.0.pos-10\"");

    // Replace graph to generation 2
    let mut s1_imp2 = server1.connect().await;
    let imp2 = post(
        &mut s1_imp2,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst1),
            ("if-match", &etag_gen1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-20","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp2.status, 201);
    assert_eq!(imp2.generation(), Some("2"));
    assert_eq!(imp2.state_version(), Some("0"));
    let etag_gen2 = imp2.etag().unwrap().to_string();
    assert_eq!(etag_gen2, "\"2.0.pos-20\"");

    // Start server 2 (independent instance)
    let mut server2 = Running::start(config_source_managed()).await;
    let inst2 = server2.instance_id().await;
    assert_ne!(inst1, inst2);

    let mut s2_reg = server2.connect().await;
    let reg2 = post(
        &mut s2_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst2)],
        &format!(r#"{{"instance_id":"{inst2}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;
    assert_eq!(reg2.status, 200);

    // Import baseline generation 1 on server 2 with identical numeric revision and topology
    let mut s2_imp1 = server2.connect().await;
    let imp2_s2 = post(
        &mut s2_imp1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst2)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst2}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp2_s2.status, 201);
    assert_eq!(imp2_s2.generation(), Some("1"));
    assert_eq!(imp2_s2.state_version(), Some("0"));
    assert_eq!(imp2_s2.etag(), Some("\"1.0.pos-10\""));

    // Verify token isolation:
    // 1. Token from server 1 gen 1 ("1.0.pos-10") cannot be used on server 2 with server 1's instance_id (409 stale_instance_id)
    let mut s2_stale_inst = server2.connect().await;
    let stale_inst_res = post(
        &mut s2_stale_inst,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst1),
            ("if-match", &etag_gen1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-next","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(stale_inst_res.status, 409);
    assert_eq!(stale_inst_res.json()["code"], "stale_instance_id");

    // 2. Token from server 1 gen 1 ("1.0.pos-10") is now stale on server 1 (which is at generation 2) -> 412 precondition_failed
    let mut s1_stale_gen = server1.connect().await;
    let stale_gen_res = post(
        &mut s1_stale_gen,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst1),
            ("if-match", &etag_gen1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-30","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(stale_gen_res.status, 412);
    assert_eq!(stale_gen_res.json()["code"], "precondition_failed");

    server1.shutdown();
    server2.shutdown();
    assert_eq!(server1.finished().await, ShutdownOutcome::Drained);
    assert_eq!(server2.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 2. Independent servers constructed from cloned configuration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_2_cloned_configuration_yields_fresh_isolated_identities() {
    let config = config_source_managed();
    let mut server_a = Running::start(config.clone()).await;
    let mut server_b = Running::start(config).await;

    let id_a = server_a.instance_id().await;
    let id_b = server_b.instance_id().await;
    assert_ne!(id_a, id_b);
    assert!(id_a.starts_with("srv-"));
    assert!(id_b.starts_with("srv-"));

    // Server A rejects requests bearing Server B's instance ID
    let mut s_a = server_a.connect().await;
    let res_a = post(
        &mut s_a,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &id_b)],
        &format!(r#"{{"instance_id":"{id_b}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;
    assert_eq!(res_a.status, 409);
    assert_eq!(res_a.json()["code"], "stale_instance_id");

    // Server B rejects requests bearing Server A's instance ID
    let mut s_b = server_b.connect().await;
    let res_b = post(
        &mut s_b,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &id_a)],
        &format!(r#"{{"instance_id":"{id_a}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;
    assert_eq!(res_b.status, 409);
    assert_eq!(res_b.json()["code"], "stale_instance_id");

    server_a.shutdown();
    server_b.shutdown();
    assert_eq!(server_a.finished().await, ShutdownOutcome::Drained);
    assert_eq!(server_b.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 3. Revision carry boundaries and preservation of arbitrary precision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_3_revision_carry_boundaries_and_arbitrary_precision() {
    let mut server = Running::start(config_standalone()).await;

    // Initial state version is "0"
    let mut s_stats = server.connect().await;
    let stats0 = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats0.status, 200);
    assert_eq!(stats0.json()["state_version"], "0");
    assert_eq!(stats0.state_version(), Some("0"));
    assert_eq!(stats0.etag(), Some("\"0.0.\""));

    // Apply 9 effective mutations: 1..=9
    for i in 1..=9 {
        let mut s_mut = server.connect().await;
        let reply = post(
            &mut s_mut,
            "/v1/operations",
            &[],
            &format!(r#"{{"op":"add_node","node":"{i}"}}"#),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["changed"], true);
        assert_eq!(reply.state_version(), Some(&i.to_string()[..]));
        assert_eq!(reply.etag(), Some(&format!("\"0.{i}.\"")[..]));
    }

    // After 9 mutations, version is "9"
    let mut s_q9 = server.connect().await;
    let q9 = post(
        &mut s_q9,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"1"}"#,
    )
    .await;
    assert_eq!(q9.status, 200);
    assert_eq!(q9.json()["connected"], true);
    assert_eq!(q9.state_version(), Some("9"));
    assert_eq!(q9.etag(), Some("\"0.9.\""));

    // 10th mutation carries to two digits: "10"
    let mut s_mut10 = server.connect().await;
    let reply10 = post(
        &mut s_mut10,
        "/v1/operations",
        &[],
        r#"{"op":"add_node","node":"10"}"#,
    )
    .await;
    assert_eq!(reply10.status, 200);
    assert_eq!(reply10.json()["changed"], true);
    assert_eq!(reply10.state_version(), Some("10"));
    assert_eq!(reply10.etag(), Some("\"0.10.\""));

    // No-op operation (adding already-existing node) does NOT increment revision
    let mut s_noop = server.connect().await;
    let reply_noop = post(
        &mut s_noop,
        "/v1/operations",
        &[],
        r#"{"op":"add_node","node":"10"}"#,
    )
    .await;
    assert_eq!(reply_noop.status, 200);
    assert_eq!(reply_noop.json()["changed"], false);
    assert_eq!(reply_noop.state_version(), Some("10"));
    assert_eq!(reply_noop.etag(), Some("\"0.10.\""));

    // Rejected operation (self-loop) does NOT increment revision
    let mut s_err = server.connect().await;
    let reply_err = post(
        &mut s_err,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"1"}"#,
    )
    .await;
    assert_eq!(reply_err.status, 422);
    assert_eq!(reply_err.json()["code"], "self_loop");
    assert_eq!(reply_err.state_version(), Some("10"));
    assert_eq!(reply_err.etag(), Some("\"0.10.\""));

    // Stats confirms final version remains "10"
    let mut s_stats_final = server.connect().await;
    let stats_final = get(&mut s_stats_final, "/v1/stats").await;
    assert_eq!(stats_final.status, 200);
    assert_eq!(stats_final.json()["state_version"], "10");
    assert_eq!(stats_final.state_version(), Some("10"));
    assert_eq!(stats_final.etag(), Some("\"0.10.\""));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 4. Metadata coherence when queries race atomic replacement
// ---------------------------------------------------------------------------

#[cfg(feature = "test-support")]
#[tokio::test]
async fn test_4_metadata_coherence_when_queries_race_atomic_replacement() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    // Register session
    let mut s_reg = server.connect().await;
    post(
        &mut s_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    // Initial baseline import: nodes 1 & 2 connected, 3 & 4 disconnected
    let mut s_imp = server.connect().await;
    let imp_res = post(
        &mut s_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2","3","4"],"edges":[{{"source":"1","target":"2"}}]}}"#),
    )
    .await;
    assert_eq!(imp_res.status, 201);
    assert_eq!(imp_res.etag(), Some("\"1.0.pos-1\""));

    // Pause replacement worker off-lock during candidate build
    let mut pause = server.lifecycle.pause_next_worker();
    let mut stream_replace = server.connect().await;

    let replace_body = format!(
        r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2","3","4"],"edges":[{{"source":"3","target":"4"}}]}}"#
    );
    let req = format!(
        "POST /v1/feeder/import HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\nx-knotrel-instance-id: {inst}\r\nif-match: \"1.0.pos-1\"\r\ncontent-length: {}\r\n\r\n{replace_body}",
        replace_body.len()
    );
    stream_replace.write_all(req.as_bytes()).await.unwrap();
    pause.wait_started().await;

    // While candidate construction is paused, query must observe coherent Generation 1 metadata and topology!
    let mut s_q1 = server.connect().await;
    let q1 = post(
        &mut s_q1,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"3","target":"4"}"#,
    )
    .await;
    assert_eq!(q1.status, 200);
    assert_eq!(q1.json()["connected"], false);
    assert_eq!(q1.generation(), Some("1"));
    assert_eq!(q1.state_version(), Some("0"));
    assert_eq!(q1.sync_status(), Some("catching_up"));
    assert_eq!(q1.etag(), Some("\"1.0.pos-1\""));

    // Release worker and let Generation 2 publish
    pause.release();
    let replace_reply = read_reply(&mut stream_replace).await.unwrap();
    assert_eq!(replace_reply.status, 201);
    assert_eq!(replace_reply.generation(), Some("2"));
    assert_eq!(replace_reply.etag(), Some("\"2.0.pos-2\""));

    // Post-publication query must observe coherent Generation 2 metadata and topology!
    let mut s_q2 = server.connect().await;
    let q2 = post(
        &mut s_q2,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"3","target":"4"}"#,
    )
    .await;
    assert_eq!(q2.status, 200);
    assert_eq!(q2.json()["connected"], true);
    assert_eq!(q2.generation(), Some("2"));
    assert_eq!(q2.state_version(), Some("0"));
    assert_eq!(q2.sync_status(), Some("catching_up"));
    assert_eq!(q2.etag(), Some("\"2.0.pos-2\""));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 5. Stale replacement tokens after publication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_5_stale_replacement_tokens_after_publication() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    let mut s_reg = server.connect().await;
    post(
        &mut s_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let mut s_imp1 = server.connect().await;
    let imp1 = post(
        &mut s_imp1,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-10","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp1.status, 201);
    let token_gen1 = imp1.etag().unwrap().to_string();

    // Replace to gen 2
    let mut s_imp2 = server.connect().await;
    let imp2 = post(
        &mut s_imp2,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", &token_gen1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-20","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp2.status, 201);
    assert_eq!(imp2.generation(), Some("2"));

    // Attempt replacement using stale token_gen1 ("1.0.pos-10")
    let mut s_imp3 = server.connect().await;
    let imp3 = post(
        &mut s_imp3,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", &token_gen1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-30","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp3.status, 412);
    assert_eq!(imp3.json()["code"], "precondition_failed");

    // Stats confirms server remained at generation 2
    let mut s_stats = server.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["generation"], 2);
    assert_eq!(stats.json()["source_position"], "pos-20");

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 6. Old instance tokens rejected by a new server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_6_old_instance_tokens_rejected_by_new_server() {
    let mut server1 = Running::start(config_source_managed()).await;
    let inst1 = server1.instance_id().await;

    let mut s1_reg = server1.connect().await;
    post(
        &mut s1_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst1)],
        &format!(r#"{{"instance_id":"{inst1}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let mut s1_imp = server1.connect().await;
    let imp1 = post(
        &mut s1_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst1)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp1.status, 201);
    let token1 = imp1.etag().unwrap().to_string();

    server1.shutdown();
    assert_eq!(server1.finished().await, ShutdownOutcome::Drained);

    // Start a new server instance (restart)
    let mut server2 = Running::start(config_source_managed()).await;
    let inst2 = server2.instance_id().await;
    assert_ne!(inst1, inst2);

    // Feeder sending request to server2 with old instance_id is immediately rejected
    let mut s2 = server2.connect().await;
    let rej1 = post(
        &mut s2,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst1),
            ("if-match", &token1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst1}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(rej1.status, 409);
    assert_eq!(rej1.json()["code"], "stale_instance_id");

    // Even if feeder uses new instance_id, token1 fails precondition because server2 is uninitialized (generation 0)
    let mut s2_reg = server2.connect().await;
    post(
        &mut s2_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst2)],
        &format!(r#"{{"instance_id":"{inst2}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let mut s2_rep = server2.connect().await;
    let rej2 = post(
        &mut s2_rep,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst2),
            ("if-match", &token1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst2}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(rej2.status, 412);
    assert_eq!(rej2.json()["code"], "precondition_failed");

    server2.shutdown();
    assert_eq!(server2.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 7. Failed operations and imports preserve metadata
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_7_failed_operations_and_imports_preserve_metadata() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    let mut s_reg = server.connect().await;
    post(
        &mut s_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let mut s_imp = server.connect().await;
    let imp = post(
        &mut s_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1","2"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp.status, 201);
    let original_etag = imp.etag().unwrap().to_string();

    // 1. Invalid graph ID returns 400
    let mut s_bad_graph = server.connect().await;
    let r_bad_graph = post(
        &mut s_bad_graph,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"invalid_name","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(r_bad_graph.status, 400);

    // 2. Self loop returns 422
    let mut s_self_loop = server.connect().await;
    let r_self_loop = post(
        &mut s_self_loop,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", &original_etag),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[{{"source":"1","target":"1"}}]}}"#),
    )
    .await;
    assert_eq!(r_self_loop.status, 422);

    // 3. Precondition mismatch returns 412
    let mut s_precond = server.connect().await;
    let r_precond = post(
        &mut s_precond,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", "\"1.99.pos-1\""),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(r_precond.status, 412);

    // Confirm /v1/stats shows completely unchanged metadata and topology
    let mut s_stats = server.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["generation"], 1);
    assert_eq!(stats.json()["state_version"], "0");
    assert_eq!(stats.json()["source_position"], "pos-1");
    assert_eq!(stats.etag(), Some(&original_etag[..]));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 8. Cursor encoding and token round trips
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_8_cursor_encoding_and_token_round_trips() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    let mut s_reg = server.connect().await;
    post(
        &mut s_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let complex_pos = "epoch=1,part=0;offset=1024/stream+v1.2:test%40";

    let mut s_imp = server.connect().await;
    let imp = post(
        &mut s_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"{complex_pos}","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp.status, 201);
    assert_eq!(
        imp.source_position(),
        Some(&knotrel_server::percent_encode_position(complex_pos)[..])
    );
    assert_eq!(imp.sync_status(), Some("catching_up"));

    // ETag safely encodes complex characters (no unescaped dots, commas, slashes)
    let etag1 = imp.etag().expect("etag header on 201 created");
    assert!(etag1.starts_with('"') && etag1.ends_with('"'));
    assert!(etag1.contains("%2F")); // slash is encoded
    assert!(etag1.contains("%2E")); // dot is encoded
    assert!(etag1.contains("%2C")); // comma is encoded

    // Directly round-trip the exact ETag as If-Match for replacement
    let next_complex_pos = "epoch=1,part=0;offset=2048/stream+v1.2:test%40";
    let mut s_rep = server.connect().await;
    let rep = post(
        &mut s_rep,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", etag1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"{next_complex_pos}","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["10","20"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(rep.status, 201);
    assert_eq!(rep.generation(), Some("2"));
    assert_eq!(
        rep.source_position(),
        Some(&knotrel_server::percent_encode_position(next_complex_pos)[..])
    );
    assert_eq!(rep.sync_status(), Some("catching_up"));

    // Verify raw cursor in stats JSON body while headers have percent-encoded position
    let mut s_stats = server.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.json()["source_position"], next_complex_pos);
    assert_eq!(stats.generation(), Some("2"));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 9. Standalone compatibility and batch metadata scope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_9_standalone_compatibility_and_batch_metadata_scope() {
    let mut server = Running::start(config_standalone()).await;

    // Verify /v1/info has no mode or instance_id in standalone mode
    let mut s_info = server.connect().await;
    let info = get(&mut s_info, "/v1/info").await;
    assert_eq!(info.status, 200);
    assert!(info.json().get("mode").is_none());
    assert!(info.json().get("instance_id").is_none());

    // Verify /v1/stats JSON body has no generation, source_position, or sync_status
    let mut s_stats = server.connect().await;
    let stats = get(&mut s_stats, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert!(stats.json().get("generation").is_none());
    assert!(stats.json().get("source_position").is_none());
    assert!(stats.json().get("sync_status").is_none());
    assert_eq!(stats.json()["state_version"], "0");

    // Execute batch: 2 effective links, 1 duplicate no-op link
    let mut s_batch = server.connect().await;
    let batch_res = post(
        &mut s_batch,
        "/v1/batch",
        &[],
        r#"{"operations":[
            {"op":"link","source":"1","target":"2"},
            {"op":"link","source":"2","target":"3"},
            {"op":"link","source":"1","target":"2"}
        ]}"#,
    )
    .await;
    assert_eq!(batch_res.status, 200);
    let results = batch_res.json()["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["changed"], true);
    assert_eq!(results[1]["changed"], true);
    assert_eq!(results[2]["changed"], false);

    // Batch response headers reflect the batch's FINAL coherent state: version "2"
    assert_eq!(batch_res.state_version(), Some("2"));
    assert_eq!(batch_res.etag(), Some("\"0.2.\""));

    // Execute batch with continue-on-error domain error:
    let mut s_batch2 = server.connect().await;
    let batch_res2 = post(
        &mut s_batch2,
        "/v1/batch",
        &[],
        r#"{"operations":[
            {"op":"link","source":"3","target":"4"},
            {"op":"link","source":"5","target":"5"},
            {"op":"link","source":"4","target":"5"}
        ]}"#,
    )
    .await;
    assert_eq!(batch_res2.status, 200);
    let results2 = batch_res2.json()["results"].as_array().unwrap().clone();
    assert_eq!(results2.len(), 3);
    assert_eq!(results2[0]["changed"], true);
    assert_eq!(results2[1]["code"], "self_loop");
    assert_eq!(results2[2]["changed"], true);

    // Final revision advanced by 2 effective operations: "2" + 2 = "4"
    assert_eq!(batch_res2.state_version(), Some("4"));
    assert_eq!(batch_res2.etag(), Some("\"0.4.\""));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

// ---------------------------------------------------------------------------
// 10. Generation bounds and overflow precondition rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_10_generation_bounds_and_overflow_precondition_rejection() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    let mut s_reg = server.connect().await;
    post(
        &mut s_reg,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-1","feeder_epoch":1,"lease_duration_ms":10000}}"#),
    )
    .await;

    let mut s_imp = server.connect().await;
    let imp = post(
        &mut s_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(imp.status, 201);

    // u64::MAX generation in If-Match is parsed, but rejected as precondition_failed because server is at generation 1
    let mut s_rep1 = server.connect().await;
    let rep1 = post(
        &mut s_rep1,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", "\"18446744073709551615.0.pos-1\""),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(rep1.status, 412);
    assert_eq!(rep1.json()["code"], "precondition_failed");

    // Overflowing u64 generation in If-Match (> u64::MAX) is rejected with 412 precondition_failed
    let mut s_rep2 = server.connect().await;
    let rep2 = post(
        &mut s_rep2,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", "\"18446744073709551616.0.pos-1\""),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-1","feeder_epoch":1,"nodes":["1"],"edges":[]}}"#),
    )
    .await;
    assert_eq!(rep2.status, 412);
    assert_eq!(rep2.json()["code"], "precondition_failed");

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_11_response_coherence_across_replacement_with_paused_delivery() {
    let mut server = Running::start(config_source_managed()).await;
    let inst = server.instance_id().await;

    // 1. Register session
    let mut s_sess = server.connect().await;
    let reply_sess = post(
        &mut s_sess,
        "/v1/feeder/session",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"instance_id":"{inst}","feeder_session_id":"f-coherence","feeder_epoch":1,"lease_duration_ms":30000}}"#),
    )
    .await;
    assert_eq!(reply_sess.status, 200);

    // 2. Initial baseline import (Generation 1): edge 1 - 2
    let mut s_imp = server.connect().await;
    let imp_res = post(
        &mut s_imp,
        "/v1/feeder/import",
        &[("x-knotrel-instance-id", &inst)],
        &format!(r#"{{"mode":"create_only","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-1","feeder_session_id":"f-coherence","feeder_epoch":1,"nodes":["1","2"],"edges":[{{"source":"1","target":"2"}}]}}"#),
    )
    .await;
    assert_eq!(imp_res.status, 201);
    assert_eq!(imp_res.generation(), Some("1"));
    assert_eq!(imp_res.source_position(), Some("pos-1"));
    let etag_g1 = imp_res.etag().expect("etag for g1").to_string();
    assert_eq!(etag_g1, "\"1.0.pos-1\"");

    // 3. Install post-lock pause point: worker pauses AFTER releasing the graph lock
    let mut pause = server.lifecycle.pause_next_worker_after_lock();

    // 4. Send query on stream A: connected(1, 2)
    let mut stream_a = server.connect().await;
    let query_task = tokio::spawn(async move {
        post(
            &mut stream_a,
            "/v1/operations",
            &[],
            r#"{"op":"connected","source":"1","target":"2"}"#,
        )
        .await
    });

    // 5. Wait until the query worker has evaluated the query, captured metadata,
    //    and RELEASED the graph lock.
    pause.wait_started().await;

    // 6. While query response delivery is paused and graph lock is released,
    //    publish a replacement (Generation 2) on stream B that cuts edge 1 - 2 and adds edge 3 - 4.
    let mut stream_b = server.connect().await;
    let replace_res = post(
        &mut stream_b,
        "/v1/feeder/import",
        &[
            ("x-knotrel-instance-id", &inst),
            ("if-match", &etag_g1),
        ],
        &format!(r#"{{"mode":"replace","instance_id":"{inst}","graph_id":"default","source_id":"pg-1","source_epoch":"ep-1","source_position":"pos-2","feeder_session_id":"f-coherence","feeder_epoch":1,"nodes":["1","2","3","4"],"edges":[{{"source":"3","target":"4"}}]}}"#),
    )
    .await;
    assert_eq!(replace_res.status, 201);
    assert_eq!(replace_res.generation(), Some("2"));
    assert_eq!(replace_res.source_position(), Some("pos-2"));
    let etag_g2 = replace_res.etag().expect("etag for g2").to_string();
    assert_eq!(etag_g2, "\"2.0.pos-2\"");

    // 7. Resume the query response delivery
    pause.release();

    // 8. The original response on stream A MUST retain its original topology result,
    //    generation, revision, cursor, and replacement token — NOT generation 2!
    let reply_a = query_task.await.unwrap();
    assert_eq!(reply_a.status, 200);
    assert_eq!(reply_a.json()["connected"], true);
    assert_eq!(reply_a.generation(), Some("1"));
    assert_eq!(reply_a.state_version(), Some("0"));
    assert_eq!(reply_a.source_position(), Some("pos-1"));
    assert_eq!(reply_a.etag(), Some("\"1.0.pos-1\""));

    // 9. Subsequent query on stream B confirms the live graph is indeed at Generation 2
    let reply_post = post(
        &mut stream_b,
        "/v1/operations",
        &[],
        r#"{"op":"connected","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(reply_post.status, 200);
    assert_eq!(reply_post.json()["connected"], false);
    assert_eq!(reply_post.generation(), Some("2"));
    assert_eq!(reply_post.source_position(), Some("pos-2"));
    assert_eq!(reply_post.etag(), Some("\"2.0.pos-2\""));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn test_12_arbitrary_precision_revision_beyond_u64_max_over_http() {
    // Start standalone server with revision seeded beyond u64::MAX ending in 599
    let seed_version = "1844674407370955161599";
    let mut server = Running::start_with_seeded_version(config_standalone(), seed_version).await;

    let mut stream = server.connect().await;

    // Initial stats check verifies exact decimal representation
    let stats = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert_eq!(stats.json()["state_version"], seed_version);
    assert_eq!(stats.state_version(), Some(seed_version));
    assert_eq!(stats.etag(), Some("\"0.1844674407370955161599.\""));

    // 1. Effective mutation triggers multi-digit carry (599 -> 600)
    let res_link = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(res_link.status, 200);
    assert_eq!(res_link.json()["changed"], true);
    assert_eq!(res_link.state_version(), Some("1844674407370955161600"));
    assert_eq!(res_link.etag(), Some("\"0.1844674407370955161600.\""));

    // 2. Duplicate link is a no-op; revision must remain unchanged
    let res_noop = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"2"}"#,
    )
    .await;
    assert_eq!(res_noop.status, 200);
    assert_eq!(res_noop.json()["changed"], false);
    assert_eq!(res_noop.state_version(), Some("1844674407370955161600"));
    assert_eq!(res_noop.etag(), Some("\"0.1844674407370955161600.\""));

    // 3. Rejected mutation (self-loop returns 422); revision must remain unchanged
    let res_reject = post(
        &mut stream,
        "/v1/operations",
        &[],
        r#"{"op":"link","source":"1","target":"1"}"#,
    )
    .await;
    assert_eq!(res_reject.status, 422);
    assert_eq!(res_reject.json()["code"], "self_loop");

    // Subsequent query confirms revision was preserved at 1844674407370955161600
    let stats_after = get(&mut stream, "/v1/stats").await;
    assert_eq!(stats_after.status, 200);
    assert_eq!(
        stats_after.json()["state_version"],
        "1844674407370955161600"
    );
    assert_eq!(stats_after.state_version(), Some("1844674407370955161600"));
    assert_eq!(stats_after.etag(), Some("\"0.1844674407370955161600.\""));

    server.shutdown();
    assert_eq!(server.finished().await, ShutdownOutcome::Drained);
}
