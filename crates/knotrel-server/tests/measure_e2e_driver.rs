//! Deterministic integration tests for `measure_e2e` measurement driver (Tooling Increment 1).
//!
//! Validates:
//! - CLI argument defaults, invalid engines, paths, and option combinations.
//! - Engine and server-binary path propagation through initial startup and recovery.
//! - Rejection of effective engine mismatches on externally managed servers.
//! - Schema validation, integer nanoseconds, timing boundaries, units, repetition accounting, and Markdown agreement.
//! - Non-overwrite behavior and output-write failure propagation.
//! - Process ownership, startup failure diagnostics, and bounded cleanup on error.
//! - Frozen v0.2.0 baseline binary compatibility across compact-bfs, ett, and hdt.

#![cfg(unix)]

use std::{
    fs,
    net::{SocketAddr, TcpListener},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

fn find_example_measure_e2e() -> PathBuf {
    static BUILD_ONCE: std::sync::Once = std::sync::Once::new();
    // Locate the built measure_e2e binary relative to knotrel-server or target dir
    let server_bin = PathBuf::from(env!("CARGO_BIN_EXE_knotrel-server"));
    let target_dir = server_bin
        .parent()
        .expect("server binary directory")
        .parent()
        .expect("target directory");

    BUILD_ONCE.call_once(|| {
        let mut command = Command::new(env!("CARGO"));
        command
            .args(["build", "--example", "measure_e2e", "--target-dir"])
            .arg(target_dir);
        if server_bin.parent().unwrap().ends_with("release") {
            command.arg("--release");
        }
        let status = command.status().expect("cargo build --example measure_e2e");
        assert!(status.success(), "Failed to build measure_e2e example");
    });

    let example_bin = target_dir
        .join(server_bin.parent().unwrap().file_name().unwrap())
        .join("examples")
        .join(if cfg!(windows) {
            "measure_e2e.exe"
        } else {
            "measure_e2e"
        });

    assert!(
        example_bin.exists(),
        "measure_e2e binary must exist at {:?}",
        example_bin
    );
    example_bin
}

fn find_example_measure_e2e_with_test_support() -> PathBuf {
    static BUILD_SUPPORT_ONCE: std::sync::Once = std::sync::Once::new();
    let server_bin = PathBuf::from(env!("CARGO_BIN_EXE_knotrel-server"));
    let target_dir = server_bin
        .parent()
        .expect("server binary directory")
        .parent()
        .expect("target directory");
    let support_target_dir = target_dir.join("test-support-target");

    BUILD_SUPPORT_ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--example",
                "measure_e2e",
                "--features",
                "test-support",
                "--target-dir",
                support_target_dir.to_str().unwrap(),
            ])
            .status()
            .expect("cargo build --example measure_e2e --features test-support");
        assert!(
            status.success(),
            "Failed to build measure_e2e example with test-support"
        );
    });

    let example_bin = support_target_dir
        .join("debug")
        .join("examples")
        .join(if cfg!(windows) {
            "measure_e2e.exe"
        } else {
            "measure_e2e"
        });

    assert!(
        example_bin.exists(),
        "measure_e2e test-support binary must exist at {:?}",
        example_bin
    );
    example_bin
}

fn free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr")
}

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct WatchdogOutput {
    output: std::process::Output,
    elapsed: Duration,
    timed_out: bool,
}

