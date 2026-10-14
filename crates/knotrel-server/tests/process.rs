//! Process-level lifecycle smoke tests: start, probe, mutate, signal, restart.
//!
//! These spawn the real binary and bind loopback sockets, so they need an
//! environment that permits local networking.
//!
//! # Parallelism note
//!
//! [`Server::start`] serialises the port-allocation → server-ready window with
//! [`START_LOCK`]. This prevents a potential TOCTOU race: `free_address()` drops its
//! listener before the server process has had a chance to bind the same port,
//! so two tests running in parallel could receive the same OS-assigned port and
//! interfere with each other's sockets. The lock ensures only
//! one test occupies that critical window at a time; once the server is
//! confirmed ready the lock is released and the remaining test body runs freely.
#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, OnceLock},
    thread::sleep,
    time::{Duration, Instant},
};

/// Serialises the `free_address() → server ready` window across all tests.
///
/// Note: While ephemeral port collisions under concurrent execution and socket
/// closures during shutdown transitions are plausible contributing factors, the
/// historical cause of intermittent `ConnectionReset` panics in earlier test runs
/// remains formally unproven and not conclusively confirmed.
static START_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn acquire_start_lock() -> std::sync::MutexGuard<'static, ()> {
    START_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

const STEP: Duration = Duration::from_secs(10);

fn free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

struct Server {
    child: Child,
    address: SocketAddr,
}

impl Server {
    fn spawn(address: SocketAddr, extra: &[(&str, &str)]) -> Child {
        let mut command = Command::new(env!("CARGO_BIN_EXE_knotrel-server"));
        command
            .env_clear()
            .env("KNOTREL_ADDR", address.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in extra {
            command.env(key, value);
        }
        command.spawn().unwrap()
    }

    fn start(extra: &[(&str, &str)]) -> Self {
        // Hold the start lock for the entire port-allocation → server-ready
        // critical section; see module doc for the rationale.
        let _guard = acquire_start_lock();
        let address = free_address();
        let mut server = Self {
            child: Self::spawn(address, extra),
            address,
        };
        server.wait_ready();
        server
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + STEP;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("server exited before becoming ready: {status}");
            }
            match TcpStream::connect(self.address) {
                Ok(mut stream) => match try_request(&mut stream, "GET", "/ready", "") {
                    Ok((200, _)) => return,
                    Ok((503, _)) => {
                        // Server is still starting up (Phase::Starting); retry.
                    }
                    Ok((status, body)) => {
                        panic!("unexpected HTTP status {status} during wait_ready: {body}");
                    }
                    Err(err) => match err.kind() {
                        std::io::ErrorKind::InvalidData => {
                            panic!("malformed HTTP response during wait_ready: {err}");
                        }
                        std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionAborted => {
                            // Admitted transient socket closure during startup handoff; retry.
                        }
                        _ => panic!("unexpected IO error during wait_ready: {err}"),
                    },
                },
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::ConnectionRefused {
                        panic!("unexpected connect error during wait_ready: {err}");
                    }
                }
            }
            assert!(Instant::now() < deadline, "server never became ready");
            sleep(Duration::from_millis(20));
        }
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn wait_exit(&mut self, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "server did not exit in time");
            sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sends one request on `stream` and returns the status and body.
///
/// Panics on any IO error. Use this for requests that must succeed; use
/// [`try_request`] in retry loops where transient IO errors are expected.
fn request(stream: &mut TcpStream, method: &str, path: &str, body: &str) -> (u16, String) {
    let message = format!(
        "{method} {path} HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(message.as_bytes()).unwrap();
    read_response(stream)
}

/// Like [`request`] but returns `Err` on any IO error instead of panicking.
///
/// Used in startup retry loops where transient connection errors (ECONNREFUSED,
/// ECONNRESET, EOF before a full response) mean "server not yet ready."
fn try_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    body: &str,
) -> std::io::Result<(u16, String)> {
    let message = format!(
        "{method} {path} HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(message.as_bytes())?;
    try_read_response(stream)
}

fn read_response(stream: &mut TcpStream) -> (u16, String) {
    try_read_response(stream).expect("IO error reading HTTP response")
}

/// Reads one HTTP/1.1 response from `stream`, returning an error for any IO
/// failure. This lets retry callers distinguish a transient connection reset
/// from a successfully-received non-200 status.
fn try_read_response(stream: &mut TcpStream) -> std::io::Result<(u16, String)> {
    stream.set_read_timeout(Some(STEP))?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before a full response",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8(buffer[..head_end].to_vec())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed inside a response body",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing HTTP status")
        })?;
    Ok((
        status,
        String::from_utf8(buffer[head_end..head_end + length].to_vec())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    ))
}

