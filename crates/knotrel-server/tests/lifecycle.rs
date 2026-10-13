//! Lifecycle tests: readiness, connection bounds, timeouts and graceful shutdown.
//!
//! These tests bind real loopback sockets, so they need an environment that
//! permits local networking.

use knotrel_core::EngineConfig;
use knotrel_server::{
    Lifecycle, Phase, ServerConfig, ShutdownOutcome, router_with_lifecycle, serve,
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

fn config() -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 32).unwrap()
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

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    async fn finished(self) -> ShutdownOutcome {
        timeout(STEP, self.task).await.unwrap().unwrap()
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn closes(&self) -> bool {
        self.headers
            .to_ascii_lowercase()
            .contains("connection: close")
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

async fn probe(running: &Running, path: &str) -> Reply {
    get(&mut running.connect().await, path).await
}

const BODY: &str = r#"{"op":"link","source":"1","target":"2"}"#;

/// Sends complete headers and all but the final byte of a mutation body.
async fn start_unfinished_request(running: &Running) -> TcpStream {
    let mut stream = running.connect().await;
    let head = format!(
        "POST /v1/operations HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        BODY.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream
        .write_all(&BODY.as_bytes()[..BODY.len() - 1])
        .await
        .unwrap();
    stream.flush().await.unwrap();
    stream
}

async fn finish_request(stream: &mut TcpStream) -> Reply {
    stream
        .write_all(&BODY.as_bytes()[BODY.len() - 1..])
        .await
        .unwrap();
    read_reply(stream).await.expect("a response")
}

async fn wait_for_phase(lifecycle: &Lifecycle, phase: Phase) {
    timeout(STEP, async {
        while lifecycle.phase() != phase {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn expect_eof(stream: &mut TcpStream, within: Duration) {
    let mut byte = [0_u8; 1];
    let read = timeout(within, stream.read(&mut byte))
        .await
        .expect("connection should have been closed")
        .unwrap_or(0);
    assert_eq!(read, 0, "unexpected data before close");
}

#[tokio::test]
async fn readiness_is_starting_before_serve_and_ready_after() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let lifecycle = Lifecycle::new();
    let app = router_with_lifecycle(config(), lifecycle.clone());
    let request = || {
        Request::builder()
            .uri("/ready")
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(&body[..], br#"{"status":"starting"}"#);
    // Liveness does not depend on the phase.
    let health = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), 200);

    let running = Running::start(config()).await;
    wait_for_phase(&running.lifecycle, Phase::Ready).await;
    let reply = probe(&running, "/ready").await;
    assert_eq!(
        (reply.status, reply.body.as_str()),
        (200, r#"{"status":"ready"}"#)
    );
    assert!(
        reply
            .headers
            .to_ascii_lowercase()
            .contains("cache-control: no-store")
    );
}

#[tokio::test]
async fn shutdown_without_connections_is_prompt() {
    let mut running = Running::start(config()).await;
    wait_for_phase(&running.lifecycle, Phase::Ready).await;
    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn draining_finishes_admitted_work_and_rejects_new_work() {
    let mut running = Running::start(config()).await;
    let mut admitted = start_unfinished_request(&running).await;
    // Ensure the request was admitted before shutdown begins.
    assert_eq!(probe(&running, "/ready").await.status, 200);
    running.shutdown();
    wait_for_phase(&running.lifecycle, Phase::Draining).await;

    let ready = probe(&running, "/ready").await;
    assert_eq!(
        (ready.status, ready.body.as_str()),
        (503, r#"{"status":"draining"}"#)
    );
    assert!(ready.closes());
    let health = probe(&running, "/health").await;
    assert_eq!(health.status, 200);
    assert!(health.closes());
    for path in ["/v1/info", "/v1/stats"] {
        let rejected = probe(&running, path).await;
        assert_eq!(rejected.status, 503, "{path}");
        assert!(rejected.closes(), "{path}");
        assert_eq!(
            rejected.body,
            r#"{"result":"error","code":"shutting_down","message":"server is shutting down; retry against another instance"}"#
        );
    }

    let finished = finish_request(&mut admitted).await;
    assert_eq!(finished.status, 200);
    assert_eq!(finished.body, r#"{"result":"changed","changed":true}"#);
    assert!(finished.closes());
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn idle_keep_alive_connections_are_closed_when_draining_starts() {
    let mut running = Running::start(config()).await;
    let mut idle = running.connect().await;
    assert_eq!(get(&mut idle, "/health").await.status, 200);
    running.shutdown();
    expect_eof(&mut idle, STEP).await;
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn drain_deadline_aborts_stuck_connections() {
    let config = config()
        .with_shutdown_timeout(Duration::from_millis(200))
        .unwrap();
    let mut running = Running::start(config).await;
    let mut stuck = start_unfinished_request(&running).await;
    assert_eq!(probe(&running, "/ready").await.status, 200);
    running.shutdown();
    let outcome = running.finished().await;
    assert_eq!(outcome, ShutdownOutcome::DeadlineExceeded { abandoned: 1 });
    expect_eof(&mut stuck, STEP).await;
}

#[tokio::test]
async fn connection_limit_rejects_extra_connections_and_recovers() {
    let config = config().with_max_connections(2).unwrap();
    let mut running = Running::start(config).await;
    let mut first = running.connect().await;
    let mut second = running.connect().await;
    assert_eq!(get(&mut first, "/health").await.status, 200);
    assert_eq!(get(&mut second, "/health").await.status, 200);

    let mut third = running.connect().await;
    let rejected = read_reply(&mut third).await.expect("a rejection");
    assert_eq!(rejected.status, 503);
    assert!(rejected.closes());
    assert_eq!(
        rejected.body,
        r#"{"result":"error","code":"too_many_connections","message":"connection limit reached"}"#
    );
    // Admitted connections are unaffected.
    assert_eq!(get(&mut first, "/health").await.status, 200);

    drop(first);
    timeout(STEP, async {
        loop {
            let mut retry = running.connect().await;
            let request = "GET /health HTTP/1.1\r\nhost: knotrel\r\n\r\n";
            retry.write_all(request.as_bytes()).await.unwrap();
            if read_reply(&mut retry)
                .await
                .is_some_and(|reply| reply.status == 200)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    running.shutdown();
    drop(second);
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn silent_and_partial_header_connections_time_out() {
    let config = config()
        .with_header_read_timeout(Duration::from_millis(150))
        .unwrap();
    let mut running = Running::start(config).await;
    let mut silent = running.connect().await;
    expect_eof(&mut silent, STEP).await;
    let mut partial = running.connect().await;
    partial
        .write_all(b"GET /health HTTP/1.1\r\nhost: kn")
        .await
        .unwrap();
    expect_eof_or_error_reply(&mut partial).await;
    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

/// hyper may answer a timed-out header read with 408 before closing.
async fn expect_eof_or_error_reply(stream: &mut TcpStream) {
    let mut sink = Vec::new();
    timeout(STEP, stream.read_to_end(&mut sink))
        .await
        .expect("connection should have been closed")
        .ok();
    assert!(
        !String::from_utf8_lossy(&sink).contains(" 200 "),
        "timed-out request must not succeed"
    );
}

#[tokio::test]
async fn idle_keep_alive_connection_times_out() {
    let config = config()
        .with_header_read_timeout(Duration::from_millis(150))
        .unwrap();
    let mut running = Running::start(config).await;
    let mut stream = running.connect().await;
    assert_eq!(get(&mut stream, "/health").await.status, 200);
    expect_eof(&mut stream, STEP).await;
    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}

#[tokio::test]
async fn request_timeout_bounds_a_stalled_body() {
    let config = config()
        .with_request_timeout(Duration::from_millis(200))
        .unwrap();
    let mut running = Running::start(config).await;
    let mut stalled = start_unfinished_request(&running).await;
    let reply = read_reply(&mut stalled).await.expect("a timeout response");
    assert_eq!(reply.status, 503);
    assert!(reply.closes());
    assert_eq!(
        reply.body,
        r#"{"result":"error","code":"request_timeout","message":"request exceeded the server time limit; its effect is unknown"}"#
    );
    // The graph was not mutated by the incomplete request.
    let stats = probe(&running, "/v1/stats").await;
    assert_eq!(stats.status, 200);
    assert!(stats.body.contains(r#""node_count":0"#), "{}", stats.body);
    running.shutdown();
    assert_eq!(running.finished().await, ShutdownOutcome::Drained);
}