/// Runs a command with a strict watchdog timeout, ensuring that child processes and their
/// descendants cannot hang the test suite indefinitely.
fn run_with_watchdog(mut cmd: Command, timeout: Duration) -> WatchdogOutput {
    let start = std::time::Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.process_group(0);
    let child = cmd.spawn().expect("spawn command under watchdog");
    let child_pid = child.id();

    let (tx, rx) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || match rx.recv_timeout(timeout) {
        Ok(()) => false,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Kill entire process group to ensure spawned child processes are reaped
            let pgid_str = format!("-{}", child_pid);
            let _ = Command::new("kill")
                .args(["-9", &pgid_str])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new("kill")
                .args(["-9", &child_pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            true
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => false,
    });

    let output = child
        .wait_with_output()
        .expect("wait_with_output under watchdog");
    let elapsed = start.elapsed();
    let _ = tx.send(());
    let timed_out = watchdog.join().unwrap_or(false);

    WatchdogOutput {
        output,
        elapsed,
        timed_out,
    }
}

/// Manages a running child process that can receive asynchronous cancellation signals,
/// bounded by a strict watchdog that guarantees descendant process cleanup if hung.
struct CancellableProcess {
    child: Option<Child>,
    child_pid: u32,
    watchdog_tx: Option<std::sync::mpsc::Sender<()>>,
    watchdog_handle: Option<std::thread::JoinHandle<bool>>,
    start: std::time::Instant,
}

impl CancellableProcess {
    fn spawn(mut cmd: Command, timeout: Duration) -> Self {
        let start = std::time::Instant::now();
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.process_group(0);
        let child = cmd.spawn().expect("spawn command under watchdog");
        let child_pid = child.id();

        let (tx, rx) = std::sync::mpsc::channel();
        let watchdog = std::thread::spawn(move || match rx.recv_timeout(timeout) {
            Ok(()) => false,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Kill entire process group on watchdog timeout
                let pgid_str = format!("-{}", child_pid);
                let _ = Command::new("kill")
                    .args(["-9", &pgid_str])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = Command::new("kill")
                    .args(["-9", &child_pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                true
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => false,
        });

        Self {
            child: Some(child),
            child_pid,
            watchdog_tx: Some(tx),
            watchdog_handle: Some(watchdog),
            start,
        }
    }

    fn send_signal(&self, signal_flag: &str) {
        let pid_str = self.child_pid.to_string();
        let status = Command::new("kill")
            .args([signal_flag, &pid_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("send signal to child process");
        assert!(
            status.success(),
            "Failed to send {signal_flag} to PID {pid_str}"
        );
    }

    fn wait_for_completion(mut self) -> WatchdogOutput {
        let child = self.child.take().expect("child exists");
        let output = child
            .wait_with_output()
            .expect("wait_with_output under watchdog");
        let elapsed = self.start.elapsed();

        if let Some(tx) = self.watchdog_tx.take() {
            let _ = tx.send(());
        }
        let timed_out = self
            .watchdog_handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or(false);

        WatchdogOutput {
            output,
            elapsed,
            timed_out,
        }
    }
}

impl Drop for CancellableProcess {
    fn drop(&mut self) {
        if let Some(tx) = self.watchdog_tx.take() {
            let _ = tx.send(());
        }
        if let Some(mut child) = self.child.take() {
            let pgid_str = format!("-{}", self.child_pid);
            let _ = Command::new("kill")
                .args(["-9", &pgid_str])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Guard ensuring tracked processes are cleanly killed if still alive upon drop.
struct ProcessCleaner {
    pids: Vec<u32>,
}

impl ProcessCleaner {
    fn new() -> Self {
        Self { pids: Vec::new() }
    }

    fn track(&mut self, pid: u32) {
        self.pids.push(pid);
    }
}

impl Drop for ProcessCleaner {
    fn drop(&mut self) {
        for pid in &self.pids {
            if is_process_alive(*pid) {
                let pid_str = pid.to_string();
                let _ = Command::new("kill")
                    .args(["-9", &pid_str])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// Checks whether a process is currently running on Unix.
fn is_process_alive(pid: u32) -> bool {
    let pid_str = pid.to_string();
    match Command::new("kill")
        .args(["-0", &pid_str])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

struct ExternalServerGuard {
    child: Option<Child>,
    address: SocketAddr,
}

impl ExternalServerGuard {
    fn spawn(engine: &str) -> Self {
        Self::spawn_with_mode(engine, "source-managed")
    }

    fn spawn_with_mode(engine: &str, mode: &str) -> Self {
        let address = free_address();
        let binary = env!("CARGO_BIN_EXE_knotrel-server");
        let mut cmd = Command::new(binary);
        for (key, _) in std::env::vars() {
            if key.starts_with("KNOTREL_") {
                cmd.env_remove(key);
            }
        }
        cmd.env("KNOTREL_ADDR", address.to_string())
            .env("KNOTREL_ENGINE", engine)
            .env("KNOTREL_MODE", mode)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().expect("spawn external knotrel-server");
        let mut guard = Self {
            child: Some(child),
            address,
        };

        // Poll /v1/info until ready
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            if let Some(status) = guard
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
            {
                panic!("External server died prematurely: {status}");
            }
            if let Ok(mut stream) = std::net::TcpStream::connect(address) {
                use std::io::{Read, Write};
                let req = format!(
                    "GET /v1/info HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(req.as_bytes()).is_ok() {
                    let mut resp = String::new();
                    if stream.read_to_string(&mut resp).is_ok() && resp.contains("200 OK") {
                        ready = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(ready, "External server did not become ready on {address}");
        guard
    }
}

impl Drop for ExternalServerGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn test_cli_argument_validation() {
    let example = find_example_measure_e2e();

    // 1. --server-bin specified without --spawn-server
    let server_bin = env!("CARGO_BIN_EXE_knotrel-server");
    let out = Command::new(&example)
        .args(["--server-bin", server_bin])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--server-bin requires --spawn-server (cannot specify server binary when connecting to an existing server)"),
        "stderr was: {stderr}"
    );

    // 2. Nonexistent --server-bin path
    let out = Command::new(&example)
        .args([
            "--spawn-server",
            "--server-bin",
            "/nonexistent/binary/path/knotrel-server",
        ])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "specified server binary '/nonexistent/binary/path/knotrel-server' does not exist"
        ),
        "stderr was: {stderr}"
    );

    // 3. Directory specified as --server-bin
    let out = Command::new(&example)
        .args(["--spawn-server", "--server-bin", "."])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is not a file"), "stderr was: {stderr}");

    // 4. Non-executable file specified as --server-bin
    let cargo_toml = Path::new("Cargo.toml");
    if cargo_toml.exists() {
        let out = Command::new(&example)
            .args(["--spawn-server", "--server-bin", "Cargo.toml"])
            .output()
            .expect("run measure_e2e");
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("is not executable"), "stderr was: {stderr}");
    }

    // 5. Invalid engine selector
    let out = Command::new(&example)
        .args(["--spawn-server", "--engine", "unsupported-engine"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "invalid engine 'unsupported-engine'; expected one of: compact-bfs, ett, hdt"
        ),
        "stderr was: {stderr}"
    );

    // 6. Invalid runs count (0)
    let out = Command::new(&example)
        .args(["--spawn-server", "--runs", "0"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--runs must be >= 1"),
        "stderr was: {stderr}"
    );

    // 7. Invalid batch size (0)
    let out = Command::new(&example)
        .args(["--spawn-server", "--batch-size", "0"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--batch-size must be >= 1"),
        "stderr was: {stderr}"
    );

    // 8. Invalid dataset tier
    let out = Command::new(&example)
        .args(["--spawn-server", "--dataset", "invalid_tier"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown dataset tier"),
        "stderr was: {stderr}"
    );

    // 9. Unknown CLI argument
    let out = Command::new(&example)
        .args(["--unknown-parameter"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Unknown argument: --unknown-parameter"),
        "stderr was: {stderr}"
    );

    // 10. Unknown scenario
    let out = Command::new(&example)
        .args(["--scenario", "unknown-scenario"])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid scenario 'unknown-scenario'"),
        "stderr was: {stderr}"
    );

    // 11. Batch size > 1024 in standalone batch
    let out = Command::new(&example)
        .args([
            "--scenario",
            "standalone-batch",
            "--batch-size",
            "2000",
            "--spawn-server",
        ])
        .output()
        .expect("run measure_e2e");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--batch-size must be <= 1024 for /v1/batch"),
        "stderr was: {stderr}"
    );
}

#[test]
fn test_json_out_refuses_overwrite() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_overwrite_test");
    let existing_json = temp_dir.path.join("evidence.json");

    fs::write(&existing_json, b"{\"existing\":\"data\"}").expect("write dummy file");

    let out = Command::new(&example)
        .args([
            "--spawn-server",
            "--dataset",
            "small",
            "--queries",
            "5",
            "--runs",
            "1",
            "--json-out",
            existing_json.to_str().unwrap(),
        ])
        .output()
        .expect("run measure_e2e");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists; refusing to overwrite existing evidence"),
        "stderr was: {stderr}"
    );

    // Ensure dummy file was not overwritten
    let content = fs::read_to_string(&existing_json).unwrap();
    assert_eq!(content, "{\"existing\":\"data\"}");
}

#[test]
fn test_json_out_write_failure_propagates() {
    let example = find_example_measure_e2e();
    let non_existent_parent = PathBuf::from("/nonexistent/dir/never/created/evidence.json");

    let out = Command::new(&example)
        .args([
            "--spawn-server",
            "--dataset",
            "small",
            "--queries",
            "5",
            "--runs",
            "1",
            "--json-out",
            non_existent_parent.to_str().unwrap(),
        ])
        .output()
        .expect("run measure_e2e");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("parent directory does not exist")
            || stderr.contains("Failed to write structured JSON report")
            || stderr.contains("No such file or directory"),
        "stderr was: {stderr}"
    );
}

#[test]
fn test_external_server_engine_mismatch_rejection() {
    let example = find_example_measure_e2e();

    // Spawn an external server configured with compact-bfs
    let server = ExternalServerGuard::spawn("compact-bfs");

    // Run driver requesting ett against this compact-bfs server
    let out = Command::new(&example)
        .args([
            "--addr",
            &server.address.to_string(),
            "--engine",
            "ett",
            "--dataset",
            "small",
            "--queries",
            "5",
        ])
        .output()
        .expect("run measure_e2e");

    assert!(!out.status.success(), "Driver must reject engine mismatch");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .contains("is configured with engine 'compact-bfs', but --engine 'ett' was requested"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("The driver cannot reconfigure an externally managed server"),
        "stderr was: {stderr}"
    );
}

#[test]
fn test_engine_propagation_json_schema_and_markdown_agreement() {
    let example = find_example_measure_e2e();
    let server_bin = env!("CARGO_BIN_EXE_knotrel-server");
    let temp_dir = TempTestDir::new("knotrel_schema_test");
    let json_out = temp_dir.path.join("measure_report.json");
    let md_out = temp_dir.path.join("measure_report.md");

    let out = Command::new(&example)
        .args([
            "--spawn-server",
            "--server-bin",
            server_bin,
            "--engine",
            "ett",
            "--dataset",
            "small",
            "--queries",
            "15",
            "--runs",
            "2",
            "--json-out",
            json_out.to_str().unwrap(),
            "--output-file",
            md_out.to_str().unwrap(),
        ])
        .output()
        .expect("run measure_e2e");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "measure_e2e failed with code {:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );

    // Verify JSON report
    assert!(json_out.exists(), "JSON output file must exist");
    let json_content = fs::read_to_string(&json_out).expect("read json output");
    let json: serde_json::Value =
        serde_json::from_str(&json_content).expect("parse structured JSON report");

    // 1. Schema version
    assert_eq!(json["schema_version"], "measure_e2e.v1");

    // 2. Driver info
    assert_eq!(json["driver"]["name"], "measure_e2e");
    assert_eq!(json["driver"]["version"], "0.3.0");

    // 3. Server provenance
    let canonical_server_bin = fs::canonicalize(server_bin)
        .unwrap_or_else(|_| PathBuf::from(server_bin))
        .to_string_lossy()
        .to_string();
    assert_eq!(
        json["server_provenance"]["binary_path"],
        canonical_server_bin
    );
    let sha = json["server_provenance"]["binary_sha256"]
        .as_str()
        .expect("binary_sha256 string");
    assert_eq!(sha.len(), 64, "SHA-256 must be 64 hex characters");
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "SHA-256 must be hex"
    );
    let inst_id = json["server_provenance"]["instance_id"]
        .as_str()
        .expect("instance_id string");
    assert!(
        inst_id.starts_with("srv-"),
        "instance_id must start with srv-"
    );

    // 4. Configuration
    assert_eq!(json["configuration"]["requested_engine"], "ett");
    assert_eq!(json["configuration"]["effective_engine"], "ett");
    assert_eq!(json["configuration"]["dataset_tier"], "small");
    assert_eq!(json["configuration"]["node_count"], 1000);
    assert_eq!(json["configuration"]["edge_count"], 2000);
    assert_eq!(json["configuration"]["event_count"], 500);
    assert_eq!(json["configuration"]["query_count"], 15);
    assert_eq!(json["configuration"]["runs"], 2);
    assert_eq!(json["configuration"]["spawn_server"], true);

    // 5. Timing boundaries & definitions
    assert!(json["timing_boundaries"]["units"].is_object());
    assert!(json["timing_boundaries"]["phase_definitions"].is_object());
    let reg_def = json["timing_boundaries"]["phase_definitions"]["registration_through_barrier"]
        .as_str()
        .expect("registration_through_barrier definition");
    assert!(
        reg_def.contains("does NOT measure process spawn to ready"),
        "Phase definition must clarify boundary: {reg_def}"
    );

    // 6. Runs accounting and precision
    let runs = json["runs"].as_array().expect("runs array");
    assert_eq!(runs.len(), 2, "Must contain exactly 2 measured repetitions");

    for (idx, r) in runs.iter().enumerate() {
        assert_eq!(r["run_index"], idx + 1);

        // Snapshot import
        let snap_ns = r["snapshot_import"]["duration_ns"].as_u64().unwrap();
        let snap_ms = r["snapshot_import"]["duration_ms"].as_f64().unwrap();
        assert!(snap_ns > 0);
        assert!((snap_ns as f64 / 1_000_000.0 - snap_ms).abs() < 1e-4);
        assert_eq!(r["snapshot_import"]["element_count"], 3000);
        assert!(r["snapshot_import"]["elements_per_sec"].as_f64().unwrap() > 0.0);

        // Catch up
        let catch_ns = r["catch_up"]["duration_ns"].as_u64().unwrap();
        let catch_ms = r["catch_up"]["duration_ms"].as_f64().unwrap();
        assert!(catch_ns > 0);
        assert!((catch_ns as f64 / 1_000_000.0 - catch_ms).abs() < 1e-4);
        assert_eq!(r["catch_up"]["event_count"], 500);

        // Barrier
        let b_ns = r["barrier"]["duration_ns"].as_u64().unwrap();
        let b_ms = r["barrier"]["duration_ms"].as_f64().unwrap();
        assert!(b_ns > 0);
        assert!((b_ns as f64 / 1_000_000.0 - b_ms).abs() < 1e-4);

        // Registration through barrier
        let sync_ns = r["registration_through_barrier"]["duration_ns"]
            .as_u64()
            .unwrap();
        let sync_ms = r["registration_through_barrier"]["duration_ms"]
            .as_f64()
            .unwrap();
        assert!(sync_ns > 0);
        assert!((sync_ns as f64 / 1_000_000.0 - sync_ms).abs() < 1e-4);

        // Queries & Latency
        assert_eq!(r["query"]["total_queries"], 15);
        assert_eq!(r["query"]["verified_queries"], 15);
        assert_eq!(r["query"]["oracle_agreement_pct"], 100.0);
        assert!(r["query"]["qps"].as_f64().unwrap() > 0.0);

        let lat_ns = &r["query"]["latency_ns"];
        let lat_us = &r["query"]["latency_us"];
        let min_ns = lat_ns["min"].as_u64().unwrap();
        let p50_ns = lat_ns["p50"].as_u64().unwrap();
        let p95_ns = lat_ns["p95"].as_u64().unwrap();
        let p99_ns = lat_ns["p99"].as_u64().unwrap();
        let max_ns = lat_ns["max"].as_u64().unwrap();

        assert!(min_ns > 0);
        assert!(min_ns <= p50_ns);
        assert!(p50_ns <= p95_ns);
        assert!(p95_ns <= p99_ns);
        assert!(p99_ns <= max_ns);

        assert!((p50_ns as f64 / 1_000.0 - lat_us["p50"].as_f64().unwrap()).abs() < 1e-4);
        assert!((p95_ns as f64 / 1_000.0 - lat_us["p95"].as_f64().unwrap()).abs() < 1e-4);

        // Memory
        let final_rss = r["memory"]["sampled_final_rss_kib"].as_u64().unwrap();
        let final_bytes = r["memory"]["sampled_final_rss_bytes"].as_u64().unwrap();
        assert!(final_rss > 0);
        assert_eq!(final_bytes, final_rss * 1024);

        // Restart recovery
        let rec_ns = r["restart_recovery"]["duration_ns"].as_u64().unwrap();
        let rec_ms = r["restart_recovery"]["duration_ms"].as_f64().unwrap();
        assert!(rec_ns > 0);
        assert!((rec_ns as f64 / 1_000_000.0 - rec_ms).abs() < 1e-4);
    }

    // 7. Summary record
    assert!(json["summary"].is_object());
    assert!(
        json["summary"]["mean_snapshot_duration_ms"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(
        json["summary"]["mean_catchup_duration_ms"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(json["summary"]["mean_query_qps"].as_f64().unwrap() > 0.0);
    assert!(json["summary"]["mean_latency_p50_us"].as_f64().unwrap() > 0.0);

    // 8. Markdown report agreement
    assert!(md_out.exists(), "Markdown file must exist");
    let md_content = fs::read_to_string(&md_out).expect("read markdown file");
    assert!(md_content.contains("### Knotrel End-to-End Measurement Report"));
    assert!(md_content.contains("| Run # | Import (ms) | Catch-up (ms) |"));
    assert!(md_content.contains("| **#1** |"));
    assert!(md_content.contains("| **#2** |"));
    assert!(md_content.contains("| **Mean** |"));
    assert!(md_content.contains("15/15 (100.00%)"));

    // Verify rounded agreement: check that run 1's p50 in Markdown matches JSON rounded to 1 decimal place
    let r1_p50_us = runs[0]["query"]["latency_us"]["p50"].as_f64().unwrap();
    let expected_p50_str = format!("{r1_p50_us:.1}");
    assert!(
        md_content.contains(&expected_p50_str),
        "Markdown must contain rounded p50 latency '{expected_p50_str}' matching JSON ({r1_p50_us})"
    );
}

#[test]
fn test_cleanup_on_failure() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_fail_diag");

    // Create a mock executable script that outputs diagnostic stderr and exits with status 42
    let script_path = temp_dir.path.join("mock_failing_server.sh");
    let script_content =
        "#!/bin/sh\necho 'FATAL: port already in use or configuration corrupt' >&2\nexit 42\n";
    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(5));
    assert!(!w_out.timed_out, "Driver hung during failure test");
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    let stdout = String::from_utf8_lossy(&w_out.output.stdout);
    let all_out = format!("{stdout}\n{stderr}");

    assert!(
        all_out.contains("Server process exited prematurely with status exit status: 42"),
        "Must record child exit code: {all_out}"
    );
    assert!(
        all_out.contains("FATAL: port already in use or configuration corrupt"),
        "Must capture child stderr diagnostics: {all_out}"
    );
}

#[test]
fn test_frozen_v020_baseline_compatibility_and_engine_selection() {
    let baseline_bin =
        Path::new("/Users/rt/workspace/knotrel/baseline-v0.2.0/target/release/knotrel-server");
    if !baseline_bin.exists() {
        eprintln!(
            "SKIPPING frozen baseline test: binary not found at {:?}",
            baseline_bin
        );
        return;
    }

    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_frozen_smoke");

    for engine in ["compact-bfs", "ett", "hdt"] {
        let json_path = temp_dir.path.join(format!("smoke_{engine}.json"));
        let out = Command::new(&example)
            .args([
                "--spawn-server",
                "--server-bin",
                baseline_bin.to_str().unwrap(),
                "--engine",
                engine,
                "--dataset",
                "small",
                "--queries",
                "5",
                "--runs",
                "1",
                "--json-out",
                json_path.to_str().unwrap(),
            ])
            .output()
            .expect("run measure_e2e against frozen baseline");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "Frozen baseline smoke failed for engine '{engine}'\nstdout: {stdout}\nstderr: {stderr}"
        );

        // Verify JSON report was written and effective engine matches
        assert!(json_path.exists());
        let content = fs::read_to_string(&json_path).expect("read json");
        let json: serde_json::Value = serde_json::from_str(&content).expect("parse json");
        assert_eq!(json["configuration"]["requested_engine"], engine);
        assert_eq!(json["configuration"]["effective_engine"], engine);
        assert_eq!(
            json["server_provenance"]["binary_sha256"],
            "b156d39718434fb395cc19d2937513376f99819c8ac5e48963a3f776ab44ed4a"
        );
        assert_eq!(json["runs"][0]["query"]["verified_queries"], 5);
        assert_eq!(json["runs"][0]["query"]["oracle_agreement_pct"], 100.0);
    }
}

#[test]
fn test_regression_server_accepting_connections_but_never_responding() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_unresponsive");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let script_path = temp_dir.path.join("mock_unresponsive_server.sh");
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
exec python3 -u -c '
import os, socket, time, sys

addr = os.environ.get("KNOTREL_ADDR", "127.0.0.1:0")
host, port = addr.split(":")
port = int(port)

with open("{pid_file}", "w") as f:
    f.write(str(os.getpid()))

s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((host, port))
s.listen(5)

while True:
    try:
        conn, _ = s.accept()
        time.sleep(60)
    except Exception:
        break
'
"#,
        pid_file = pid_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
    ]);

    // Use a 12s bounded watchdog to prevent test hang if deadline fails
    let w_out = run_with_watchdog(cmd, Duration::from_secs(12));

    if let Some(pid) = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
    {
        cleaner.track(pid);
    }

    assert!(
        !w_out.timed_out,
        "Driver hung past watchdog deadline! (Elapsed: {:.2}s)",
        w_out.elapsed.as_secs_f64()
    );
    assert!(
        !w_out.output.status.success(),
        "Driver must fail when server does not respond to health check"
    );

    // Enforce deadline bounds: driver startup deadline is 5s.
    // Elapsed wall-clock time must be >= 4.9s and < 8.0s (reproduction showed > 8s hang previously).
    assert!(
        w_out.elapsed >= Duration::from_millis(4900),
        "Driver exited too early ({:.2}s); expected full ~5s timeout",
        w_out.elapsed.as_secs_f64()
    );
    assert!(
        w_out.elapsed < Duration::from_millis(8000),
        "Driver took {:.2}s; startup deadline of 5s was not enforced across probe operations",
        w_out.elapsed.as_secs_f64()
    );

    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );
    assert!(
        all_out.contains("Timed out after 5s waiting for server to listen on"),
        "Must report bounded timeout diagnostics: {all_out}"
    );
}

#[test]
fn test_regression_server_emitting_more_than_pipe_capacity_before_startup() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_pipe_saturation");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let script_path = temp_dir.path.join("mock_pipe_flooder.sh");

    // Generates 256 KiB (> 4x standard pipe capacity) to stderr before exiting with code 42
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
exec python3 -u -c '
import os, sys

with open("{pid_file}", "w") as f:
    f.write(str(os.getpid()))

chunk = "X" * 1000 + "\n"
for i in range(256):
    sys.stderr.write(f"DIAG_FLOOD_{{i:04d}}: {{chunk}}")
sys.stderr.flush()
sys.exit(42)
'
"#,
        pid_file = pid_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
    ]);

    // Use a 10s watchdog to detect if pipe saturation causes child/driver deadlock
    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));

    if let Some(pid) = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
    {
        cleaner.track(pid);
    }

    assert!(
        !w_out.timed_out,
        "Driver deadlocked on pipe saturation! (Elapsed: {:.2}s)",
        w_out.elapsed.as_secs_f64()
    );
    assert!(
        w_out.elapsed < Duration::from_secs(4),
        "Driver should detect child exit promptly without blocking on pipe (Elapsed: {:.2}s)",
        w_out.elapsed.as_secs_f64()
    );
    assert!(!w_out.output.status.success());

    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );

    // Stderr drainer must have captured truncation header and diagnostic markers
    assert!(
        all_out.contains("status exit status: 42"),
        "Must record exit status 42: {all_out}"
    );
    assert!(
        all_out.contains("earlier stderr truncated; total bytes read:"),
        "Must report bounded truncation banner for flooded stderr: {all_out}"
    );
    assert!(
        all_out.contains("DIAG_FLOOD_"),
        "Must contain preserved diagnostic lines: {all_out}"
    );
}

#[test]
fn test_regression_early_exit_with_diagnostics() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_early_exit");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let script_path = temp_dir.path.join("mock_early_exit_server.sh");
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
echo "$$" > "{pid_file}"
echo "FATAL_CONFIG_ERROR: invalid journal offset at block 4096" >&2
echo "DIAGNOSTIC_TRACE: memory subsystem initialized but journal is invalid" >&2
exit 77
"#,
        pid_file = pid_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(5));

    if let Some(pid) = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
    {
        cleaner.track(pid);
    }

    assert!(!w_out.timed_out, "Driver hung during early child exit");
    assert!(
        w_out.elapsed < Duration::from_millis(1500),
        "Driver should detect premature exit immediately without waiting for 5s deadline (Elapsed: {:.2}s)",
        w_out.elapsed.as_secs_f64()
    );
    assert!(!w_out.output.status.success());

    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );

    assert!(
        all_out.contains("status exit status: 77"),
        "Must report exit code 77: {all_out}"
    );
    assert!(
        all_out.contains("FATAL_CONFIG_ERROR: invalid journal offset at block 4096"),
        "Must capture fatal error diagnostic: {all_out}"
    );
    assert!(
        all_out.contains("DIAGNOSTIC_TRACE: memory subsystem initialized but journal is invalid"),
        "Must capture diagnostic trace: {all_out}"
    );
}