fn call(server: &Server, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(server.address).unwrap();
    request(&mut stream, method, path, body)
}

#[test]
fn startup_probe_mutation_query_and_sigterm_shutdown() {
    let mut server = Server::start(&[]);
    assert_eq!(
        call(&server, "GET", "/health", ""),
        (200, r#"{"status":"ok"}"#.into())
    );
    assert_eq!(
        call(&server, "GET", "/ready", ""),
        (200, r#"{"status":"ready"}"#.into())
    );
    let link = r#"{"op":"link","source":"1","target":"2"}"#;
    assert_eq!(
        call(&server, "POST", "/v1/operations", link),
        (200, r#"{"result":"changed","changed":true}"#.into())
    );
    let query = r#"{"op":"connected","source":"1","target":"2"}"#;
    assert_eq!(
        call(&server, "POST", "/v1/operations", query),
        (200, r#"{"result":"connected","connected":true}"#.into())
    );
    server.signal("TERM");
    let status = server.wait_exit(STEP);
    assert_eq!(status.code(), Some(0), "{status}");
}

#[test]
fn sigint_is_a_graceful_shutdown() {
    let mut server = Server::start(&[]);
    server.signal("INT");
    assert_eq!(server.wait_exit(STEP).code(), Some(0));
}

#[test]
fn restart_starts_with_an_empty_graph() {
    let _guard = acquire_start_lock();
    let address = free_address();
    let mut first = Server {
        child: Server::spawn(address, &[]),
        address,
    };
    first.wait_ready();
    let link = r#"{"op":"link","source":"1","target":"2"}"#;
    assert_eq!(call(&first, "POST", "/v1/operations", link).0, 200);
    first.signal("TERM");
    assert_eq!(first.wait_exit(STEP).code(), Some(0));

    let mut second = Server {
        child: Server::spawn(address, &[]),
        address,
    };
    second.wait_ready();
    let (status, stats) = call(&second, "GET", "/v1/stats", "");
    assert_eq!(status, 200);
    assert!(stats.contains(r#""node_count":0"#), "{stats}");
    assert!(stats.contains(r#""edge_count":0"#), "{stats}");
}

#[test]
fn sigterm_drains_an_admitted_request_before_exiting() {
    let mut server = Server::start(&[]);
    let body = r#"{"op":"link","source":"1","target":"2"}"#;
    let mut in_flight = TcpStream::connect(server.address).unwrap();
    let head = format!(
        "POST /v1/operations HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    in_flight.write_all(head.as_bytes()).unwrap();
    in_flight
        .write_all(&body.as_bytes()[..body.len() - 1])
        .unwrap();
    // Note on black-box admission boundary:
    // In this real-process smoke test, internal middleware hooks are not accessible.
    // The test uses loopback TCP connection establishment and a completed probe on a
    // separate connection as an externally observable proxy that the in-flight
    // request connection was established before shutdown. The exact internal admission
    // boundary through `shutdown_gate` is deterministically verified with explicit
    // admission acknowledgements in `lifecycle.rs`.
    assert_eq!(call(&server, "GET", "/ready", "").0, 200);

    server.signal("TERM");
    let deadline = Instant::now() + STEP;
    loop {
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "server exited early"
        );
        match TcpStream::connect(server.address) {
            Ok(mut probe) => match try_request(&mut probe, "GET", "/ready", "") {
                Ok((503, body)) => {
                    assert_eq!(body, r#"{"status":"draining"}"#);
                    break;
                }
                Ok((200, _)) => {
                    // Still in Phase::Ready; signal is not yet processed.
                }
                Ok((status, body)) => {
                    panic!("unexpected status {status} during drain probe: {body}");
                }
                Err(err) => match err.kind() {
                    std::io::ErrorKind::InvalidData => {
                        panic!("malformed HTTP response during drain probe: {err}");
                    }
                    std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe => {
                        // An idle socket closure or connection reset at the drain transition
                        // boundary is admitted by graceful shutdown semantics.
                    }
                    _ => panic!("unexpected IO error during drain probe: {err}"),
                },
            },
            Err(err) => {
                if err.kind() != std::io::ErrorKind::ConnectionRefused {
                    panic!("unexpected connect error during drain probe: {err}");
                }
            }
        }
        assert!(Instant::now() < deadline, "server never reported draining");
        sleep(Duration::from_millis(10));
    }
    assert!(server.child.try_wait().unwrap().is_none(), "exited early");

    in_flight
        .write_all(&body.as_bytes()[body.len() - 1..])
        .unwrap();
    let (status, response) = read_response(&mut in_flight);
    assert_eq!(
        (status, response.as_str()),
        (200, r#"{"result":"changed","changed":true}"#)
    );
    assert_eq!(server.wait_exit(STEP).code(), Some(0));
}

#[test]
fn drain_deadline_exit_status_is_distinct() {
    let mut server = Server::start(&[("KNOTREL_SHUTDOWN_TIMEOUT_MS", "300")]);
    let mut stuck = TcpStream::connect(server.address).unwrap();
    stuck
        .write_all(b"POST /v1/operations HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: 40\r\n\r\n{")
        .unwrap();
    assert_eq!(call(&server, "GET", "/ready", "").0, 200);
    let started = Instant::now();
    server.signal("TERM");
    let status = server.wait_exit(STEP);
    assert_eq!(status.code(), Some(2), "{status}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn failed_bind_exits_with_an_error() {
    let _guard = acquire_start_lock();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = occupied.local_addr().unwrap();
    let mut child = Server::spawn(address, &[]);
    let deadline = Instant::now() + STEP;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("server kept running although the address was taken");
        }
        sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1), "{status}");
}

#[test]
fn second_signal_during_drain_forces_immediate_exit() {
    let mut server = Server::start(&[("KNOTREL_SHUTDOWN_TIMEOUT_MS", "5000")]);
    let mut stuck = TcpStream::connect(server.address).unwrap();
    stuck
        .write_all(b"POST /v1/operations HTTP/1.1\r\nhost: knotrel\r\ncontent-type: application/json\r\ncontent-length: 40\r\n\r\n{")
        .unwrap();
    // External admission boundary proxy (see note in sigterm_drains_an_admitted_request_before_exiting).
    assert_eq!(call(&server, "GET", "/ready", "").0, 200);

    server.signal("TERM");
    let deadline = Instant::now() + STEP;
    loop {
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "server exited prematurely"
        );
        match TcpStream::connect(server.address) {
            Ok(mut probe) => match try_request(&mut probe, "GET", "/ready", "") {
                Ok((503, body)) => {
                    assert_eq!(body, r#"{"status":"draining"}"#);
                    break;
                }
                Ok((200, _)) => {
                    // Still in Phase::Ready; signal is not yet processed.
                }
                Ok((status, body)) => {
                    panic!("unexpected status {status} during drain probe: {body}");
                }
                Err(err) => match err.kind() {
                    std::io::ErrorKind::InvalidData => {
                        panic!("malformed HTTP response during drain probe: {err}");
                    }
                    std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe => {
                        // An idle socket closure or connection reset at the drain transition
                        // boundary is admitted by graceful shutdown semantics.
                    }
                    _ => panic!("unexpected IO error during drain probe: {err}"),
                },
            },
            Err(err) => {
                if err.kind() != std::io::ErrorKind::ConnectionRefused {
                    panic!("unexpected connect error during drain probe: {err}");
                }
            }
        }
        assert!(Instant::now() < deadline, "server never reported draining");
        sleep(Duration::from_millis(10));
    }
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "server exited prematurely before second signal"
    );

    server.signal("INT");
    let status = server.wait_exit(STEP);
    assert_eq!(status.code(), Some(130), "{status}");
}