#[test]
fn test_regression_child_termination_and_reaping_after_startup_failure() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_reaping");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let script_path = temp_dir.path.join("mock_hanging_server.sh");

    // Script records its own PID, then sleeps.
    // Using exec ensures the sleep process replaces the shell with identical PID.
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
echo "$$" > "{pid_file}"
exec /bin/sleep 60
"#,
        pid_file = pid_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
    ]);

    // Startup deadline is 5s, watchdog at 10s
    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));

    assert!(
        !w_out.timed_out,
        "Driver hung past watchdog deadline during startup failure"
    );
    assert!(!w_out.output.status.success());

    // Read recorded child PID
    let pid_text = fs::read_to_string(&pid_file).expect("child.pid must exist and be readable");
    let child_pid: u32 = pid_text
        .trim()
        .parse()
        .expect("child.pid must contain valid numeric PID");

    // Always register for cleanup safety in case test fails
    cleaner.track(child_pid);

    // Verify child process was terminated and reaped:
    // `kill -0 <pid>` returns non-zero (exit code 1) when the process no longer exists.
    assert!(
        !is_process_alive(child_pid),
        "Child process {child_pid} was not terminated and reaped after startup failure!"
    );
}

#[test]
fn test_regression_cancellation_during_startup_cleans_up_owned_server_and_preserves_diagnostics() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_cancel_startup");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let script_path = temp_dir.path.join("mock_startup_hang_server.sh");
    let json_out = temp_dir.path.join("should_not_exist.json");

    // Script records PID, writes diagnostic to stderr, and hangs
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
echo "$$" > "{pid_file}"
echo "STARTUP_DIAGNOSTIC_BEFORE_CANCEL: subsystem init ok, waiting for bind" >&2
exec /bin/sleep 60
"#,
        pid_file = pid_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let proc = CancellableProcess::spawn(cmd, Duration::from_secs(10));

    // Synchronize on observable child PID evidence
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut child_pid: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .filter(|&pid| is_process_alive(pid))
        {
            child_pid = Some(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let child_pid = child_pid.expect("Child process did not write PID file within deadline");
    cleaner.track(child_pid);

    // Send SIGINT to measure_e2e while in startup phase
    proc.send_signal("-2");

    let w_out = proc.wait_for_completion();
    assert!(!w_out.timed_out, "Driver hung during SIGINT cancellation");
    assert!(
        !w_out.output.status.success(),
        "Driver must exit unsuccessfully on SIGINT"
    );

    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );

    // 1. Verify owned child was terminated and reaped
    assert!(
        !is_process_alive(child_pid),
        "Owned child process {child_pid} was not killed and reaped upon SIGINT cancellation!"
    );

    // 2. Verify cancellation notice and preserved stderr diagnostics
    assert!(
        all_out.contains("measure_e2e: received SIGINT; terminating owned server and exiting..."),
        "Driver output must record SIGINT cancellation: {all_out}"
    );
    assert!(
        all_out.contains("STARTUP_DIAGNOSTIC_BEFORE_CANCEL: subsystem init ok, waiting for bind"),
        "Driver output must preserve child stderr diagnostics: {all_out}"
    );

    // 3. Verify no successful report was published
    assert!(
        !json_out.exists(),
        "JSON report file must not be published upon cancellation"
    );
}

#[test]
fn test_regression_cancellation_during_active_measurement_cleans_up_owned_server() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_cancel_active");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("child.pid");
    let marker_file = temp_dir.path.join("active_measurement.marker");
    let script_path = temp_dir.path.join("mock_active_server.sh");
    let json_out = temp_dir.path.join("should_not_exist.json");

    // Mock server starts HTTP server, responds to health/info, and signals active measurement marker
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
exec python3 -u -c '
import os, sys, http.server, socketserver, time

pid_file = "{pid_file}"
marker_file = "{marker_file}"
addr = os.environ.get("KNOTREL_ADDR", "127.0.0.1:0")
host, port = addr.split(":")
port = int(port)

with open(pid_file, "w") as f:
    f.write(str(os.getpid()))

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"OK")
        elif self.path == "/v1/info":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{{\"engine\":\"compact-bfs\",\"mode\":\"source-managed\",\"instance_id\":\"srv-mock\"}}")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        # Signal that measurement feeder phase is active
        with open(marker_file, "w") as f:
            f.write("ACTIVE")
        # Delay to hold active measurement open
        time.sleep(60)

    def log_message(self, format, *args):
        pass

server = socketserver.TCPServer((host, port), Handler)
server.serve_forever()
'
"#,
        pid_file = pid_file.display(),
        marker_file = marker_file.display()
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        script_path.to_str().unwrap(),
        "--dataset",
        "small",
        "--queries",
        "5",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let proc = CancellableProcess::spawn(cmd, Duration::from_secs(10));

    // Synchronize on observable active measurement marker evidence
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut active = false;
    let mut child_pid: Option<u32> = None;

    while std::time::Instant::now() < deadline {
        if child_pid.is_none() {
            child_pid = fs::read_to_string(&pid_file)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok());
        }
        if marker_file.exists() {
            active = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    assert!(
        active,
        "Driver did not reach active measurement phase within deadline"
    );
    let child_pid = child_pid.expect("Child PID must be recorded");
    cleaner.track(child_pid);

    // Send SIGTERM to measure_e2e during active measurement
    proc.send_signal("-15");

    let w_out = proc.wait_for_completion();
    assert!(!w_out.timed_out, "Driver hung during SIGTERM cancellation");
    assert!(
        !w_out.output.status.success(),
        "Driver must exit unsuccessfully on SIGTERM"
    );

    // 1. Verify owned child was terminated and reaped
    assert!(
        !is_process_alive(child_pid),
        "Owned child process {child_pid} was not killed and reaped upon SIGTERM cancellation!"
    );

    // 2. Verify no report was published
    assert!(
        !json_out.exists(),
        "JSON report file must not be published upon cancellation"
    );
}

#[test]
fn test_regression_cancellation_does_not_kill_externally_managed_server() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_cancel_external");
    let mut cleaner = ProcessCleaner::new();

    let pid_file = temp_dir.path.join("external.pid");
    let marker_file = temp_dir.path.join("driver_connected.marker");
    let script_path = temp_dir.path.join("mock_external_server.sh");
    let json_out = temp_dir.path.join("should_not_exist.json");
    let address = free_address();

    // Mock external server starts HTTP service and signals when driver connects
    let script_content = format!(
        r#"#!/bin/sh
export PATH="/bin:/usr/bin"
exec python3 -u -c '
import os, sys, http.server, socketserver, time

pid_file = "{pid_file}"
marker_file = "{marker_file}"
addr = "{address}"
host, port = addr.split(":")
port = int(port)

with open(pid_file, "w") as f:
    f.write(str(os.getpid()))

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"OK")
        elif self.path == "/v1/info":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{{\"engine\":\"compact-bfs\",\"mode\":\"source-managed\",\"instance_id\":\"srv-ext\"}}")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        # Driver connects to feeder discover/import: create witness marker
        with open(marker_file, "w") as f:
            f.write("CONNECTED")
        time.sleep(60)

    def log_message(self, format, *args):
        pass

server = socketserver.TCPServer((host, port), Handler)
server.serve_forever()
'
"#,
        pid_file = pid_file.display(),
        marker_file = marker_file.display(),
        address = address
    );

    fs::write(&script_path, script_content).expect("write mock script");
    let mut perms = fs::metadata(&script_path).expect("meta").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set executable");

    // Spawn externally managed server under watchdog
    let mut ext_cmd = Command::new(&script_path);
    ext_cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let mut ext_child = ext_cmd.spawn().expect("spawn mock external server");

    // Synchronize on observable external server readiness
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut ext_pid: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .filter(|&pid| is_process_alive(pid))
        {
            ext_pid = Some(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let ext_pid = ext_pid.expect("External server failed to start within deadline");
    cleaner.track(ext_pid);

    let mut cmd = Command::new(&example);
    cmd.args([
        "--addr",
        &address.to_string(),
        "--engine",
        "compact-bfs",
        "--dataset",
        "small",
        "--queries",
        "1000",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let proc = CancellableProcess::spawn(cmd, Duration::from_secs(10));

    // Synchronize on observable connection marker evidence
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut connected = false;
    while std::time::Instant::now() < deadline {
        if marker_file.exists() {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        connected,
        "Driver did not connect to external server within deadline"
    );

    // Send SIGINT to measure_e2e while connected to external server
    proc.send_signal("-2");

    let w_out = proc.wait_for_completion();
    assert!(!w_out.timed_out, "Driver hung during SIGINT cancellation");
    assert!(
        !w_out.output.status.success(),
        "Driver must exit unsuccessfully on SIGINT"
    );

    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );
    assert!(
        all_out.contains("measure_e2e: received SIGINT"),
        "Driver output must record SIGINT: {all_out}"
    );
    assert!(
        all_out.contains("measure_e2e: no owned child server process active."),
        "Driver output must recognize server is external: {all_out}"
    );

    // CRITICAL: Externally managed server MUST NOT be killed
    assert!(
        is_process_alive(ext_pid),
        "Externally managed server process {ext_pid} was killed by cancellation handler!"
    );

    // Clean up external server
    let _ = ext_child.kill();
    let _ = ext_child.wait();

    // No report published
    assert!(
        !json_out.exists(),
        "JSON report must not be published upon cancellation"
    );
}

#[test]
fn test_normal_build_ignores_test_hooks_and_does_not_pause() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_normal_build_ignores_hooks");

    let witness_file = temp_dir.path.join("spawn.witness");
    let resume_file = temp_dir.path.join("spawn.resume"); // will never be created
    let json_out = temp_dir.path.join("normal_build.json");

    let server_bin = env!("CARGO_BIN_EXE_knotrel-server");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        server_bin,
        "--dataset",
        "small",
        "--queries",
        "5",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);
    cmd.env(
        "KNOTREL_TEST_HOOK_POST_SPAWN",
        format!("{}:{}", witness_file.display(), resume_file.display()),
    );

    // If normal build inspected the hook, it would pause waiting for resume_file (which never exists).
    // Because normal build ignores hooks, it completes promptly.
    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    assert!(!w_out.timed_out, "Normal build hung on test hook");
    assert!(
        w_out.output.status.success(),
        "Normal build failed: {}",
        String::from_utf8_lossy(&w_out.output.stderr)
    );
    assert!(
        !witness_file.exists(),
        "Normal build must not create witness files or inspect test hooks"
    );
    assert!(json_out.exists(), "Normal build completed and wrote report");
}

#[test]
fn test_regression_cancellation_during_spawn_transition_forces_reap_and_no_orphan() {
    let example = find_example_measure_e2e_with_test_support();
    let temp_dir = TempTestDir::new("knotrel_cancel_spawn_trans");
    let mut cleaner = ProcessCleaner::new();

    let witness_file = temp_dir.path.join("spawn.witness");
    let cancel_witness_file = temp_dir.path.join("spawn.cancelled");
    let resume_file = temp_dir.path.join("spawn.resume");
    let json_out = temp_dir.path.join("should_not_exist.json");

    let server_bin = env!("CARGO_BIN_EXE_knotrel-server");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        server_bin,
        "--dataset",
        "small",
        "--queries",
        "5",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);
    cmd.env(
        "KNOTREL_TEST_HOOK_POST_SPAWN",
        format!("{}:{}", witness_file.display(), resume_file.display()),
    );
    cmd.env(
        "KNOTREL_TEST_HOOK_CANCEL_WITNESS",
        cancel_witness_file.to_str().unwrap(),
    );

    let proc = CancellableProcess::spawn(cmd, Duration::from_secs(25));

    // 1. Await witness file produced by KNOTREL_TEST_HOOK_POST_SPAWN
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut driver_pid: Option<u32> = None;
    let mut child_pid: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(&witness_file) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 2 {
                let d_pid = lines[0].trim().parse::<u32>().ok();
                let c_pid = lines[1].trim().parse::<u32>().ok();
                if let (Some(d), Some(c)) = (d_pid, c_pid) {
                    driver_pid = Some(d);
                    child_pid = Some(c);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let driver_pid = driver_pid.expect("Spawn transition hook witness driver PID not found");
    let child_pid = child_pid.expect("Spawn transition hook witness child PID not found");
    cleaner.track(driver_pid);
    cleaner.track(child_pid);

    assert!(
        is_process_alive(driver_pid),
        "Driver process must be alive at spawn hook"
    );
    assert!(
        is_process_alive(child_pid),
        "Child process must be alive at spawn hook"
    );

    // 2. Send SIGINT while in the spawn transition before ServerGuard registration
    proc.send_signal("-2");

    // 3. Obtain a witness that cancellation has been accepted while the transition is paused
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut cancel_accepted = false;
    while std::time::Instant::now() < deadline {
        if cancel_witness_file.exists() {
            cancel_accepted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cancel_accepted,
        "Cancellation witness was not created while spawn transition was paused"
    );

    // 4. Cover delayed transition completion beyond the former five-second timeout:
    // With transition paused, sleep for 6.0 seconds. The driver must NOT time out,
    // must NOT discard state, and must NOT exit or leave the child orphaned.
    std::thread::sleep(Duration::from_millis(6000));

    assert!(
        is_process_alive(driver_pid),
        "Driver process exited prematurely while spawn transition was still paused!"
    );

    // 5. Only then release the transition
    fs::write(&resume_file, b"RESUME").expect("write resume file");

    let w_out = proc.wait_for_completion();
    assert!(!w_out.timed_out, "Driver hung during spawn cancellation");
    assert_eq!(
        w_out.output.status.code(),
        Some(130),
        "Driver must exit with status 130 on SIGINT"
    );

    // 6. CRITICAL: Child process MUST be terminated and reaped; no orphan process may survive
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut child_reaped = false;
    while std::time::Instant::now() < deadline {
        if !is_process_alive(child_pid) {
            child_reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child_reaped,
        "Child process {child_pid} survived cancellation during spawn transition!"
    );

    // 7. Verify no successful report published
    assert!(
        !json_out.exists(),
        "JSON report must not be published upon cancellation"
    );

    // 8. Verify no subsequent recovery spawn
    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );
    assert!(
        !all_out.contains("Restart & Rebuild Recovery Duration"),
        "Driver must not have attempted recovery after cancellation: {all_out}"
    );
}

#[test]
fn test_regression_cancellation_during_termination_transition_blocks_exit_until_reaped() {
    let example = find_example_measure_e2e_with_test_support();
    let temp_dir = TempTestDir::new("knotrel_cancel_term_trans");
    let mut cleaner = ProcessCleaner::new();

    let witness_file = temp_dir.path.join("term.witness");
    let cancel_witness_file = temp_dir.path.join("term.cancelled");
    let resume_file = temp_dir.path.join("term.resume");
    let json_out = temp_dir.path.join("should_not_exist.json");

    let server_bin = env!("CARGO_BIN_EXE_knotrel-server");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--spawn-server",
        "--server-bin",
        server_bin,
        "--dataset",
        "small",
        "--queries",
        "5",
        "--runs",
        "2",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);
    cmd.env(
        "KNOTREL_TEST_HOOK_TERMINATING",
        format!("{}:{}", witness_file.display(), resume_file.display()),
    );
    cmd.env(
        "KNOTREL_TEST_HOOK_CANCEL_WITNESS",
        cancel_witness_file.to_str().unwrap(),
    );

    let proc = CancellableProcess::spawn(cmd, Duration::from_secs(25));

    // 1. Await witness file produced by KNOTREL_TEST_HOOK_TERMINATING at end of run 1 before recovery
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut driver_pid: Option<u32> = None;
    let mut child_pid: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(&witness_file) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 2 {
                let d_pid = lines[0].trim().parse::<u32>().ok();
                let c_pid = lines[1].trim().parse::<u32>().ok();
                if let (Some(d), Some(c)) = (d_pid, c_pid) {
                    driver_pid = Some(d);
                    child_pid = Some(c);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let driver_pid = driver_pid.expect("Terminating transition hook witness driver PID not found");
    let child_pid = child_pid.expect("Terminating transition hook witness child PID not found");
    cleaner.track(driver_pid);
    cleaner.track(child_pid);

    assert!(
        is_process_alive(driver_pid),
        "Driver process must be alive at term hook"
    );

    // 2. Send SIGTERM while in the middle of terminating transition
    proc.send_signal("-15");

    // 3. Obtain a witness that cancellation has been accepted while the transition is paused
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut cancel_accepted = false;
    while std::time::Instant::now() < deadline {
        if cancel_witness_file.exists() {
            cancel_accepted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cancel_accepted,
        "Cancellation witness was not created while terminating transition was paused"
    );

    // 4. Cover delayed transition completion beyond the former five-second timeout:
    // With transition paused, sleep for 6.0 seconds. The driver must NOT time out,
    // must NOT discard state, and must NOT exit or leave the child orphaned.
    std::thread::sleep(Duration::from_millis(6000));

    assert!(
        is_process_alive(driver_pid),
        "Driver process exited prematurely while termination transition was still paused!"
    );

    // 5. Only then release the transition
    fs::write(&resume_file, b"RESUME").expect("write resume file");

    let w_out = proc.wait_for_completion();
    assert!(
        !w_out.timed_out,
        "Driver hung during termination cancellation"
    );
    assert_eq!(
        w_out.output.status.code(),
        Some(143),
        "Driver must exit with status 143 on SIGTERM"
    );

    // 6. CRITICAL: Child process MUST be reaped; no orphan process may survive
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut child_reaped = false;
    while std::time::Instant::now() < deadline {
        if !is_process_alive(child_pid) {
            child_reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child_reaped,
        "Child process {child_pid} survived cancellation during termination transition!"
    );

    // 7. Verify no report was written
    assert!(
        !json_out.exists(),
        "JSON report must not be published upon cancellation"
    );

    // 8. Verify no subsequent recovery spawn
    let all_out = format!(
        "{}\n{}",
        String::from_utf8_lossy(&w_out.output.stdout),
        String::from_utf8_lossy(&w_out.output.stderr)
    );
    assert!(
        !all_out.contains("Restart & Rebuild Recovery Duration"),
        "Driver must not have attempted recovery after cancellation: {all_out}"
    );
}

#[test]
fn test_standalone_batch_trace_and_partial_batch_across_sizes() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_batch_sizes_test");

    for &batch_size in &[1, 8, 64, 256] {
        let json_out = temp_dir.path.join(format!("batch_{batch_size}.json"));
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--batch-size",
            &batch_size.to_string(),
            "--spawn-server",
            "--engine",
            "compact-bfs",
            "--json-out",
            json_out.to_str().unwrap(),
        ]);

        let w_out = run_with_watchdog(cmd, Duration::from_secs(45));
        assert!(
            !w_out.timed_out,
            "measure_e2e timed out for batch_size={batch_size}"
        );
        let stdout = String::from_utf8_lossy(&w_out.output.stdout);
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            w_out.output.status.success(),
            "measure_e2e failed for batch_size={batch_size}\nstdout: {stdout}\nstderr: {stderr}"
        );

        assert!(json_out.exists(), "JSON output file must exist");
        let content = fs::read_to_string(&json_out).expect("read json output");
        let json: serde_json::Value = serde_json::from_str(&content).expect("parse json");

        assert_eq!(json["schema_version"], "measure_e2e.batch.v1");
        assert_eq!(json["configuration"]["scenario"], "standalone-batch");
        assert_eq!(
            json["configuration"]["trace_identity"],
            "deterministic_500_round_link_cut_reversal"
        );
        assert_eq!(json["configuration"]["node_count"], 1000);
        assert_eq!(json["configuration"]["total_logical_operations"], 2000);
        assert_eq!(json["configuration"]["link_operations"], 500);
        assert_eq!(json["configuration"]["cut_operations"], 500);
        assert_eq!(json["configuration"]["query_operations"], 1000);
        assert_eq!(json["configuration"]["batch_size"], batch_size);

        let expected_reqs = 2000_usize.div_ceil(batch_size);
        let expected_partial = if 2000 % batch_size != 0 {
            Some(2000 % batch_size)
        } else {
            None
        };
        let expected_full = if expected_partial.is_some() {
            expected_reqs - 1
        } else {
            expected_reqs
        };

        assert_eq!(json["configuration"]["actual_request_count"], expected_reqs);
        assert_eq!(json["configuration"]["full_batches"], expected_full);
        if let Some(p) = expected_partial {
            assert_eq!(json["configuration"]["partial_batch_size"], p);
        } else {
            assert!(json["configuration"]["partial_batch_size"].is_null());
        }

        // Run verification
        let run = &json["runs"][0];
        assert_eq!(run["setup"]["created_nodes"], 1000);
        assert_eq!(run["setup"]["initial_state_version"], "1000");
        assert_eq!(run["warmup"]["query_count"], 50);
        assert_eq!(run["warmup"]["post_warmup_state_version"], "1000");

        assert_eq!(run["measurement_window"]["total_logical_operations"], 2000);
        assert_eq!(run["measurement_window"]["effective_mutations"], 1000);
        assert!(
            run["measurement_window"]["logical_ops_per_sec"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        assert!(
            run["measurement_window"]["elapsed_window_ns_per_logical_op"]
                .as_f64()
                .unwrap()
                > 0.0
        );

        assert_eq!(run["request_latency"]["total_requests"], expected_reqs);
        assert!(
            run["request_latency"]["latency_ns"]["min"]
                .as_u64()
                .unwrap()
                > 0
        );

        assert_eq!(run["verification"]["all_items_matched"], true);
        assert_eq!(run["verification"]["total_verified_items"], 2000);
        assert_eq!(run["verification"]["expected_final_state_version"], "2000");
        assert_eq!(run["verification"]["actual_final_state_version"], "2000");
        assert_eq!(run["verification"]["final_node_count"], 1000);
        assert_eq!(run["verification"]["final_edge_count"], 0);

        // Verify timing boundary concepts
        let units = &json["timing_boundaries"]["units"];
        assert!(
            !units["elapsed_window_ns_per_logical_op"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        let notes = &json["timing_boundaries"]["notes"];
        assert!(
            notes["latency_concept"]
                .as_str()
                .unwrap()
                .contains("Never divide request latency by batch size")
        );
    }
}

#[test]
fn test_standalone_batch_external_server_validation() {
    let example = find_example_measure_e2e();

    // 1. External server in source-managed mode -> must be rejected
    {
        let server = ExternalServerGuard::spawn_with_mode("compact-bfs", "source-managed");
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &server.address.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("is running in source-managed mode; standalone batch scenario requires standalone mode"),
            "stderr was: {stderr}"
        );
    }

    // 2. External server in standalone mode, but engine mismatch -> must be rejected
    {
        let server = ExternalServerGuard::spawn_with_mode("compact-bfs", "standalone");
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &server.address.to_string(),
            "--engine",
            "ett",
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains(
                "is configured with engine 'compact-bfs', but --engine 'ett' was requested"
            ),
            "stderr was: {stderr}"
        );
    }

    // 3. External server in standalone mode, but non-pristine state -> must be rejected
    {
        let server = ExternalServerGuard::spawn_with_mode("compact-bfs", "standalone");

        // Mutate external server before driver connects
        {
            use std::io::{Read, Write};
            let mut stream =
                std::net::TcpStream::connect(server.address).expect("connect to external server");
            let body = r#"{"operations":[{"op":"add_node","node":"9999"}]}"#;
            let req = format!(
                "POST /v1/batch HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                server.address,
                body.len(),
                body
            );
            stream.write_all(req.as_bytes()).expect("write request");
            let mut resp = String::new();
            stream.read_to_string(&mut resp).expect("read response");
            assert!(resp.contains("200 OK"), "setup dirty node failed: {resp}");
        }

        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &server.address.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("is not in pristine state")
                && stderr.contains("refusing to mutate non-pristine server"),
            "stderr was: {stderr}"
        );
    }
}

#[test]
fn test_standalone_batch_rejection_of_corrupt_or_failing_server() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_corrupt_server_test");
    let json_out = temp_dir.path.join("corrupt_report.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        use std::io::{Read, Write};
        // 1. Handle GET /v1/info
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req = [0u8; 1024];
            let _ = stream.read(&mut req);
            let body =
                r#"{"engine":"compact-bfs","experimental":false,"instance_id":"mock-corrupt"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }

        // 2. Handle GET /v1/stats (pristine check)
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req = [0u8; 1024];
            let _ = stream.read(&mut req);
            let body = r#"{"node_count":0,"edge_count":0,"state_version":"0"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nx-knotrel-instance-id: mock-corrupt\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }

        // 3. Handle POST /v1/batch during setup - return 200 OK with changed: false (corrupt oracle outcome)
        if let Ok((mut stream, _)) = listener.accept() {
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                let s = String::from_utf8_lossy(&req);
                if let Some(pos) = s.find("Content-Length: ") {
                    let cl_str = &s[pos + 16..];
                    let cl: usize = cl_str
                        .lines()
                        .next()
                        .and_then(|l| l.trim().parse().ok())
                        .unwrap_or(0);
                    if let Some(head_end) = req.windows(4).position(|w| w == b"\r\n\r\n")
                        && req.len() >= head_end + 4 + cl
                    {
                        break;
                    }
                }
            }

            // Return 1,000 results where op 0 has changed: false (violating expected true)
            let mut results = Vec::new();
            for _ in 0..1000 {
                results.push(serde_json::json!({ "result": "changed", "changed": false }));
            }
            let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nx-knotrel-state-version: 1000\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out, "Driver hung waiting for corrupt response");
    assert!(
        !w_out.output.status.success(),
        "Driver must fail when server returns corrupt item outcome during setup"
    );
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("Setup op 0 (add_node) did not return changed: true"),
        "stderr was: {stderr}"
    );
    assert!(
        !json_out.exists(),
        "JSON report must not be written when batch benchmark fails"
    );
}

#[test]
fn test_standalone_batch_frozen_v020_compatibility() {
    let baseline_bin =
        Path::new("/Users/rt/workspace/knotrel/baseline-v0.2.0/target/release/knotrel-server");
    if !baseline_bin.exists() {
        eprintln!(
            "SKIPPING frozen baseline batch test: binary not found at {:?}",
            baseline_bin
        );
        return;
    }

    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_frozen_batch_smoke");
    let json_out = temp_dir.path.join("frozen_batch.json");

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--server-bin",
        baseline_bin.to_str().unwrap(),
        "--spawn-server",
        "--engine",
        "compact-bfs",
        "--batch-size",
        "64",
        "--runs",
        "1",
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(30));
    assert!(
        !w_out.timed_out,
        "measure_e2e timed out against frozen baseline"
    );
    let stdout = String::from_utf8_lossy(&w_out.output.stdout);
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        w_out.output.status.success(),
        "measure_e2e failed against frozen baseline v0.2.0\nstdout: {stdout}\nstderr: {stderr}"
    );

    assert!(json_out.exists(), "JSON output file must exist");
    let content = fs::read_to_string(&json_out).expect("read json output");
    let json: serde_json::Value = serde_json::from_str(&content).expect("parse json");

    assert_eq!(json["schema_version"], "measure_e2e.batch.v1");
    assert_eq!(
        json["server_provenance"]["binary_sha256"],
        "b156d39718434fb395cc19d2937513376f99819c8ac5e48963a3f776ab44ed4a"
    );
    assert_eq!(json["runs"][0]["verification"]["all_items_matched"], true);
    assert_eq!(
        json["runs"][0]["verification"]["total_verified_items"],
        2000
    );
    assert_eq!(
        json["runs"][0]["verification"]["actual_final_state_version"],
        "2000"
    );
}

fn read_http_request_from_stream(stream: &mut std::net::TcpStream) -> (String, String) {
    use std::io::Read;
    let mut req_bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        req_bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&req_bytes[..pos]).to_string();
            let cl = headers
                .lines()
                .find(|l| l.to_lowercase().starts_with("content-length:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if req_bytes.len() >= pos + 4 + cl {
                let body = String::from_utf8_lossy(&req_bytes[pos + 4..pos + 4 + cl]).to_string();
                return (headers, body);
            }
        }
    }
    (
        String::from_utf8_lossy(&req_bytes).to_string(),
        String::new(),
    )
}

fn write_mock_http_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &str,
) {
    use std::io::Write;
    let mut resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    resp.push_str(body);
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn handle_mock_batch_preamble(listener: &TcpListener) {
    // 1. GET /v1/info
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[],
            r#"{"engine":"compact-bfs","experimental":false,"instance_id":"mock-preamble"}"#,
        );
    }

    // 2. Initial GET /v1/stats (pristine)
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[("x-knotrel-instance-id", "mock-preamble")],
            r#"{"node_count":0,"edge_count":0,"state_version":"0"}"#,
        );
    }

    // 3. Setup POST /v1/batch (1000 add_node)
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        let mut results = Vec::new();
        for _ in 0..1000 {
            results.push(serde_json::json!({ "result": "changed", "changed": true }));
        }
        let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[
                ("x-knotrel-state-version", "1000"),
                ("x-knotrel-instance-id", "mock-preamble"),
            ],
            &body,
        );
    }

    // 4. Post-setup GET /v1/stats
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[("x-knotrel-instance-id", "mock-preamble")],
            r#"{"node_count":1000,"edge_count":0,"state_version":"1000"}"#,
        );
    }

    // 5. Warmup POST /v1/batch (50 connected)
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        let mut results = Vec::new();
        for _ in 0..50 {
            results.push(serde_json::json!({ "result": "connected", "connected": false }));
        }
        let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[
                ("x-knotrel-state-version", "1000"),
                ("x-knotrel-instance-id", "mock-preamble"),
            ],
            &body,
        );
    }

    // 6. Post-warmup GET /v1/stats
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = read_http_request_from_stream(&mut stream);
        write_mock_http_response(
            &mut stream,
            200,
            "OK",
            &[("x-knotrel-instance-id", "mock-preamble")],
            r#"{"node_count":1000,"edge_count":0,"state_version":"1000"}"#,
        );
    }
}

#[test]
fn test_standalone_batch_timeout_on_server_accepting_but_never_responding() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_timeout_unresponsive");
    let json_out = temp_dir.path.join("unresponsive.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        if let Ok((_stream, _)) = listener.accept() {
            // Accept connection but never respond; sleep longer than 5s HTTP request timeout
            std::thread::sleep(Duration::from_millis(7000));
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(12));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out, "Driver hung beyond request deadline");
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("HTTP request timed out after 5.0s"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_standalone_batch_timeout_on_unclosed_incomplete_response() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_timeout_incomplete");
    let json_out = temp_dir.path.join("incomplete.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        use std::io::Write;
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request_from_stream(&mut stream);
            // Send partial response with Content-Length: 100, but only 10 bytes and leave connection open
            let partial = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nContent-Type: application/json\r\n\r\n0123456789";
            let _ = stream.write_all(partial.as_bytes());
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(7000));
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(12));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out, "Driver hung beyond request deadline");
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("HTTP request timed out after 5.0s"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_standalone_batch_strict_state_validation_rejections() {
    let example = find_example_measure_e2e();

    // 1. /v1/info returns HTTP 500
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mock_addr = listener.local_addr().expect("local addr");
        let mock_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    500,
                    "Internal Server Error",
                    &[],
                    r#"{"error":"fail"}"#,
                );
            }
        });
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &mock_addr.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        let _ = mock_thread.join();
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("Failed to query /v1/info from server at")
                && stderr.contains("expected 200 OK"),
            "stderr: {stderr}"
        );
    }

    // 2. /v1/info returns missing engine
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mock_addr = listener.local_addr().expect("local addr");
        let mock_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(&mut stream, 200, "OK", &[], r#"{"experimental":false}"#);
            }
        });
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &mock_addr.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        let _ = mock_thread.join();
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("missing or non-string 'engine' field"),
            "stderr: {stderr}"
        );
    }

    // 3. /v1/stats returns HTTP 500 before setup
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mock_addr = listener.local_addr().expect("local addr");
        let mock_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    200,
                    "OK",
                    &[],
                    r#"{"engine":"compact-bfs","experimental":false}"#,
                );
            }
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    500,
                    "Internal Server Error",
                    &[],
                    r#"{"error":"stats failed"}"#,
                );
            }
        });
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &mock_addr.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        let _ = mock_thread.join();
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("Failed to query /v1/stats from server at")
                && stderr.contains("expected 200 OK"),
            "stderr: {stderr}"
        );
    }

    // 4. /v1/stats returns missing node_count
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mock_addr = listener.local_addr().expect("local addr");
        let mock_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    200,
                    "OK",
                    &[],
                    r#"{"engine":"compact-bfs","experimental":false}"#,
                );
            }
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    200,
                    "OK",
                    &[("x-knotrel-instance-id", "mock-inst")],
                    r#"{"edge_count":0,"state_version":"0"}"#,
                );
            }
        });
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &mock_addr.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        let _ = mock_thread.join();
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("missing or non-integer 'node_count' field"),
            "stderr: {stderr}"
        );
    }

    // 5. Missing instance identity in both body and headers
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let mock_addr = listener.local_addr().expect("local addr");
        let mock_thread = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    200,
                    "OK",
                    &[],
                    r#"{"engine":"compact-bfs","experimental":false}"#,
                );
            }
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_http_request_from_stream(&mut stream);
                write_mock_http_response(
                    &mut stream,
                    200,
                    "OK",
                    &[],
                    r#"{"node_count":0,"edge_count":0,"state_version":"0"}"#,
                );
            }
        });
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--addr",
            &mock_addr.to_string(),
        ]);
        let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
        let _ = mock_thread.join();
        assert!(!w_out.output.status.success());
        let stderr = String::from_utf8_lossy(&w_out.output.stderr);
        assert!(
            stderr.contains("Missing instance identity"),
            "stderr: {stderr}"
        );
    }
}

#[test]
fn test_standalone_batch_measured_loop_rejection_of_incorrect_result() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_measured_inc_res");
    let json_out = temp_dir.path.join("incorrect_result.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        handle_mock_batch_preamble(&listener);

        // Request 7: First measured batch window request (batch_size=64)
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request_from_stream(&mut stream);
            // Op 0 is link(u, v) expecting changed: true; return changed: false
            let mut results = Vec::new();
            results.push(serde_json::json!({ "result": "changed", "changed": false }));
            for _ in 1..64 {
                results.push(serde_json::json!({ "result": "connected", "connected": true }));
            }
            let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
            write_mock_http_response(
                &mut stream,
                200,
                "OK",
                &[("x-knotrel-state-version", "1016")],
                &body,
            );
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--batch-size",
        "64",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out);
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("Batch 0 op 0: expected changed=true, got false"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_standalone_batch_measured_loop_rejection_of_missing_results() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_measured_missing");
    let json_out = temp_dir.path.join("missing_results.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        handle_mock_batch_preamble(&listener);

        // Request 7: Return 63 items instead of 64
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request_from_stream(&mut stream);
            let mut results = Vec::new();
            for _ in 0..63 {
                results.push(serde_json::json!({ "result": "changed", "changed": true }));
            }
            let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
            write_mock_http_response(
                &mut stream,
                200,
                "OK",
                &[("x-knotrel-state-version", "1016")],
                &body,
            );
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--batch-size",
        "64",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out);
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("Batch request 0 results length 63 != expected operations 64"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_standalone_batch_measured_loop_rejection_of_domain_error() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_measured_domain_err");
    let json_out = temp_dir.path.join("domain_error.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        handle_mock_batch_preamble(&listener);

        // Request 7: Return HTTP 422 with domain error
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request_from_stream(&mut stream);
            let body =
                r#"{"result":"error","code":"invalid_request","message":"simulated domain error"}"#;
            write_mock_http_response(&mut stream, 422, "Unprocessable Entity", &[], body);
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--batch-size",
        "64",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out);
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("Batch request 0 failed with HTTP status 422"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_standalone_batch_measured_loop_rejection_of_incorrect_revision() {
    let example = find_example_measure_e2e();
    let temp_dir = TempTestDir::new("knotrel_measured_bad_rev");
    let json_out = temp_dir.path.join("bad_revision.json");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let mock_addr = listener.local_addr().expect("local addr");

    let mock_thread = std::thread::spawn(move || {
        handle_mock_batch_preamble(&listener);

        // Request 7: Return correct outcomes, but wrong revision progression in header
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = read_http_request_from_stream(&mut stream);
            // 64 ops = 16 rounds of (link, conn, cut, conn). 16 * 2 = 32 changes. Expected version = 1032.
            let mut results = Vec::new();
            for _ in 0..16 {
                results.push(serde_json::json!({ "result": "changed", "changed": true }));
                results.push(serde_json::json!({ "result": "connected", "connected": true }));
                results.push(serde_json::json!({ "result": "changed", "changed": true }));
                results.push(serde_json::json!({ "result": "connected", "connected": false }));
            }
            let body = serde_json::to_string(&serde_json::json!({ "results": results })).unwrap();
            write_mock_http_response(
                &mut stream,
                200,
                "OK",
                &[("x-knotrel-state-version", "1000")], // Wrong! (expected 1032)
                &body,
            );
        }
    });

    let mut cmd = Command::new(&example);
    cmd.args([
        "--scenario",
        "standalone-batch",
        "--batch-size",
        "64",
        "--addr",
        &mock_addr.to_string(),
        "--json-out",
        json_out.to_str().unwrap(),
    ]);

    let w_out = run_with_watchdog(cmd, Duration::from_secs(10));
    let _ = mock_thread.join();

    assert!(!w_out.timed_out);
    assert!(!w_out.output.status.success());
    let stderr = String::from_utf8_lossy(&w_out.output.stderr);
    assert!(
        stderr.contains("header version '1000' != expected arithmetic version '1032'"),
        "stderr was: {stderr}"
    );
    assert!(!json_out.exists());
}

#[test]
fn test_batch_warmup_chunking_preserves_work_and_revision() {
    let example = find_example_measure_e2e();
    let temp = TempTestDir::new("warmup_chunking");
    for size in [1, 8, 50] {
        let out = temp.path.join(format!("{size}.json"));
        let mut cmd = Command::new(&example);
        cmd.args([
            "--scenario",
            "standalone-batch",
            "--batch-size",
            "256",
            "--batch-warmup-chunk-size",
            &size.to_string(),
            "--spawn-server",
            "--json-out",
            out.to_str().unwrap(),
        ]);
        let result = run_with_watchdog(cmd, Duration::from_secs(45));
        assert!(
            result.output.status.success(),
            "{}",
            String::from_utf8_lossy(&result.output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
        assert_eq!(report["configuration"]["warmup_chunk_size"], size);
        let run = &report["runs"][0];
        assert_eq!(run["warmup"]["request_count"], 50_usize.div_ceil(size));
        assert_eq!(run["warmup"]["query_count"], 50);
        assert_eq!(run["warmup"]["post_warmup_state_version"], "1000");
        assert_eq!(run["verification"]["actual_final_state_version"], "2000");
        assert_eq!(run["verification"]["total_verified_items"], 2000);
        assert_eq!(run["request_latency"]["total_requests"], 8);
    }
}

#[test]
fn test_batch_warmup_option_rejects_invalid_configuration() {
    let example = find_example_measure_e2e();
    for args in [
        vec![
            "--scenario",
            "standalone-batch",
            "--batch-warmup-chunk-size",
            "0",
        ],
        vec![
            "--scenario",
            "standalone-batch",
            "--batch-warmup-chunk-size",
            "51",
        ],
        vec!["--batch-warmup-chunk-size", "50"],
    ] {
        let result = Command::new(&example).args(args).output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("--batch-warmup-chunk-size"));
    }
}

#[test]
fn test_empty_http_response_identifies_request() {
    let example = find_example_measure_e2e();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let address = listener.local_addr().expect("local address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let _ = read_http_request_from_stream(&mut stream);
    });
    let mut command = Command::new(example);
    command.args([
        "--scenario",
        "standalone-batch",
        "--addr",
        &address.to_string(),
    ]);
    let result = run_with_watchdog(command, Duration::from_secs(10));
    server.join().expect("mock server thread");
    assert!(!result.timed_out);
    assert!(!result.output.status.success());
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert!(stderr.contains("GET /v1/info"), "stderr was: {stderr}");
    assert!(
        stderr.contains(&address.to_string()),
        "stderr was: {stderr}"
    );
}
