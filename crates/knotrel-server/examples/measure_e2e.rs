//! End-to-End Reproducible Measurements for Knotrel Feature 0.2.6 & Tooling Increment 2 (Feature M3.1).
//!
//! Evaluates the external feeder lifecycle and standalone `/v1/batch` scenarios against a real `knotrel-server` process:
//! - Lifecycle scenario: atomic snapshot import, ordered catch-up event streaming, Time-to-Ready, query latency, BFS verification, RSS sampling, and rebuild recovery.
//! - Standalone batch scenario (`--scenario standalone-batch`): 1,000 isolated nodes, 2,000 deterministic logical operations (500 links, 500 cuts, 1,000 queries) in 500 reversible rounds (ADR 0013 contract).
//! - Repeated-run benchmark support (`--runs <N>`) with statistical aggregation.
//! - Explicit engine selection (`--engine <compact-bfs|ett|hdt>`) forwarded to spawned servers and verified via `/v1/info`.
//! - Explicit server binary path (`--server-bin <PATH>`) for targeting specific builds without path auto-discovery.
//! - Lossless machine-readable JSON output (`--json-out <PATH>`) with integer nanosecond precision and timing boundaries (`measure_e2e.v1` and `measure_e2e.batch.v1`).
//!
//! Standalone warmup uses 50 read-only queries. `--batch-warmup-chunk-size <1..50>`
//! changes only their untimed request grouping (default 50: one request). The JSON
//! records chunk size and request count; measured work and connection policy stay fixed.
//!
//! # Build & Run Instructions
//!
//! Fast bounded smoke run (debug build, unoptimized):
//! ```bash
//! cargo run --example measure_e2e -- --dataset small --spawn-server
//! ```
//!
//! Standalone batch measurement run:
//! ```bash
//! cargo run --release --example measure_e2e -- \
//!   --scenario standalone-batch \
//!   --batch-size 64 \
//!   --spawn-server \
//!   --engine compact-bfs \
//!   --runs 3 \
//!   --json-out evidence/batch_64.json
//! ```
//!
//! Multi-engine lifecycle benchmark with explicit server binary and JSON output:
//! ```bash
//! cargo run --release --example measure_e2e -- \
//!   --spawn-server \
//!   --server-bin target/release/knotrel-server \
//!   --engine ett \
//!   --dataset small \
//!   --runs 3 \
//!   --json-out evidence/ett_small.json
//! ```

#[path = "../tests/common/feeder.rs"]
mod feeder;

use feeder::{ExternalFeederClient, ExternalSourceFixture, ExternalSourceOracle, HttpResponse};
use serde::Serialize;
use std::{
    io::Read,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Supported server engine selectors for performance qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EngineSelector {
    /// Default compact BFS engine.
    CompactBfs,
    /// Experimental Euler-Tour forest engine.
    EulerTour,
    /// Experimental Holm-de Lichtenberg-Thorup hierarchical levels engine.
    Hdt,
}

impl EngineSelector {
    /// Parses an engine string into an [`EngineSelector`], accepting exact identifiers.
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact-bfs" => Ok(Self::CompactBfs),
            "ett" => Ok(Self::EulerTour),
            "hdt" => Ok(Self::Hdt),
            other => Err(format!(
                "Error: invalid engine '{other}'; expected one of: compact-bfs, ett, hdt"
            )),
        }
    }

    /// Returns the canonical CLI and environment string for this engine.
    fn as_str(&self) -> &'static str {
        match self {
            Self::CompactBfs => "compact-bfs",
            Self::EulerTour => "ett",
            Self::Hdt => "hdt",
        }
    }
}

impl std::fmt::Display for EngineSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Supported measurement scenario choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Scenario {
    /// Source-managed external feeder lifecycle scenario (Feature 0.2.6).
    Lifecycle,
    /// Standalone HTTP batch measurement scenario (POST /v1/batch, Feature M3.1).
    StandaloneBatch,
}

impl Scenario {
    /// Parses a scenario string into a [`Scenario`].
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lifecycle" => Ok(Self::Lifecycle),
            "standalone-batch" | "batch" => Ok(Self::StandaloneBatch),
            other => Err(format!(
                "Error: invalid scenario '{other}'; expected one of: lifecycle, standalone-batch"
            )),
        }
    }

    /// Returns the canonical string representation for this scenario.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::StandaloneBatch => "standalone-batch",
        }
    }
}

/// Bounded ring buffer retaining up to `max_bytes` of child process stderr diagnostics
/// as a bounded, best-effort tail.
///
/// Note: The 15 ms delay after child exit does not guarantee complete stderr capture
/// unless child completion and pipe closure are explicitly synchronized.
struct BoundedStderrBuffer {
    buffer: Vec<u8>,
    max_bytes: usize,
    total_bytes_read: usize,
    truncated: bool,
}

impl BoundedStderrBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(max_bytes.min(8192)),
            max_bytes,
            total_bytes_read: 0,
            truncated: false,
        }
    }

    fn push_bytes(&mut self, chunk: &[u8]) {
        self.total_bytes_read += chunk.len();
        if self.buffer.len() + chunk.len() <= self.max_bytes {
            self.buffer.extend_from_slice(chunk);
        } else if chunk.len() >= self.max_bytes {
            self.buffer.clear();
            self.buffer
                .extend_from_slice(&chunk[chunk.len() - self.max_bytes..]);
            self.truncated = true;
        } else {
            let overflow = (self.buffer.len() + chunk.len()) - self.max_bytes;
            self.buffer.drain(..overflow);
            self.buffer.extend_from_slice(chunk);
            self.truncated = true;
        }
    }

    fn to_string_lossy(&self) -> String {
        let text = String::from_utf8_lossy(&self.buffer);
        if self.truncated {
            format!(
                "[... earlier stderr truncated; total bytes read: {} ...]\n{}",
                self.total_bytes_read, text
            )
        } else {
            text.into_owned()
        }
    }
}

/// Asynchronously drains child stderr in a background thread to prevent pipe buffer saturation.
struct StderrDrainHandle {
    buffer: Arc<Mutex<BoundedStderrBuffer>>,
}

impl StderrDrainHandle {
    fn spawn(mut stderr: std::process::ChildStderr) -> Self {
        let buffer = Arc::new(Mutex::new(BoundedStderrBuffer::new(64 * 1024)));
        let buf_clone = buffer.clone();

        std::thread::Builder::new()
            .name("knotrel-stderr-drain".to_string())
            .spawn(move || {
                let mut chunk = [0_u8; 4096];
                loop {
                    match stderr.read(&mut chunk) {
                        Ok(0) => break, // EOF reached when child terminates or closes stderr
                        Ok(n) => {
                            if let Ok(mut buf) = buf_clone.lock() {
                                buf.push_bytes(&chunk[..n]);
                            }
                        }
                        Err(_) => break, // Pipe closed or broken
                    }
                }
            })
            .expect("spawn stderr drainer thread");

        Self { buffer }
    }

    fn get_lossy(&self) -> String {
        self.buffer
            .lock()
            .map(|b| b.to_string_lossy())
            .unwrap_or_default()
    }
}

/// Ownership phase of child server process lifecycle.
enum ChildPhase {
    /// No owned child server running or being created.
    Idle,
    /// An owned child server is actively spawning (pid known once spawn succeeds).
    Spawning { pid: Option<u32> },
    /// An owned child server is running and managed by `ServerGuard`.
    Active {
        child: Child,
        stderr_drain: Option<StderrDrainHandle>,
    },
    /// An owned child server is currently being terminated and reaped.
    Terminating { pid: u32 },
}

/// Internal state of child server lifecycle coordinator.
struct CoordinatorState {
    /// Tracks if cancellation (SIGINT/SIGTERM) was received and accepted.
    cancelled: bool,
    /// Detailed phase of child process ownership.
    phase: ChildPhase,
}

/// Coordinated lifecycle manager ensuring cancellation cannot exit while an owned
/// child is being spawned, registered, terminated, or reaped.
struct ChildLifecycleCoordinator {
    state: Mutex<CoordinatorState>,
    condvar: Condvar,
}

static COORDINATOR: ChildLifecycleCoordinator = ChildLifecycleCoordinator {
    state: Mutex::new(CoordinatorState {
        cancelled: false,
        phase: ChildPhase::Idle,
    }),
    condvar: Condvar::new(),
};

#[cfg(feature = "test-support")]
mod test_hooks {
    use std::time::Duration;

    pub fn on_cancellation_accepted() {
        if let Ok(cancel_path) = std::env::var("KNOTREL_TEST_HOOK_CANCEL_WITNESS") {
            let _ = std::fs::write(&cancel_path, format!("cancelled\n{}\n", std::process::id()));
        }
    }

    pub fn trigger_test_hook(env_var: &str, pid: u32) {
        if let Ok(hook_spec) = std::env::var(env_var) {
            let parts: Vec<&str> = hook_spec.split(':').collect();
            if parts.len() >= 2 {
                let witness_path = parts[0];
                let resume_path = parts[1];
                // Write witness file with driver PID and child PID
                let _ = std::fs::write(witness_path, format!("{}\n{}\n", std::process::id(), pid));
                // Await resume signal file with a bounded deadline (e.g. 30 seconds for test safety)
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                while std::time::Instant::now() < deadline {
                    if std::path::Path::new(resume_path).exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            }
        }
    }
}

#[cfg(not(feature = "test-support"))]
mod test_hooks {
    #[inline(always)]
    pub fn on_cancellation_accepted() {}

    #[inline(always)]
    pub fn trigger_test_hook(_env_var: &str, _pid: u32) {}
}

impl ChildLifecycleCoordinator {
    /// Spawns an owned child server process under coordinated lifecycle guarantees:
    /// 1. If cancellation was already accepted, immediately refuses to spawn any child.
    /// 2. Sets phase to `Spawning { pid: None }` before spawning, ensuring cancellation handler will not exit
    ///    without coordinating with the in-flight spawn.
    /// 3. Spawns child process and records child PID.
    /// 4. Executes isolated test hook (if enabled in test-support build).
    /// 5. If cancellation arrived during spawn/hook, immediately terminates and reaps the newly spawned child.
    /// 6. Otherwise, transitions directly to `Active`, registers the child and drainer, and returns `ServerGuard`.
    fn spawn_server(
        &self,
        address: SocketAddr,
        engine: EngineSelector,
        server_bin: Option<&Path>,
        mode: &str,
    ) -> Result<ServerGuard, Box<dyn std::error::Error>> {
        let binary = if let Some(p) = server_bin {
            p.to_path_buf()
        } else {
            find_server_binary()?
        };

        if !binary.exists() {
            return Err(format!("Server binary '{}' does not exist", binary.display()).into());
        }

        // 1. Enter Spawning phase under mutex
        {
            let mut state = self.state.lock().unwrap();
            if state.cancelled {
                drop(state);
                loop {
                    std::thread::park();
                }
            }
            state.phase = ChildPhase::Spawning { pid: None };
        }

        // 2. Prepare command
        let mut cmd = Command::new(&binary);
        cmd.env_clear()
            .env("KNOTREL_ADDR", address.to_string())
            .env("KNOTREL_MODE", mode)
            .env("KNOTREL_ENGINE", engine.as_str())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        // 3. Spawn child process
        let spawn_result = cmd.spawn().map_err(|e| {
            format!(
                "failed to spawn knotrel-server binary from '{}': {e}\nPlease build with: cargo build --bin knotrel-server --example measure_e2e",
                binary.display()
            )
        });

        let mut child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                let mut state = self.state.lock().unwrap();
                state.phase = ChildPhase::Idle;
                self.condvar.notify_all();
                return Err(e.into());
            }
        };

        let pid = child.id();

        // Update Spawning phase with known child PID before running test hook
        {
            let mut state = self.state.lock().unwrap();
            state.phase = ChildPhase::Spawning { pid: Some(pid) };
        }

        // Deterministic test hook: allows test to observe spawn before registration completes
        test_hooks::trigger_test_hook("KNOTREL_TEST_HOOK_POST_SPAWN", pid);

        // 4. Re-acquire lock to transition to Active or reap if cancelled
        let mut state = self.state.lock().unwrap();
        if state.cancelled {
            // Cancellation arrived while spawn was in flight or during hook!
            // Terminate and reap immediately to guarantee no orphan is left:
            let _ = child.kill();
            let _ = child.wait();
            state.phase = ChildPhase::Idle;
            self.condvar.notify_all();
            drop(state);
            // Park caller thread so handle_cancellation exits with the appropriate signal exit code (130/143)
            loop {
                std::thread::park();
            }
        }

        let stderr_drain = child.stderr.take().map(StderrDrainHandle::spawn);
        state.phase = ChildPhase::Active {
            child,
            stderr_drain,
        };
        self.condvar.notify_all();

        Ok(ServerGuard)
    }

    /// Terminates and reaps the active child process under coordinated lifecycle guarantees:
    /// Transitions through `Terminating { pid }` so cancellation cannot exit during reaping.
    fn terminate_and_reap(&self) {
        let mut state = self.state.lock().unwrap();

        // Extract active child, if any
        let (mut child, _stderr_drain) = match std::mem::replace(&mut state.phase, ChildPhase::Idle)
        {
            ChildPhase::Active {
                child,
                stderr_drain,
            } => (child, stderr_drain),
            other => {
                state.phase = other;
                return;
            }
        };

        let pid = child.id();
        state.phase = ChildPhase::Terminating { pid };
        // Drop lock during hook and kill/wait so signal handler coordinates via condvar without deadlocking
        drop(state);

        // Deterministic test hook: allows test to observe termination transition
        test_hooks::trigger_test_hook("KNOTREL_TEST_HOOK_TERMINATING", pid);

        let _ = child.kill();
        let _ = child.wait();

        let mut state = self.state.lock().unwrap();
        state.phase = ChildPhase::Idle;
        self.condvar.notify_all();
    }

    /// Checks whether the active child process has exited prematurely.
    fn try_wait(&self) -> Result<Option<std::process::ExitStatus>, String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        match &mut state.phase {
            ChildPhase::Active { child, .. } => child
                .try_wait()
                .map_err(|e| format!("Failed to query child status: {e}")),
            _ => Ok(None),
        }
    }

    /// Returns the captured stderr diagnostics from the active drainer.
    fn captured_stderr(&self) -> String {
        let state = self.state.lock().unwrap();
        match &state.phase {
            ChildPhase::Active { stderr_drain, .. } => stderr_drain
                .as_ref()
                .map(|d| d.get_lossy())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Returns the PID of the owned child process, if any.
    fn pid(&self) -> Option<u32> {
        let state = self.state.lock().unwrap();
        match &state.phase {
            ChildPhase::Active { child, .. } => Some(child.id()),
            ChildPhase::Terminating { pid } => Some(*pid),
            ChildPhase::Spawning { pid: Some(pid) } => Some(*pid),
            _ => None,
        }
    }

    /// Returns whether cancellation has been received and accepted.
    fn is_cancelled(&self) -> bool {
        self.state.lock().unwrap().cancelled
    }

    /// Handles cancellation (SIGINT/SIGTERM) with strict lifecycle coordination:
    /// - Marks `cancelled = true` (preventing any subsequent child spawns).
    /// - Coordinates with any in-flight `Spawning` or `Terminating` transitions, waiting for them to resolve.
    /// - If an active child exists, terminates via SIGKILL, reaps via wait, and preserves diagnostics.
    /// - If no child was owned, terminates nothing (preserves external servers).
    /// - Exits with status 130 or 143.
    fn handle_cancellation(&self, sig_name: &'static str) -> ! {
        eprintln!("\nmeasure_e2e: received {sig_name}; terminating owned server and exiting...");

        let mut state = self.state.lock().unwrap();
        state.cancelled = true;

        // Record witness that cancellation was accepted if configured
        test_hooks::on_cancellation_accepted();

        // Coordinate: wait for in-flight Spawning or Terminating transitions to complete.
        // We do not discard Spawning/Terminating state or time out here, ensuring every
        // created child is terminated and reaped before process exit.
        while matches!(
            state.phase,
            ChildPhase::Spawning { .. } | ChildPhase::Terminating { .. }
        ) {
            if let ChildPhase::Terminating { pid } = state.phase {
                // Issue SIGKILL as a fail-safe to accelerate child exit
                let _ = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            } else if let ChildPhase::Spawning { pid: Some(pid) } = state.phase {
                // Child has already been spawned at the OS level; kill it so it terminates promptly
                let _ = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            state = self.condvar.wait(state).unwrap();
        }

        // If an Active child remains, terminate and reap it directly:
        if let ChildPhase::Active {
            mut child,
            stderr_drain,
        } = std::mem::replace(&mut state.phase, ChildPhase::Idle)
        {
            let pid = child.id();
            let _ = child.kill();
            match child.wait() {
                Ok(status) => {
                    eprintln!(
                        "measure_e2e: owned child server (PID {pid}) terminated with {status}."
                    );
                }
                Err(e) => {
                    eprintln!("Warning: failed waiting on child process {pid}: {e}");
                }
            }
            if let Some(drain) = stderr_drain {
                let diag = drain.get_lossy();
                if !diag.trim().is_empty() {
                    eprintln!(
                        "measure_e2e: preserved child stderr diagnostics:\n{}",
                        diag.trim()
                    );
                }
            }
        } else {
            eprintln!("measure_e2e: no owned child server process active.");
        }

        state.phase = ChildPhase::Idle;
        self.condvar.notify_all();
        drop(state);

        let code = match sig_name {
            "SIGINT" => 130,
            "SIGTERM" => 143,
            _ => 1,
        };
        std::process::exit(code);
    }
}

/// Process guard representing caller-level ownership of an active child server process.
///
/// Terminates and reaps the child process on drop or explicit kill.
struct ServerGuard;

impl ServerGuard {
    fn pid(&self) -> Option<u32> {
        COORDINATOR.pid()
    }

    fn try_wait(&self) -> Result<Option<std::process::ExitStatus>, String> {
        COORDINATOR.try_wait()
    }

    fn captured_stderr(&self) -> String {
        COORDINATOR.captured_stderr()
    }

    fn kill(&mut self) {
        COORDINATOR.terminate_and_reap();
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        COORDINATOR.terminate_and_reap();
    }
}

/// Installs cancellation signal listeners (SIGINT and SIGTERM on Unix; Ctrl+C on non-Unix).
#[cfg(unix)]
fn install_cancellation_handler() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: failed to install SIGINT handler: {e}");
            return;
        }
    };
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: failed to install SIGTERM handler: {e}");
            return;
        }
    };

    tokio::spawn(async move {
        let sig_name = tokio::select! {
            _ = sigint.recv() => "SIGINT",
            _ = sigterm.recv() => "SIGTERM",
        };
        COORDINATOR.handle_cancellation(sig_name);
    });
}

#[cfg(not(unix))]
fn install_cancellation_handler() {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            COORDINATOR.handle_cancellation("SIGINT");
        }
    });
}

/// Obtains an available loopback port by temporarily binding port 0.
///
/// Note: This does not guarantee an atomic reservation against concurrent binders.
fn get_free_address() -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Failed to bind ephemeral loopback socket: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read ephemeral socket address: {e}"))?;
    drop(listener);
    Ok(addr)
}

/// Discovers the default server binary location when `--server-bin` is not specified.
fn find_server_binary() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("KNOTREL_BIN") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop(); // pop measure_e2e
        if exe.ends_with("examples") {
            exe.pop();
        }
        let candidate = exe.join(if cfg!(windows) {
            "knotrel-server.exe"
        } else {
            "knotrel-server"
        });
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    let fallback = if cfg!(debug_assertions) {
        PathBuf::from("target/debug/knotrel-server")
    } else {
        PathBuf::from("target/release/knotrel-server")
    };
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(format!(
        "Server executable not found at '{}'. Specify explicitly via --server-bin <PATH> or compile with:\n  cargo build --bin knotrel-server --example measure_e2e",
        fallback.display()
    ))
}

/// Computes the SHA-256 hex digest of a file using pure Rust (FIPS 180-4 standard).
fn compute_file_sha256(path: &Path) -> Result<String, std::io::Error> {
    let data = std::fs::read(path)?;
    Ok(sha256_digest(&data))
}

/// Pure Rust SHA-256 digest computation avoiding external dependencies.
fn sha256_digest(data: &[u8]) -> String {
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
        h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]
    )
}

/// Spawns a child server process with strict environment clearing and coordinated lifecycle guarantees.
fn spawn_server(
    address: SocketAddr,
    engine: EngineSelector,
    server_bin: Option<&Path>,
    mode: &str,
) -> Result<ServerGuard, Box<dyn std::error::Error>> {
    COORDINATOR.spawn_server(address, engine, server_bin, mode)
}

/// Waits for a spawned child server to begin listening on the designated address,
/// actively monitoring for premature process termination, enforcing a single overall
/// startup deadline across all asynchronous operations, and surfacing stderr diagnostics.
///
/// Diagnostics represent a bounded, best-effort tail (retaining up to 64 KiB).
/// The 15 ms delay after observing child exit allows the drainer thread a brief window to read
/// trailing pipe contents, but does not guarantee complete capture unless child completion and
/// pipe closure are explicitly synchronized.
async fn wait_server_listening(
    guard: &ServerGuard,
    address: SocketAddr,
    timeout_dur: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout_dur;

    let probe_operation = async {
        loop {
            // 1. Inspect whether child exited prematurely
            match guard.try_wait() {
                Ok(Some(status)) => {
                    // Allow drainer thread a brief moment to process any trailing piped output.
                    // Note: This 15 ms delay is a bounded, best-effort window and does not guarantee
                    // complete stderr capture if the child exited abruptly before flushing.
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    let diag = guard.captured_stderr();
                    return Err(format!(
                        "Server process exited prematurely with status {status}. Stderr diagnostics: {}",
                        diag.trim()
                    ));
                }
                Ok(None) => {}
                Err(e) => return Err(e),
            }

            // 2. Bound each probe attempt so connect/write/read cannot hang or block past deadline
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            let attempt_dur = Duration::from_millis(200).min(remaining);

            let probe_success = tokio::time::timeout(attempt_dur, async {
                match tokio::net::TcpStream::connect(address).await {
                    Ok(mut stream) => {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let req = format!(
                            "GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
                        );
                        let mut buf = [0_u8; 256];
                        stream.write_all(req.as_bytes()).await.is_ok()
                            && matches!(
                                stream.read(&mut buf).await,
                                Ok(n) if n > 0 && String::from_utf8_lossy(&buf[..n]).contains("200 OK")
                            )
                    }
                    Err(_) => false,
                }
            })
            .await
            .unwrap_or(false);

            if probe_success {
                return Ok(());
            }

            // Brief backoff before next check, bounded by deadline
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let diag = guard.captured_stderr();
        Err(format!(
            "Timed out after {}s waiting for server to listen on {address}. Stderr diagnostics: {}",
            timeout_dur.as_secs(),
            diag.trim()
        ))
    };

    match tokio::time::timeout_at(deadline, probe_operation).await {
        Ok(res) => res,
        Err(_) => {
            let diag = guard.captured_stderr();
            Err(format!(
                "Timed out after {}s waiting for server to listen on {address}. Stderr diagnostics: {}",
                timeout_dur.as_secs(),
                diag.trim()
            ))
        }
    }
}

fn get_process_rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        text.trim().parse::<u64>().ok()
    } else {
        None
    }
}

/// Spawns a background task polling RSS every 25ms to capture a sampled maximum process RSS.
///
/// Note: This is a sampled maximum across 25ms polling intervals, not a guaranteed absolute process peak;
/// brief transient allocations between sampling intervals may not be captured.
fn start_rss_monitor(
    pid: u32,
) -> (
    Arc<AtomicU64>,
    Arc<AtomicU64>,
    tokio::sync::oneshot::Sender<()>,
) {
    let peak_rss_kib = Arc::new(AtomicU64::new(0));
    let last_rss_kib = Arc::new(AtomicU64::new(0));
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

    let peak_clone = peak_rss_kib.clone();
    let last_clone = last_rss_kib.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(25));
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = interval.tick() => {
                    if let Some(rss) = get_process_rss_kib(pid) {
                        last_clone.store(rss, Ordering::Relaxed);
                        peak_clone.fetch_max(rss, Ordering::Relaxed);
                    }
                }
            }
        }
    });

    (peak_rss_kib, last_rss_kib, stop_tx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatasetTier {
    Small,
    Medium,
    Large,
}

impl DatasetTier {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    fn params(&self) -> (usize, usize, usize, usize) {
        match self {
            Self::Small => (1_000, 2_000, 500, 1_000),
            Self::Medium => (10_000, 20_000, 2_000, 2_500),
            Self::Large => (50_000, 100_000, 5_000, 5_000),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

/// Statistics calculated over discrete query latency samples, preserving integer nanoseconds.
#[derive(Debug, Clone, Serialize)]
struct LatencyStatsRecord {
    min_ns: u64,
    mean_ns: f64,
    median_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,

    min_us: f64,
    mean_us: f64,
    median_us: f64,
    p95_us: f64,
    p99_us: f64,
    max_us: f64,
}

fn calculate_latency_stats(mut samples: Vec<Duration>) -> LatencyStatsRecord {
    if samples.is_empty() {
        return LatencyStatsRecord {
            min_ns: 0,
            mean_ns: 0.0,
            median_ns: 0,
            p95_ns: 0,
            p99_ns: 0,
            max_ns: 0,
            min_us: 0.0,
            mean_us: 0.0,
            median_us: 0.0,
            p95_us: 0.0,
            p99_us: 0.0,
            max_us: 0.0,
        };
    }
    samples.sort();
    let count = samples.len();
    let sum_ns: u128 = samples.iter().map(|d| d.as_nanos()).sum();
    let mean_ns = sum_ns as f64 / count as f64;
    let mean_us = mean_ns / 1_000.0;

    let p50_idx = ((count as f64) * 0.50).min((count - 1) as f64) as usize;
    let p95_idx = ((count as f64) * 0.95).min((count - 1) as f64) as usize;
    let p99_idx = ((count as f64) * 0.99).min((count - 1) as f64) as usize;

    let min_ns = samples[0].as_nanos() as u64;
    let median_ns = samples[p50_idx].as_nanos() as u64;
    let p95_ns = samples[p95_idx].as_nanos() as u64;
    let p99_ns = samples[p99_idx].as_nanos() as u64;
    let max_ns = samples[count - 1].as_nanos() as u64;

    LatencyStatsRecord {
        min_ns,
        mean_ns,
        median_ns,
        p95_ns,
        p99_ns,
        max_ns,
        min_us: min_ns as f64 / 1_000.0,
        mean_us,
        median_us: median_ns as f64 / 1_000.0,
        p95_us: p95_ns as f64 / 1_000.0,
        p99_us: p99_ns as f64 / 1_000.0,
        max_us: max_ns as f64 / 1_000.0,
    }
}

/// Raw timing and correctness observations collected from a single benchmark iteration.
#[derive(Debug, Clone)]
struct RunMetrics {
    run_index: usize,
    seed: u64,
    snap_duration: Duration,
    snap_elements: usize,
    snap_elements_per_sec: f64,
    catchup_duration: Duration,
    catchup_events: usize,
    batch_count: usize,
    catchup_events_per_sec: f64,
    barrier_duration: Duration,
    total_sync_duration: Duration,
    total_query_duration: Duration,
    query_qps: f64,
    latency: LatencyStatsRecord,
    oracle_agreement_pct: f64,
    verified_queries: usize,
    total_queries: usize,
    sampled_rss_kib: u64,
    peak_rss_kib: u64,
    recovery_duration: Option<Duration>,
}

impl RunMetrics {
    fn snap_duration_ms(&self) -> f64 {
        self.snap_duration.as_secs_f64() * 1000.0
    }
    fn catchup_duration_ms(&self) -> f64 {
        self.catchup_duration.as_secs_f64() * 1000.0
    }
    fn barrier_duration_ms(&self) -> f64 {
        self.barrier_duration.as_secs_f64() * 1000.0
    }
    fn total_time_to_ready_ms(&self) -> f64 {
        self.total_sync_duration.as_secs_f64() * 1000.0
    }
    fn recovery_duration_ms(&self) -> Option<f64> {
        self.recovery_duration.map(|d| d.as_secs_f64() * 1000.0)
    }
}

// ============================================================================
// JSON Output Schema (measure_e2e.v1)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct JsonReport {
    schema_version: &'static str,
    driver: DriverMetadata,
    server_provenance: ServerProvenance,
    configuration: RunConfiguration,
    timing_boundaries: TimingBoundariesMetadata,
    runs: Vec<JsonRunRecord>,
    summary: Option<JsonSummaryRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct DriverMetadata {
    name: &'static str,
    version: &'static str,
    build_profile: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ServerProvenance {
    binary_path: String,
    binary_sha256: Option<String>,
    instance_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct RunConfiguration {
    dataset_tier: &'static str,
    node_count: usize,
    edge_count: usize,
    event_count: usize,
    query_count: usize,
    batch_size: usize,
    seed: u64,
    runs: usize,
    requested_engine: &'static str,
    effective_engine: String,
    engine_experimental: bool,
    spawn_server: bool,
    server_address: String,
}

#[derive(Debug, Clone, Serialize)]
struct TimingBoundariesMetadata {
    units: UnitsDefinition,
    phase_definitions: PhaseDefinitions,
}

#[derive(Debug, Clone, Serialize)]
struct UnitsDefinition {
    duration_ns: &'static str,
    duration_ms: &'static str,
    latency_ns: &'static str,
    latency_us: &'static str,
    throughput_qps: &'static str,
    memory_kib: &'static str,
    memory_bytes: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct PhaseDefinitions {
    snapshot_import: &'static str,
    catch_up: &'static str,
    barrier: &'static str,
    registration_through_barrier: &'static str,
    query_latency: &'static str,
    restart_recovery: &'static str,
    sampled_rss: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct JsonRunRecord {
    run_index: usize,
    seed: u64,
    snapshot_import: JsonSnapshotRecord,
    catch_up: JsonCatchupRecord,
    barrier: JsonBarrierRecord,
    registration_through_barrier: JsonPhaseRecord,
    query: JsonQueryRecord,
    memory: JsonMemoryRecord,
    restart_recovery: Option<JsonPhaseRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct JsonPhaseRecord {
    duration_ns: u64,
    duration_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct JsonSnapshotRecord {
    duration_ns: u64,
    duration_ms: f64,
    elements_per_sec: f64,
    element_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct JsonCatchupRecord {
    duration_ns: u64,
    duration_ms: f64,
    events_per_sec: f64,
    event_count: usize,
    batch_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBarrierRecord {
    duration_ns: u64,
    duration_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct JsonQueryRecord {
    total_queries: usize,
    verified_queries: usize,
    oracle_agreement_pct: f64,
    qps: f64,
    total_duration_ns: u64,
    total_duration_ms: f64,
    latency_ns: JsonLatencyNs,
    latency_us: JsonLatencyUs,
}

#[derive(Debug, Clone, Serialize)]
struct JsonLatencyNs {
    min: u64,
    mean: f64,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

#[derive(Debug, Clone, Serialize)]
struct JsonLatencyUs {
    min: f64,
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

#[derive(Debug, Clone, Serialize)]
struct JsonMemoryRecord {
    sampled_final_rss_kib: u64,
    sampled_final_rss_bytes: u64,
    sampled_max_rss_kib: u64,
    sampled_max_rss_bytes: u64,
    sampling_interval_ms: u64,
    note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct JsonSummaryRecord {
    mean_snapshot_duration_ms: f64,
    mean_catchup_duration_ms: f64,
    mean_registration_through_barrier_ms: f64,
    mean_query_qps: f64,
    mean_latency_p50_us: f64,
    mean_latency_p95_us: f64,
    mean_sampled_final_rss_kib: f64,
    max_sampled_max_rss_kib: u64,
    mean_recovery_duration_ms: Option<f64>,
}

// ============================================================================
// Standalone Batch Workload & Measurement Types (Feature M3.1 / Tooling Incr 2)
// ============================================================================

/// Fully specified deterministic 64-bit PRNG (SplitMix64) used to generate standalone batch workloads.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Constructs a new [`SplitMix64`] with the provided seed.
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Advances state and returns the next pseudo-random `u64`.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Generates a pseudo-random integer in `0..upper`.
    fn gen_range(&mut self, upper: usize) -> usize {
        assert!(upper > 0);
        (self.next_u64() % (upper as u64)) as usize
    }
}

/// A single operation in the standalone `/v1/batch` workload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "op")]
enum BatchOp {
    #[serde(rename = "add_node")]
    AddNode { node: String },
    #[serde(rename = "link")]
    Link { source: String, target: String },
    #[serde(rename = "cut")]
    Cut { source: String, target: String },
    #[serde(rename = "connected")]
    Connected { source: String, target: String },
}

/// Precomputed ground-truth expectation for a single operation in the trace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedOutcome {
    Changed(bool),
    Connected(bool),
}

/// A standalone batch workload of 1,000 isolated nodes and 2,000 logical operations
/// (500 links, 500 cuts, 1,000 queries) organized into 500 reversible rounds.
#[derive(Debug, Clone)]
struct StandaloneBatchWorkload {
    /// 1,000 isolated node IDs created in the setup phase.
    setup_nodes: Vec<String>,
    /// Exactly 2,000 logical operations in the timed trace.
    trace: Vec<BatchOp>,
    /// Precomputed expected outcome for each of the 2,000 operations.
    expected_outcomes: Vec<ExpectedOutcome>,
}

impl StandaloneBatchWorkload {
    const NODE_COUNT: usize = 1_000;
    const ROUND_COUNT: usize = 500;
    const TOTAL_OPERATIONS: usize = 2_000;
    const TRACE_IDENTITY: &'static str = "deterministic_500_round_link_cut_reversal";

    /// Generates the deterministic standalone batch workload using seed.
    ///
    /// Generates a trace of 500 rounds of: `link(u, v)`, `connected(u, v)`, `cut(u, v)`, `connected(u, v)`.
    /// Each round uses distinct endpoints and restores the empty-edge topology.
    fn generate(seed: u64) -> Self {
        let setup_nodes: Vec<String> = (0..Self::NODE_COUNT).map(|i| i.to_string()).collect();

        let mut perm = setup_nodes.clone();
        let mut rng = SplitMix64::new(seed);
        for i in (1..perm.len()).rev() {
            let j = rng.gen_range(i + 1);
            perm.swap(i, j);
        }

        let mut trace = Vec::with_capacity(Self::TOTAL_OPERATIONS);
        let mut expected_outcomes = Vec::with_capacity(Self::TOTAL_OPERATIONS);

        for r in 0..Self::ROUND_COUNT {
            let u = perm[2 * r].clone();
            let v = perm[2 * r + 1].clone();

            // 1. link(u, v) -> changed: true
            trace.push(BatchOp::Link {
                source: u.clone(),
                target: v.clone(),
            });
            expected_outcomes.push(ExpectedOutcome::Changed(true));

            // 2. connected(u, v) -> connected: true
            trace.push(BatchOp::Connected {
                source: u.clone(),
                target: v.clone(),
            });
            expected_outcomes.push(ExpectedOutcome::Connected(true));

            // 3. cut(u, v) -> changed: true
            trace.push(BatchOp::Cut {
                source: u.clone(),
                target: v.clone(),
            });
            expected_outcomes.push(ExpectedOutcome::Changed(true));

            // 4. connected(u, v) -> connected: false
            trace.push(BatchOp::Connected {
                source: u,
                target: v,
            });
            expected_outcomes.push(ExpectedOutcome::Connected(false));
        }

        Self {
            setup_nodes,
            trace,
            expected_outcomes,
        }
    }
}

/// Raw timing, counts, and verification observations from a single standalone batch benchmark run.
#[derive(Debug, Clone)]
struct BatchRunMetrics {
    run_index: usize,
    seed: u64,
    batch_size: usize,
    setup_duration: Duration,
    created_nodes: usize,
    initial_state_version: String,
    warmup_duration: Duration,
    warmup_queries: usize,
    post_warmup_state_version: String,
    window_duration: Duration,
    total_logical_operations: usize,
    effective_mutations: usize,
    actual_requests: usize,
    request_latencies: Vec<Duration>,
    verified_items: usize,
    final_state_version: String,
    final_node_count: usize,
    final_edge_count: usize,
    sampled_final_rss_kib: u64,
    sampled_max_rss_kib: u64,
}

/// Top-level structured JSON report for standalone `/v1/batch` measurement (`measure_e2e.batch.v1`).
#[derive(Debug, Clone, Serialize)]
struct JsonBatchReport {
    schema_version: &'static str,
    driver: DriverMetadata,
    server_provenance: ServerProvenance,
    configuration: BatchRunConfiguration,
    timing_boundaries: BatchTimingBoundariesMetadata,
    runs: Vec<JsonBatchRunRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<JsonBatchSummaryRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct BatchRunConfiguration {
    scenario: &'static str,
    trace_identity: &'static str,
    workload_description: &'static str,
    topology: &'static str,
    node_count: usize,
    seed: u64,
    batch_size: usize,
    total_logical_operations: usize,
    link_operations: usize,
    cut_operations: usize,
    query_operations: usize,
    actual_request_count: usize,
    full_batches: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_batch_size: Option<usize>,
    warmup: &'static str,
    warmup_chunk_size: usize,
    connection_model: &'static str,
    runs: usize,
    requested_engine: &'static str,
    effective_engine: String,
    engine_experimental: bool,
    spawn_server: bool,
    server_address: String,
}

#[derive(Debug, Clone, Serialize)]
struct BatchTimingBoundariesMetadata {
    units: BatchUnitsDefinition,
    notes: BatchTimingNotes,
}

#[derive(Debug, Clone, Serialize)]
struct BatchUnitsDefinition {
    duration_ns: &'static str,
    duration_ms: &'static str,
    request_latency_ns: &'static str,
    request_latency_us: &'static str,
    elapsed_window_ns_per_logical_op: &'static str,
    logical_ops_per_sec: &'static str,
    requests_per_sec: &'static str,
    memory_kib: &'static str,
    memory_bytes: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct BatchTimingNotes {
    latency_concept: &'static str,
    window_concept: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchRunRecord {
    run_index: usize,
    seed: u64,
    setup: JsonBatchSetupRecord,
    warmup: JsonBatchWarmupRecord,
    measurement_window: JsonBatchWindowRecord,
    request_latency: JsonBatchRequestLatencyRecord,
    verification: JsonBatchVerificationRecord,
    memory: JsonMemoryRecord,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchSetupRecord {
    duration_ns: u64,
    duration_ms: f64,
    created_nodes: usize,
    initial_state_version: String,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchWarmupRecord {
    request_count: usize,
    duration_ns: u64,
    duration_ms: f64,
    query_count: usize,
    post_warmup_state_version: String,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchWindowRecord {
    duration_ns: u64,
    duration_ms: f64,
    total_logical_operations: usize,
    effective_mutations: usize,
    elapsed_window_ns_per_logical_op: f64,
    logical_ops_per_sec: f64,
    requests_per_sec: f64,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchRequestLatencyRecord {
    total_requests: usize,
    latency_ns: JsonLatencyNs,
    latency_us: JsonLatencyUs,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchVerificationRecord {
    all_items_matched: bool,
    total_verified_items: usize,
    expected_final_state_version: String,
    actual_final_state_version: String,
    final_node_count: usize,
    final_edge_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct JsonBatchSummaryRecord {
    mean_window_duration_ms: f64,
    mean_logical_ops_per_sec: f64,
    mean_elapsed_window_ns_per_logical_op: f64,
    mean_request_latency_p50_us: f64,
    mean_request_latency_p95_us: f64,
    mean_sampled_final_rss_kib: f64,
    max_sampled_max_rss_kib: u64,
}

/// Finite deadline applied to all standalone HTTP requests, covering TCP connect, request transmission, and response read.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Sends a direct HTTP request to a server over loopback TCP using sequential Connection: close, bounded by a finite deadline.
async fn send_http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    tokio::time::timeout(HTTP_REQUEST_TIMEOUT, async {
        send_http_request_inner(address, method, path, headers, body).await
    })
    .await
    .map_err(|_| {
        format!(
            "HTTP request timed out after {:.1}s: {method} {path} to {address}",
            HTTP_REQUEST_TIMEOUT.as_secs_f64()
        )
    })?
}

async fn send_http_request_inner(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let mut stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("failed to connect to server at {address}: {e}"))?;

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body);

    stream.write_all(req.as_bytes()).await?;

    let mut response_bytes = Vec::new();
    stream.read_to_end(&mut response_bytes).await?;

    if response_bytes.is_empty() {
        return Err("connection closed without HTTP response".into());
    }

    let head_end = response_bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response: missing header terminator")?;

    let headers_str = String::from_utf8_lossy(&response_bytes[..head_end]).to_string();
    let body_str = String::from_utf8_lossy(&response_bytes[head_end + 4..]).to_string();

    let status = headers_str
        .lines()
        .next()
        .and_then(|status_line| status_line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("malformed HTTP response: invalid or missing status code")?;

    Ok(HttpResponse {
        status,
        headers: headers_str,
        body: body_str,
    })
}

/// Request chunking for the measured trace and the fixed 50-query untimed warmup.
struct BatchRequestSizes {
    measured: usize,
    warmup: usize,
}

/// Executes a single standalone batch benchmark run against an owned or external server.
async fn execute_single_batch_measurement_run(
    run_index: usize,
    seed: u64,
    sizes: BatchRequestSizes,
    custom_addr: Option<SocketAddr>,
    should_spawn: bool,
    engine: EngineSelector,
    server_bin: Option<&Path>,
) -> Result<(BatchRunMetrics, String, String, bool), Box<dyn std::error::Error>> {
    let batch_size = sizes.measured;
    let warmup_chunk_size = sizes.warmup;
    let address = if let Some(addr) = custom_addr {
        addr
    } else if should_spawn {
        get_free_address()?
    } else {
        SocketAddr::from(([127, 0, 0, 1], 8080))
    };

    let mut server_guard: Option<ServerGuard> = None;
    let mut rss_monitor: Option<(
        Arc<AtomicU64>,
        Arc<AtomicU64>,
        tokio::sync::oneshot::Sender<()>,
    )> = None;

    if should_spawn {
        println!(
            "🚀 [Run {run_index}] Spawning knotrel-server in standalone mode ({}) on {address}...",
            engine.as_str()
        );
        let guard = spawn_server(address, engine, server_bin, "standalone")?;
        let pid = guard.pid().unwrap_or(0);

        if let Err(e) = wait_server_listening(&guard, address, Duration::from_secs(5)).await {
            return Err(format!("Server startup failed on {address}: {e}").into());
        }

        rss_monitor = Some(start_rss_monitor(pid));
        server_guard = Some(guard);
    }

    // 1. Discover Server, Verify Mode & Engine, Check Pristine State
    let info_res = send_http_request(address, "GET", "/v1/info", &[], "").await?;
    if info_res.status != 200 {
        return Err(format!(
            "Failed to query /v1/info from server at {address}: HTTP {} (expected 200 OK): {}",
            info_res.status, info_res.body
        )
        .into());
    }
    let info_json = info_res.json();
    let effective_engine = info_json
        .get("engine")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "missing or non-string 'engine' field in /v1/info response: {}",
                info_res.body
            )
        })?
        .to_string();
    let engine_experimental = info_json
        .get("experimental")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            format!(
                "missing or non-boolean 'experimental' field in /v1/info response: {}",
                info_res.body
            )
        })?;

    if let Some(mode_val) = info_json.get("mode") {
        let mode_str = mode_val.as_str().ok_or_else(|| {
            format!(
                "non-string 'mode' field in /v1/info response: {}",
                info_res.body
            )
        })?;
        if mode_str == "source_managed" {
            return Err(format!(
                "Server at {address} is running in source-managed mode; standalone batch scenario requires standalone mode"
            )
            .into());
        }
    }

    if effective_engine != engine.as_str() {
        if should_spawn {
            return Err(format!(
                "Server reported effective engine '{effective_engine}', expected requested engine '{}'",
                engine.as_str()
            )
            .into());
        } else {
            return Err(format!(
                "External server at {address} is configured with engine '{effective_engine}', but --engine '{}' was requested. The driver cannot reconfigure an externally managed server; verify external configuration or specify matching --engine.",
                engine.as_str()
            )
            .into());
        }
    }

    // Verify pristine state and extract instance identity from /v1/stats BEFORE setup mutations
    let initial_stats_res = send_http_request(address, "GET", "/v1/stats", &[], "").await?;
    if initial_stats_res.status != 200 {
        return Err(format!(
            "Failed to query /v1/stats from server at {address}: HTTP {} (expected 200 OK): {}",
            initial_stats_res.status, initial_stats_res.body
        )
        .into());
    }

    let disc_id = if let Some(id) = info_json.get("instance_id").and_then(|v| v.as_str()) {
        id.to_string()
    } else if let Some(id) = initial_stats_res.header("x-knotrel-instance-id") {
        id.to_string()
    } else {
        return Err(format!(
            "Missing instance identity: /v1/info contains no 'instance_id' and /v1/stats contains no 'x-knotrel-instance-id' header from server at {address}"
        )
        .into());
    };

    let init_stats_json = initial_stats_res.json();
    let init_node_count = init_stats_json
        .get("node_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'node_count' field in initial /v1/stats response: {}",
                initial_stats_res.body
            )
        })?;
    let init_edge_count = init_stats_json
        .get("edge_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'edge_count' field in initial /v1/stats response: {}",
                initial_stats_res.body
            )
        })?;
    let init_state_version = init_stats_json
        .get("state_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "missing or non-string 'state_version' field in initial /v1/stats response: {}",
                initial_stats_res.body
            )
        })?;

    if init_node_count != 0 || init_edge_count != 0 || init_state_version != "0" {
        return Err(format!(
            "Server at {address} is not in pristine state (node_count={init_node_count}, edge_count={init_edge_count}, state_version={init_state_version}); refusing to mutate non-pristine server"
        )
        .into());
    }

    println!(
        "✓ [Run {run_index}] Discovered server instance: {disc_id} (effective engine: {effective_engine})"
    );

    // 2. Generate Deterministic Standalone Batch Workload
    let workload_start = Instant::now();
    let workload = StandaloneBatchWorkload::generate(seed);
    let workload_dur = workload_start.elapsed();
    println!(
        "✓ [Run {run_index}] Generated batch workload in {:.2}ms (nodes={}, logical_ops={}, seed={})",
        workload_dur.as_secs_f64() * 1000.0,
        workload.setup_nodes.len(),
        workload.trace.len(),
        seed,
    );

    // 3. Setup Phase: Create 1,000 Isolated Nodes via POST /v1/batch
    let setup_start = Instant::now();
    let setup_ops: Vec<BatchOp> = workload
        .setup_nodes
        .iter()
        .map(|node_id| BatchOp::AddNode {
            node: node_id.clone(),
        })
        .collect();
    let setup_body = serde_json::to_string(&serde_json::json!({ "operations": setup_ops }))?;
    let setup_res = send_http_request(address, "POST", "/v1/batch", &[], &setup_body).await?;
    if setup_res.status != 200 {
        return Err(format!(
            "Setup failed to create 1,000 nodes: HTTP {} {}",
            setup_res.status, setup_res.body
        )
        .into());
    }
    let setup_json = setup_res.json();
    let setup_results = setup_json["results"]
        .as_array()
        .ok_or("missing 'results' in setup response")?;
    if setup_results.len() != 1000 {
        return Err(format!("Setup expected 1,000 results, got {}", setup_results.len()).into());
    }
    for (idx, r) in setup_results.iter().enumerate() {
        if r.get("result").and_then(|v| v.as_str()) != Some("changed")
            || r.get("changed").and_then(|v| v.as_bool()) != Some(true)
        {
            return Err(
                format!("Setup op {idx} (add_node) did not return changed: true: {r:?}").into(),
            );
        }
    }
    let setup_duration = setup_start.elapsed();

    // Verify initial graph state via /v1/stats
    let post_setup_stats = send_http_request(address, "GET", "/v1/stats", &[], "").await?;
    if post_setup_stats.status != 200 {
        return Err(format!(
            "Failed to query /v1/stats after setup: HTTP {} (expected 200 OK): {}",
            post_setup_stats.status, post_setup_stats.body
        )
        .into());
    }
    let stats_json = post_setup_stats.json();
    let s_nodes = stats_json
        .get("node_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'node_count' field in post-setup /v1/stats: {}",
                post_setup_stats.body
            )
        })? as usize;
    let s_edges = stats_json
        .get("edge_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'edge_count' field in post-setup /v1/stats: {}",
                post_setup_stats.body
            )
        })? as usize;
    let initial_state_version = stats_json
        .get("state_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "missing or non-string 'state_version' field in post-setup /v1/stats: {}",
                post_setup_stats.body
            )
        })?
        .to_string();
    if s_nodes != 1000 || s_edges != 0 || initial_state_version != "1000" {
        return Err(format!(
            "Unexpected state after setup: nodes={s_nodes} (expected 1000), edges={s_edges} (expected 0), version={initial_state_version} (expected 1000)"
        )
        .into());
    }
    println!(
        "✓ [Run {run_index}] Setup completed in {:.2}ms (created 1,000 nodes; initial version: {initial_state_version})",
        setup_duration.as_secs_f64() * 1000.0
    );

    // 4. Warmup Phase: Query-Only Warmup (50 queries via POST /v1/batch)
    let warmup_start = Instant::now();
    let warmup_queries_count = 50_usize;
    let warmup_ops: Vec<serde_json::Value> = (0..warmup_queries_count)
        .map(|i| {
            let u = (i * 2 % 1000).to_string();
            let v = ((i * 2 + 1) % 1000).to_string();
            serde_json::json!({ "op": "connected", "source": u, "target": v })
        })
        .collect();
    for warmup_chunk in warmup_ops.chunks(warmup_chunk_size) {
        let warmup_body =
            serde_json::to_string(&serde_json::json!({ "operations": warmup_chunk }))?;
        let warmup_res = send_http_request(address, "POST", "/v1/batch", &[], &warmup_body).await?;
        if warmup_res.status != 200 {
            return Err(format!(
                "Warmup failed: HTTP {} {}",
                warmup_res.status, warmup_res.body
            )
            .into());
        }
        let warmup_json = warmup_res.json();
        let warmup_results = warmup_json["results"]
            .as_array()
            .ok_or("missing 'results' in warmup response")?;
        if warmup_results.len() != warmup_chunk.len() {
            return Err(format!(
                "Warmup expected {} results, got {}",
                warmup_chunk.len(),
                warmup_results.len()
            )
            .into());
        }
        for (idx, r) in warmup_results.iter().enumerate() {
            if r.get("result").and_then(|v| v.as_str()) != Some("connected")
                || r.get("connected").and_then(|v| v.as_bool()) != Some(false)
            {
                return Err(
                    format!("Warmup query {idx} did not return connected: false: {r:?}").into(),
                );
            }
        }
    }
    let warmup_duration = warmup_start.elapsed();

    // Verify warmup did not mutate the graph or revision
    let post_warmup_stats = send_http_request(address, "GET", "/v1/stats", &[], "").await?;
    if post_warmup_stats.status != 200 {
        return Err(format!(
            "Failed to query /v1/stats after warmup: HTTP {} (expected 200 OK): {}",
            post_warmup_stats.status, post_warmup_stats.body
        )
        .into());
    }
    let pw_json = post_warmup_stats.json();
    let pw_nodes = pw_json
        .get("node_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'node_count' field in post-warmup /v1/stats: {}",
                post_warmup_stats.body
            )
        })? as usize;
    let pw_edges = pw_json
        .get("edge_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'edge_count' field in post-warmup /v1/stats: {}",
                post_warmup_stats.body
            )
        })? as usize;
    let post_warmup_state_version = pw_json
        .get("state_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "missing or non-string 'state_version' field in post-warmup /v1/stats: {}",
                post_warmup_stats.body
            )
        })?
        .to_string();
    if pw_nodes != 1000 || pw_edges != 0 || post_warmup_state_version != initial_state_version {
        return Err(format!(
            "Warmup mutated graph state: nodes={pw_nodes}, edges={pw_edges}, version={post_warmup_state_version} (baseline {initial_state_version})"
        )
        .into());
    }
    println!(
        "✓ [Run {run_index}] Warmup completed in {:.2}ms ({warmup_queries_count} query-only ops; version unchanged: {post_warmup_state_version})",
        warmup_duration.as_secs_f64() * 1000.0
    );

    // 5. Timed Measurement Window Phase
    let chunks: Vec<&[BatchOp]> = workload.trace.chunks(batch_size).collect();
    let expected_chunks: Vec<&[ExpectedOutcome]> =
        workload.expected_outcomes.chunks(batch_size).collect();
    let actual_requests = chunks.len();
    let partial_batch_size = if 2000 % batch_size != 0 {
        Some(2000 % batch_size)
    } else {
        None
    };

    let mut request_latencies = Vec::with_capacity(actual_requests);
    let mut verified_items = 0_usize;
    let mut current_version: u64 = initial_state_version.parse()?;
    let mut total_effective_mutations = 0_usize;

    println!(
        "⚡ [Run {run_index}] Executing measurement window (2,000 ops across {actual_requests} requests; batch_size={batch_size}, partial={partial_batch_size:?})..."
    );

    let window_start = Instant::now();

    for (chunk_idx, (chunk, exp_chunk)) in chunks.iter().zip(expected_chunks.iter()).enumerate() {
        let req_body = serde_json::to_string(&serde_json::json!({
            "operations": chunk
        }))?;

        let req_start = Instant::now();
        let resp = send_http_request(address, "POST", "/v1/batch", &[], &req_body).await?;
        let req_duration = req_start.elapsed();
        request_latencies.push(req_duration);

        // Immediate per-item correctness and error checking outside individual request latency timer
        if resp.status != 200 {
            return Err(format!(
                "Batch request {chunk_idx} failed with HTTP status {}: {}",
                resp.status, resp.body
            )
            .into());
        }

        let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Batch request {chunk_idx} returned invalid JSON: {e}"))?;
        let results = resp_json["results"]
            .as_array()
            .ok_or_else(|| format!("Batch request {chunk_idx} missing 'results' array"))?;

        if results.len() != chunk.len() {
            return Err(format!(
                "Batch request {chunk_idx} results length {} != expected operations {}",
                results.len(),
                chunk.len()
            )
            .into());
        }

        let mut chunk_effective_mutations = 0_u64;

        for (op_idx, (res, exp)) in results.iter().zip(exp_chunk.iter()).enumerate() {
            match exp {
                ExpectedOutcome::Changed(exp_changed) => {
                    if res.get("result").and_then(|v| v.as_str()) != Some("changed") {
                        return Err(format!(
                            "Batch {chunk_idx} op {op_idx}: expected 'changed', got {res:?}"
                        )
                        .into());
                    }
                    let changed = res
                        .get("changed")
                        .and_then(|v| v.as_bool())
                        .ok_or("missing 'changed' bool")?;
                    if changed != *exp_changed {
                        return Err(format!(
                            "Batch {chunk_idx} op {op_idx}: expected changed={exp_changed}, got {changed}"
                        )
                        .into());
                    }
                    if changed {
                        chunk_effective_mutations += 1;
                    }
                }
                ExpectedOutcome::Connected(exp_connected) => {
                    if res.get("result").and_then(|v| v.as_str()) != Some("connected") {
                        return Err(format!(
                            "Batch {chunk_idx} op {op_idx}: expected 'connected', got {res:?}"
                        )
                        .into());
                    }
                    let connected = res
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .ok_or("missing 'connected' bool")?;
                    if connected != *exp_connected {
                        return Err(format!(
                            "Batch {chunk_idx} op {op_idx}: expected connected={exp_connected}, got {connected}"
                        )
                        .into());
                    }
                }
            }
            verified_items += 1;
        }

        total_effective_mutations += chunk_effective_mutations as usize;
        current_version += chunk_effective_mutations;

        // Verify revision progression arithmetic from response headers
        let header_ver_str = resp.header("x-knotrel-state-version").ok_or_else(|| {
            format!("Batch request {chunk_idx} missing x-knotrel-state-version header")
        })?;
        let header_ver: u64 = header_ver_str
            .parse()
            .map_err(|e| format!("invalid header version '{header_ver_str}': {e}"))?;
        if header_ver != current_version {
            return Err(format!(
                "Batch {chunk_idx} header version '{header_ver}' != expected arithmetic version '{current_version}'"
            )
            .into());
        }
    }

    let window_duration = window_start.elapsed();

    // 6. Final State Verification
    let final_stats_res = send_http_request(address, "GET", "/v1/stats", &[], "").await?;
    if final_stats_res.status != 200 {
        return Err(format!(
            "Failed to query final /v1/stats: HTTP {} (expected 200 OK): {}",
            final_stats_res.status, final_stats_res.body
        )
        .into());
    }
    let fs_json = final_stats_res.json();
    let final_node_count = fs_json
        .get("node_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'node_count' field in final /v1/stats: {}",
                final_stats_res.body
            )
        })? as usize;
    let final_edge_count = fs_json
        .get("edge_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            format!(
                "missing or non-integer 'edge_count' field in final /v1/stats: {}",
                final_stats_res.body
            )
        })? as usize;
    let final_state_version = fs_json
        .get("state_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "missing or non-string 'state_version' field in final /v1/stats: {}",
                final_stats_res.body
            )
        })?
        .to_string();

    if final_node_count != 1000 || final_edge_count != 0 || final_state_version != "2000" {
        return Err(format!(
            "Final graph state mismatch: nodes={final_node_count} (expected 1000), edges={final_edge_count} (expected 0), version={final_state_version} (expected 2000)"
        )
        .into());
    }

    println!(
        "✓ [Run {run_index}] Measurement window completed in {:.2}ms (2,000/2,000 items verified; final version: {final_state_version})",
        window_duration.as_secs_f64() * 1000.0
    );

    let (peak_rss_kib, sampled_final_rss_kib) =
        if let Some((peak_atom, last_atom, stop_tx)) = rss_monitor {
            let _ = stop_tx.send(());
            (
                peak_atom.load(Ordering::Relaxed),
                last_atom.load(Ordering::Relaxed),
            )
        } else {
            (0, 0)
        };

    if let Some(mut guard) = server_guard {
        guard.kill();
    }

    Ok((
        BatchRunMetrics {
            run_index,
            seed,
            batch_size,
            setup_duration,
            created_nodes: 1000,
            initial_state_version,
            warmup_duration,
            warmup_queries: warmup_queries_count,
            post_warmup_state_version,
            window_duration,
            total_logical_operations: 2000,
            effective_mutations: total_effective_mutations,
            actual_requests,
            request_latencies,
            verified_items,
            final_state_version,
            final_node_count,
            final_edge_count,
            sampled_final_rss_kib,
            sampled_max_rss_kib: peak_rss_kib,
        },
        disc_id,
        effective_engine,
        engine_experimental,
    ))
}

// ============================================================================
// Execution Logic
// ============================================================================

#[allow(clippy::too_many_arguments)]
async fn execute_single_measurement_run(
    run_index: usize,
    dataset_tier: DatasetTier,
    seed: u64,
    query_count: usize,
    batch_size: usize,
    custom_addr: Option<SocketAddr>,
    should_spawn: bool,
    engine: EngineSelector,
    server_bin: Option<&Path>,
) -> Result<(RunMetrics, String, String, bool), Box<dyn std::error::Error>> {
    let (node_count, edge_count, event_count, _) = dataset_tier.params();

    let address = if let Some(addr) = custom_addr {
        addr
    } else if should_spawn {
        get_free_address()?
    } else {
        SocketAddr::from(([127, 0, 0, 1], 8080))
    };

    let mut server_guard: Option<ServerGuard> = None;
    let mut rss_monitor: Option<(
        Arc<AtomicU64>,
        Arc<AtomicU64>,
        tokio::sync::oneshot::Sender<()>,
    )> = None;

    if should_spawn {
        println!(
            "🚀 [Run {run_index}] Spawning knotrel-server ({}) on {address}...",
            engine.as_str()
        );
        let guard = spawn_server(address, engine, server_bin, "source-managed")?;
        let pid = guard.pid().unwrap_or(0);

        if let Err(e) = wait_server_listening(&guard, address, Duration::from_secs(5)).await {
            return Err(format!("Server startup failed on {address}: {e}").into());
        }

        rss_monitor = Some(start_rss_monitor(pid));
        server_guard = Some(guard);
    }

    // 1. Generate Deterministic Synthetic Fixture
    let fixture_start = Instant::now();
    let fixture = ExternalSourceFixture::synthetic(node_count, edge_count, event_count, seed);
    let fixture_dur = fixture_start.elapsed();
    println!(
        "✓ [Run {run_index}] Generated fixture in {:.2}ms (nodes={}, edges={}, events={})",
        fixture_dur.as_secs_f64() * 1000.0,
        fixture.snapshot.nodes.len(),
        fixture.snapshot.edges.len(),
        fixture.events.len(),
    );

    // 2. Build Independent Oracle for Ground-Truth Verification
    let oracle_start = Instant::now();
    let mut oracle = ExternalSourceOracle::new();
    oracle.load_snapshot(&fixture.snapshot);
    for evt in &fixture.events {
        oracle.apply_event(evt);
    }
    let oracle_dur = oracle_start.elapsed();
    println!(
        "✓ [Run {run_index}] Oracle initialized in {:.2}ms (nodes={}, edges={})",
        oracle_dur.as_secs_f64() * 1000.0,
        oracle.node_count(),
        oracle.edge_count(),
    );

    // 3. Connect Client, Discover Server & Verify Effective Engine
    let mut client = ExternalFeederClient::new(address, &format!("feeder-bench-{run_index}"));
    let disc_id = client.discover().await?;

    let info_res = client.raw_request("GET", "/v1/info", &[], "").await?;
    if info_res.status != 200 {
        return Err(format!(
            "Failed to query /v1/info from server at {address}: HTTP {} {}",
            info_res.status, info_res.body
        )
        .into());
    }
    let info_json = info_res.json();
    let effective_engine = info_json["engine"]
        .as_str()
        .ok_or("missing 'engine' field in /v1/info response")?
        .to_string();
    let engine_experimental = info_json["experimental"].as_bool().unwrap_or(false);

    if should_spawn {
        if effective_engine != engine.as_str() {
            return Err(format!(
                "Server reported effective engine '{effective_engine}', expected requested engine '{}'",
                engine.as_str()
            )
            .into());
        }
    } else if effective_engine != engine.as_str() {
        return Err(format!(
            "External server at {address} is configured with engine '{effective_engine}', but --engine '{}' was requested. The driver cannot reconfigure an externally managed server; verify external configuration or specify matching --engine.",
            engine.as_str()
        )
        .into());
    }

    println!(
        "✓ [Run {run_index}] Discovered server instance: {disc_id} (effective engine: {effective_engine})"
    );

    let total_sync_start = Instant::now();

    // 4. Session Registration & Heartbeats
    client.register_session(30_000).await?;
    client.start_heartbeat(Duration::from_millis(2_000));

    // 5. Measure Baseline Snapshot Import
    let snap_start = Instant::now();
    let imp_res = client
        .import(
            "create_only",
            &fixture.source_id,
            &fixture.source_epoch,
            &fixture.snapshot,
            None,
        )
        .await?;
    let snap_duration = snap_start.elapsed();
    if imp_res.status != 200 && imp_res.status != 201 {
        return Err(format!(
            "Snapshot import failed: HTTP {} {}",
            imp_res.status, imp_res.body
        )
        .into());
    }
    let snap_elements = fixture.snapshot.nodes.len() + fixture.snapshot.edges.len();
    let snap_elements_per_sec = (snap_elements as f64) / snap_duration.as_secs_f64();

    // 6. Measure Catch-Up Streaming Throughput
    let catchup_start = Instant::now();
    let mut batch_count = 0;
    for chunk in fixture.events.chunks(batch_size) {
        batch_count += 1;
        let evt_res = client.apply_events(chunk).await?;
        if evt_res.status != 200 {
            return Err(format!(
                "Batch {} failed: HTTP {} {}",
                batch_count, evt_res.status, evt_res.body
            )
            .into());
        }
    }
    let catchup_duration = catchup_start.elapsed();
    let catchup_events_per_sec = (fixture.events.len() as f64) / catchup_duration.as_secs_f64();

    // 7. Measure Synchronization Barrier
    let last_seq = fixture
        .events
        .last()
        .map(|e| e.delivery_sequence)
        .unwrap_or(0);
    let last_pos = fixture
        .events
        .last()
        .map(|e| e.source_position.clone())
        .unwrap_or_else(|| fixture.snapshot.position.clone());

    let barrier_start = Instant::now();
    let barrier_res = client.declare_barrier(last_seq, &last_pos).await?;
    let barrier_duration = barrier_start.elapsed();
    if barrier_res.status != 200 {
        return Err(format!(
            "Barrier declaration failed: HTTP {} {}",
            barrier_res.status, barrier_res.body
        )
        .into());
    }
    let total_sync_duration = total_sync_start.elapsed();

    // Verify readiness
    let ready_res = client.get_ready().await?;
    if ready_res.status != 200 {
        return Err(format!(
            "Server readiness check failed: HTTP {} {}",
            ready_res.status, ready_res.body
        )
        .into());
    }

    // 8. Measure Query Serving Latency (SEPARATED FROM ORACLE BFS CALCULATION)
    // Warmup: 50 sequential queries executed before timing window
    for u in 1..=50 {
        let v = (u % node_count as u64) + 1;
        let _ = client.query_connected(u, v).await?;
    }

    let mut query_latencies: Vec<Duration> = Vec::with_capacity(query_count);
    let mut query_results: Vec<(u64, u64, bool)> = Vec::with_capacity(query_count);

    // Timed Query Loop (Pure HTTP latency without oracle computation)
    let query_eval_start = Instant::now();
    for i in 1..=query_count {
        let u = ((i * 17) % node_count + 1) as u64;
        let v = (((i * 37) + 5) % node_count + 1) as u64;

        let q_start = Instant::now();
        let q_res = client.query_connected(u, v).await?;
        let q_dur = q_start.elapsed();
        query_latencies.push(q_dur);

        if q_res.status != 200 {
            return Err(format!(
                "Unexpected HTTP response status {}: {}",
                q_res.status, q_res.body
            )
            .into());
        }

        let json = q_res.json();
        let knotrel_conn = json
            .get("connected")
            .and_then(|c| c.as_bool())
            .ok_or_else(|| {
                format!(
                    "Malformed query response, missing boolean 'connected' field: {}",
                    q_res.body
                )
            })?;

        query_results.push((u, v, knotrel_conn));
    }
    let total_query_duration = query_eval_start.elapsed();
    let query_qps = (query_count as f64) / total_query_duration.as_secs_f64();
    let q_stats = calculate_latency_stats(query_latencies);

    // Independent Ground-Truth Verification (OUTSIDE THE TIMED QUERY WINDOW)
    let mut verified_correct = 0_usize;
    for &(u, v, knotrel_conn) in &query_results {
        let oracle_conn = oracle.is_connected(u, v);
        if knotrel_conn == oracle_conn {
            verified_correct += 1;
        } else {
            return Err(format!(
                "CRITICAL: Oracle disagreement on connected({u}, {v})! Knotrel={knotrel_conn}, Oracle={oracle_conn}"
            )
            .into());
        }
    }
    let agreement_pct = (verified_correct as f64 / query_count as f64) * 100.0;

    // 9. Process RSS Readings
    let (peak_rss_kib, sampled_rss_kib) = if let Some((peak_atom, last_atom, stop_tx)) = rss_monitor
    {
        let _ = stop_tx.send(());
        (
            peak_atom.load(Ordering::Relaxed),
            last_atom.load(Ordering::Relaxed),
        )
    } else {
        (0, 0)
    };

    // 10. Measure Restart & Rebuild Recovery Duration
    let mut recovery_duration: Option<Duration> = None;
    if should_spawn {
        client.stop_heartbeat().await;
        if let Some(mut guard) = server_guard.take() {
            guard.kill(); // Kill previous server and confirm exit
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let recovery_start = Instant::now();
        let mut recovered_guard = spawn_server(address, engine, server_bin, "source-managed")?;

        if let Err(e) =
            wait_server_listening(&recovered_guard, address, Duration::from_secs(5)).await
        {
            return Err(
                format!("Failed to restart server on {address} during recovery: {e}").into(),
            );
        }

        let mut rec_client = ExternalFeederClient::new(address, &format!("feeder-rec-{run_index}"));
        rec_client.discover().await?;

        let rec_info = rec_client.raw_request("GET", "/v1/info", &[], "").await?;
        if rec_info.status != 200 {
            return Err(format!(
                "Rebuilt server /v1/info query failed: HTTP {}",
                rec_info.status
            )
            .into());
        }
        let rec_effective_engine = rec_info.json()["engine"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if rec_effective_engine != engine.as_str() {
            return Err(format!(
                "Rebuilt server effective engine mismatch: reported '{rec_effective_engine}', expected '{}'",
                engine.as_str()
            )
            .into());
        }

        rec_client.register_session(30_000).await?;
        rec_client.start_heartbeat(Duration::from_millis(2_000));

        let imp_rec = rec_client
            .import(
                "create_only",
                &fixture.source_id,
                &fixture.source_epoch,
                &fixture.snapshot,
                None,
            )
            .await?;
        if imp_rec.status != 200 && imp_rec.status != 201 {
            return Err(format!("Rebuild import failed: HTTP {}", imp_rec.status).into());
        }

        for chunk in fixture.events.chunks(batch_size) {
            rec_client.apply_events(chunk).await?;
        }

        let b_rec = rec_client.declare_barrier(last_seq, &last_pos).await?;
        if b_rec.status != 200 {
            return Err(format!("Rebuild barrier failed: HTTP {}", b_rec.status).into());
        }

        let ready_rec = rec_client.get_ready().await?;
        if ready_rec.status != 200 {
            return Err(format!("Rebuilt server not ready: HTTP {}", ready_rec.status).into());
        }

        recovery_duration = Some(recovery_start.elapsed());
        rec_client.stop_heartbeat().await;
        recovered_guard.kill();
    } else {
        client.stop_heartbeat().await;
    }

    Ok((
        RunMetrics {
            run_index,
            seed,
            snap_duration,
            snap_elements,
            snap_elements_per_sec,
            catchup_duration,
            catchup_events: fixture.events.len(),
            batch_count,
            catchup_events_per_sec,
            barrier_duration,
            total_sync_duration,
            total_query_duration,
            query_qps,
            latency: q_stats,
            oracle_agreement_pct: agreement_pct,
            verified_queries: verified_correct,
            total_queries: query_count,
            sampled_rss_kib,
            peak_rss_kib,
            recovery_duration,
        },
        disc_id,
        effective_engine,
        engine_experimental,
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_cancellation_handler();

    if let Err(e) = run().await {
        if COORDINATOR.is_cancelled() {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        return Err(e);
    }

    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut scenario = Scenario::Lifecycle;
    let mut dataset_tier = DatasetTier::Small;
    let mut seed = 42_u64;
    let mut custom_queries: Option<usize> = None;
    let mut batch_size = 100_usize;
    let mut batch_warmup_chunk_size: Option<usize> = None;
    let mut custom_addr: Option<SocketAddr> = None;
    let mut should_spawn = false;
    let mut runs = 1_usize;
    let mut output_file: Option<String> = None;
    let mut engine = EngineSelector::CompactBfs;
    let mut server_bin: Option<PathBuf> = None;
    let mut json_out: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--scenario" if i + 1 < args.len() => {
                scenario = Scenario::parse(&args[i + 1]).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--dataset" if i + 1 < args.len() => {
                dataset_tier = DatasetTier::parse(&args[i + 1])
                    .ok_or_else(|| format!("unknown dataset tier: {}", args[i + 1]))?;
                i += 2;
            }
            "--seed" if i + 1 < args.len() => {
                seed = args[i + 1].parse()?;
                i += 2;
            }
            "--queries" if i + 1 < args.len() => {
                custom_queries = Some(args[i + 1].parse()?);
                i += 2;
            }
            "--batch-size" if i + 1 < args.len() => {
                batch_size = args[i + 1].parse()?;
                if batch_size == 0 {
                    eprintln!("Error: --batch-size must be >= 1");
                    std::process::exit(1);
                }
                i += 2;
            }
            "--batch-warmup-chunk-size" if i + 1 < args.len() => {
                let size = args[i + 1].parse::<usize>()?;
                if !(1..=50).contains(&size) {
                    return Err("--batch-warmup-chunk-size must be between 1 and 50".into());
                }
                batch_warmup_chunk_size = Some(size);
                i += 2;
            }
            "--runs" if i + 1 < args.len() => {
                runs = args[i + 1].parse()?;
                if runs == 0 {
                    eprintln!("Error: --runs must be >= 1");
                    std::process::exit(1);
                }
                i += 2;
            }
            "--output-file" if i + 1 < args.len() => {
                output_file = Some(args[i + 1].clone());
                i += 2;
            }
            "--addr" if i + 1 < args.len() => {
                custom_addr = Some(args[i + 1].parse()?);
                i += 2;
            }
            "--spawn-server" => {
                should_spawn = true;
                i += 1;
            }
            "--engine" if i + 1 < args.len() => {
                engine = EngineSelector::parse(&args[i + 1]).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--server-bin" if i + 1 < args.len() => {
                let p = PathBuf::from(&args[i + 1]);
                if !p.exists() {
                    eprintln!(
                        "Error: specified server binary '{}' does not exist",
                        p.display()
                    );
                    std::process::exit(1);
                }
                if !p.is_file() {
                    eprintln!(
                        "Error: specified server binary '{}' is not a file",
                        p.display()
                    );
                    std::process::exit(1);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if p.metadata()
                        .map(|m| m.permissions().mode() & 0o111 == 0)
                        .unwrap_or(false)
                    {
                        eprintln!(
                            "Error: specified server binary '{}' is not executable",
                            p.display()
                        );
                        std::process::exit(1);
                    }
                }
                server_bin = Some(p);
                i += 2;
            }
            "--json-out" if i + 1 < args.len() => {
                let p = PathBuf::from(&args[i + 1]);
                if p.exists() {
                    eprintln!(
                        "Error: output file '{}' already exists; refusing to overwrite existing evidence",
                        p.display()
                    );
                    std::process::exit(1);
                }
                json_out = Some(p);
                i += 2;
            }
            "--help" | "-h" => {
                println!(
                    "  --batch-warmup-chunk-size <1..50> Untimed query chunk size, standalone-batch only (default: 50)"
                );
                println!("Knotrel End-to-End Measurement Suite (Feature 0.2.6 & Feature M3.1)");
                println!("Usage: cargo run --example measure_e2e -- [options]");
                println!();
                println!("Options:");
                println!(
                    "  --scenario <lifecycle|standalone-batch>  Measurement scenario (default: lifecycle)"
                );
                println!(
                    "  --dataset <small|medium|large>   Synthetic dataset tier for lifecycle (default: small)"
                );
                println!(
                    "  --seed <u64>                     RNG seed for synthetic fixture (default: 42)"
                );
                println!(
                    "  --queries <usize>                Number of queries to benchmark (default per tier)"
                );
                println!(
                    "  --batch-size <usize>             Batch size for event streaming or /v1/batch (default: 100)"
                );
                println!(
                    "  --runs <usize>                   Number of repeated measurement runs (default: 1)"
                );
                println!(
                    "  --output-file <path>             Path to append or write Markdown evidence table"
                );
                println!("  --spawn-server                   Spawn child knotrel-server process");
                println!(
                    "  --server-bin <path>              Explicit server executable path (requires --spawn-server)"
                );
                println!(
                    "  --engine <compact-bfs|ett|hdt>   Server engine selector (default: compact-bfs)"
                );
                println!(
                    "  --json-out <path>                Path to write structured JSON report (refuses to overwrite)"
                );
                println!("  --addr <socket_addr>             Connect to an existing server");
                println!("  --help, -h                       Show this help message");
                return Ok(());
            }
            unknown => {
                eprintln!("Unknown argument: {unknown}. Run with --help for usage.");
                std::process::exit(1);
            }
        }
    }

    if batch_warmup_chunk_size.is_some() && scenario != Scenario::StandaloneBatch {
        return Err("--batch-warmup-chunk-size requires --scenario standalone-batch".into());
    }
    let batch_warmup_chunk_size = batch_warmup_chunk_size.unwrap_or(50);

    if scenario == Scenario::StandaloneBatch && batch_size > 1024 {
        eprintln!("Error: --batch-size must be <= 1024 for /v1/batch");
        std::process::exit(1);
    }

    if server_bin.is_some() && !should_spawn {
        eprintln!(
            "Error: --server-bin requires --spawn-server (cannot specify server binary when connecting to an existing server)"
        );
        std::process::exit(1);
    }

    let (node_count, edge_count, event_count, default_queries) = dataset_tier.params();
    let query_count = custom_queries.unwrap_or(default_queries);

    let build_profile = if cfg!(debug_assertions) {
        "debug (unoptimized smoke validation build - not release evidence)"
    } else {
        "release (optimized performance build)"
    };

    let resolved_server_bin = if should_spawn {
        if let Some(ref p) = server_bin {
            p.clone()
        } else {
            find_server_binary().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            })
        }
    } else {
        PathBuf::from("external-server")
    };

    let server_bin_sha256 = if should_spawn {
        compute_file_sha256(&resolved_server_bin).ok()
    } else {
        None
    };

    if scenario == Scenario::Lifecycle {
        println!(
            "================================================================================"
        );
        println!("KNOTREL END-TO-END MEASUREMENT SUITE (Feature 0.2.6 & Qualification)");
        println!("Build Profile:    {}", build_profile);
        println!(
            "Dataset Tier:     {} (nodes={}, edges={}, events={}, queries={})",
            dataset_tier.name(),
            node_count,
            edge_count,
            event_count,
            query_count
        );
        println!("Repeated Runs:    {}", runs);
        println!("Engine Selector:  {}", engine.as_str());
        if should_spawn {
            println!("Server Binary:    {}", resolved_server_bin.display());
            if let Some(ref sha) = server_bin_sha256 {
                println!("Server SHA-256:   {}", sha);
            }
        }
        println!(
            "Connection Model: HTTP/1.1 sequential loopback TCP (Connection: close; full round-trip)"
        );
        println!("Warmup:           50 warmup queries prior to timing window");
        println!(
            "Timing Model:     Pure HTTP query loop; Oracle BFS verification executed out-of-band"
        );
        println!(
            "================================================================================"
        );
        println!();

        let mut all_runs: Vec<RunMetrics> = Vec::with_capacity(runs);
        let mut last_instance_id = String::new();
        let mut effective_engine_str = String::new();
        let mut is_experimental = false;

        for r in 1..=runs {
            let run_seed = seed + (r as u64 - 1);
            let result = execute_single_measurement_run(
                r,
                dataset_tier,
                run_seed,
                query_count,
                batch_size,
                custom_addr,
                should_spawn,
                engine,
                if should_spawn {
                    Some(resolved_server_bin.as_path())
                } else {
                    None
                },
            )
            .await;

            match result {
                Ok((m, inst_id, eff_eng, exp)) => {
                    last_instance_id = inst_id;
                    effective_engine_str = eff_eng;
                    is_experimental = exp;
                    all_runs.push(m);
                }
                Err(e) => {
                    eprintln!("\n❌ Measurement run {r} failed: {e}");
                    std::process::exit(1);
                }
            }
        }

        // Markdown Report Generation (Preserving historical columns and format)
        let mut out = String::new();
        out.push_str("\n### Knotrel End-to-End Measurement Report (Feature 0.2.6)\n\n");
        out.push_str(&format!("- **Build Profile**: `{build_profile}`\n"));
        out.push_str(&format!("- **Engine Selector**: `{}`\n", engine.as_str()));
        out.push_str(&format!(
            "- **Dataset Tier**: `{}` (`{}` nodes, `{}` edges, `{}` events, `{}` queries)\n",
            dataset_tier.name(),
            node_count,
            edge_count,
            event_count,
            query_count
        ));
        out.push_str(&format!("- **Benchmark Iterations**: `{runs}`\n"));
        if should_spawn {
            out.push_str(&format!(
                "- **Server Binary**: `{}`\n",
                resolved_server_bin.display()
            ));
        }
        out.push_str("- **Connection Mode**: Sequential loopback TCP (`Connection: close`, measures full connect + request + response per query)\n");
        out.push_str("- **Timing Boundary**: Timed HTTP query loop separated from ground-truth BFS oracle verification\n\n");

        out.push_str("| Run # | Import (ms) | Catch-up (ms) | Time-to-Ready (ms) | Query QPS | Latency p50 (µs) | Latency p95 (µs) | Oracle Agreement | Final Sampled RSS (MiB) | Sampled Max RSS (MiB)* | Restart Recovery (ms) |\n");
        out.push_str("| :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |\n");

        for m in &all_runs {
            let rec_str = m
                .recovery_duration_ms()
                .map(|d| format!("{d:.2}"))
                .unwrap_or_else(|| "N/A".to_string());
            let sampled_mib = m.sampled_rss_kib as f64 / 1024.0;
            let peak_mib = m.peak_rss_kib as f64 / 1024.0;

            out.push_str(&format!(
            "| **#{}** | `{:.2}` | `{:.2}` | `{:.2}` | `{:.1}` | `{:.1}` | `{:.1}` | `{}/{} ({:.2}%)` | `{:.2}` | `{:.2}` | `{}` |\n",
            m.run_index,
            m.snap_duration_ms(),
            m.catchup_duration_ms(),
            m.total_time_to_ready_ms(),
            m.query_qps,
            m.latency.median_us,
            m.latency.p95_us,
            m.verified_queries,
            m.total_queries,
            m.oracle_agreement_pct,
            sampled_mib,
            peak_mib,
            rec_str
        ));
        }

        if runs > 1 {
            let avg_snap = all_runs.iter().map(|r| r.snap_duration_ms()).sum::<f64>() / runs as f64;
            let avg_catchup = all_runs
                .iter()
                .map(|r| r.catchup_duration_ms())
                .sum::<f64>()
                / runs as f64;
            let avg_ttr = all_runs
                .iter()
                .map(|r| r.total_time_to_ready_ms())
                .sum::<f64>()
                / runs as f64;
            let avg_qps = all_runs.iter().map(|r| r.query_qps).sum::<f64>() / runs as f64;
            let avg_p50 = all_runs.iter().map(|r| r.latency.median_us).sum::<f64>() / runs as f64;
            let avg_p95 = all_runs.iter().map(|r| r.latency.p95_us).sum::<f64>() / runs as f64;
            let avg_sampled_rss = all_runs
                .iter()
                .map(|r| r.sampled_rss_kib as f64 / 1024.0)
                .sum::<f64>()
                / runs as f64;
            let max_peak_rss = all_runs
                .iter()
                .map(|r| r.peak_rss_kib as f64 / 1024.0)
                .fold(0.0_f64, f64::max);
            let avg_rec = if should_spawn {
                let sum_rec: f64 = all_runs
                    .iter()
                    .filter_map(|r| r.recovery_duration_ms())
                    .sum();
                format!("{:.2}", sum_rec / runs as f64)
            } else {
                "N/A".to_string()
            };

            out.push_str(&format!(
            "| **Mean** | `{:.2}` | `{:.2}` | `{:.2}` | `{:.1}` | `{:.1}` | `{:.1}` | `100.00%` | `{:.2}` | `{:.2}` (max)* | `{}` |\n",
            avg_snap, avg_catchup, avg_ttr, avg_qps, avg_p50, avg_p95, avg_sampled_rss, max_peak_rss, avg_rec
        ));
        }

        out.push_str("\n\\* *Note: Sampled Max RSS is the maximum of process RSS values sampled at 25ms intervals. It is a sampled maximum rather than a guaranteed absolute process peak; brief transient allocations between sampling intervals may not be captured.*\n");

        println!("{out}");

        if let Some(ref path) = output_file {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            file.write_all(out.as_bytes())?;
            println!("✓ Evidence appended to {path}");
        }

        // JSON Output Generation
        if let Some(ref json_path) = json_out {
            let json_runs: Vec<JsonRunRecord> = all_runs
                .iter()
                .map(|m| JsonRunRecord {
                    run_index: m.run_index,
                    seed: m.seed,
                    snapshot_import: JsonSnapshotRecord {
                        duration_ns: m.snap_duration.as_nanos() as u64,
                        duration_ms: m.snap_duration_ms(),
                        elements_per_sec: m.snap_elements_per_sec,
                        element_count: m.snap_elements,
                    },
                    catch_up: JsonCatchupRecord {
                        duration_ns: m.catchup_duration.as_nanos() as u64,
                        duration_ms: m.catchup_duration_ms(),
                        events_per_sec: m.catchup_events_per_sec,
                        event_count: m.catchup_events,
                        batch_count: m.batch_count,
                    },
                    barrier: JsonBarrierRecord {
                        duration_ns: m.barrier_duration.as_nanos() as u64,
                        duration_ms: m.barrier_duration_ms(),
                    },
                    registration_through_barrier: JsonPhaseRecord {
                        duration_ns: m.total_sync_duration.as_nanos() as u64,
                        duration_ms: m.total_time_to_ready_ms(),
                    },
                    query: JsonQueryRecord {
                        total_queries: m.total_queries,
                        verified_queries: m.verified_queries,
                        oracle_agreement_pct: m.oracle_agreement_pct,
                        qps: m.query_qps,
                        total_duration_ns: m.total_query_duration.as_nanos() as u64,
                        total_duration_ms: m.total_query_duration.as_secs_f64() * 1000.0,
                        latency_ns: JsonLatencyNs {
                            min: m.latency.min_ns,
                            mean: m.latency.mean_ns,
                            p50: m.latency.median_ns,
                            p95: m.latency.p95_ns,
                            p99: m.latency.p99_ns,
                            max: m.latency.max_ns,
                        },
                        latency_us: JsonLatencyUs {
                            min: m.latency.min_us,
                            mean: m.latency.mean_us,
                            p50: m.latency.median_us,
                            p95: m.latency.p95_us,
                            p99: m.latency.p99_us,
                            max: m.latency.max_us,
                        },
                    },
                    memory: JsonMemoryRecord {
                        sampled_final_rss_kib: m.sampled_rss_kib,
                        sampled_final_rss_bytes: m.sampled_rss_kib * 1024,
                        sampled_max_rss_kib: m.peak_rss_kib,
                        sampled_max_rss_bytes: m.peak_rss_kib * 1024,
                        sampling_interval_ms: 25,
                        note: "Sampled at 25ms intervals via ps; not guaranteed absolute peak RSS",
                    },
                    restart_recovery: m.recovery_duration.map(|d| JsonPhaseRecord {
                        duration_ns: d.as_nanos() as u64,
                        duration_ms: d.as_secs_f64() * 1000.0,
                    }),
                })
                .collect();

            let json_summary = if runs > 1 {
                let avg_snap =
                    all_runs.iter().map(|r| r.snap_duration_ms()).sum::<f64>() / runs as f64;
                let avg_catchup = all_runs
                    .iter()
                    .map(|r| r.catchup_duration_ms())
                    .sum::<f64>()
                    / runs as f64;
                let avg_ttr = all_runs
                    .iter()
                    .map(|r| r.total_time_to_ready_ms())
                    .sum::<f64>()
                    / runs as f64;
                let avg_qps = all_runs.iter().map(|r| r.query_qps).sum::<f64>() / runs as f64;
                let avg_p50 =
                    all_runs.iter().map(|r| r.latency.median_us).sum::<f64>() / runs as f64;
                let avg_p95 = all_runs.iter().map(|r| r.latency.p95_us).sum::<f64>() / runs as f64;
                let avg_sampled_rss = all_runs
                    .iter()
                    .map(|r| r.sampled_rss_kib as f64)
                    .sum::<f64>()
                    / runs as f64;
                let max_peak_rss = all_runs.iter().map(|r| r.peak_rss_kib).max().unwrap_or(0);
                let avg_rec = if should_spawn {
                    let rec_runs: Vec<f64> = all_runs
                        .iter()
                        .filter_map(|r| r.recovery_duration_ms())
                        .collect();
                    if rec_runs.is_empty() {
                        None
                    } else {
                        Some(rec_runs.iter().sum::<f64>() / rec_runs.len() as f64)
                    }
                } else {
                    None
                };

                Some(JsonSummaryRecord {
                    mean_snapshot_duration_ms: avg_snap,
                    mean_catchup_duration_ms: avg_catchup,
                    mean_registration_through_barrier_ms: avg_ttr,
                    mean_query_qps: avg_qps,
                    mean_latency_p50_us: avg_p50,
                    mean_latency_p95_us: avg_p95,
                    mean_sampled_final_rss_kib: avg_sampled_rss,
                    max_sampled_max_rss_kib: max_peak_rss,
                    mean_recovery_duration_ms: avg_rec,
                })
            } else {
                None
            };

            let server_address_str = if let Some(addr) = custom_addr {
                addr.to_string()
            } else {
                "dynamic_loopback".to_string()
            };

            let report = JsonReport {
                schema_version: "measure_e2e.v1",
                driver: DriverMetadata {
                    name: "measure_e2e",
                    version: env!("CARGO_PKG_VERSION"),
                    build_profile,
                },
                server_provenance: ServerProvenance {
                    binary_path: resolved_server_bin.display().to_string(),
                    binary_sha256: server_bin_sha256,
                    instance_id: last_instance_id,
                },
                configuration: RunConfiguration {
                    dataset_tier: dataset_tier.name(),
                    node_count,
                    edge_count,
                    event_count,
                    query_count,
                    batch_size,
                    seed,
                    runs,
                    requested_engine: engine.as_str(),
                    effective_engine: effective_engine_str,
                    engine_experimental: is_experimental,
                    spawn_server: should_spawn,
                    server_address: server_address_str,
                },
                timing_boundaries: TimingBoundariesMetadata {
                    units: UnitsDefinition {
                        duration_ns: "Raw integer nanoseconds as measured by std::time::Instant",
                        duration_ms: "Floating-point milliseconds (duration_ns / 1,000,000.0)",
                        latency_ns: "Raw integer nanoseconds per query",
                        latency_us: "Floating-point microseconds per query (latency_ns / 1,000.0)",
                        throughput_qps: "Queries per second (total_queries / total_query_duration_s)",
                        memory_kib: "Kibibytes (1024 bytes)",
                        memory_bytes: "Bytes (kib * 1024)",
                    },
                    phase_definitions: PhaseDefinitions {
                        snapshot_import: "Duration of HTTP snapshot import call (client send to 200/201 response receipt)",
                        catch_up: "Sum of sequential catch-up event batch HTTP calls from client send to response receipt",
                        barrier: "Duration of declare_barrier HTTP call from client send to 200 response receipt",
                        registration_through_barrier: "Registration-through-barrier duration: discovery start through barrier completion (does NOT measure process spawn to ready)",
                        query_latency: "Pure loopback TCP query round-trip: connect, send request, receive body, close connection; excludes BFS oracle verification",
                        restart_recovery: "Abrupt termination, process reap, restart, listen wait, rediscovery, re-registration, re-import, event replay, and barrier declaration to ready",
                        sampled_rss: "Process resident set size polled every 25ms via ps; sampled maximum, not guaranteed absolute peak",
                    },
                },
                runs: json_runs,
                summary: json_summary,
            };

            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(json_path)
                .map_err(|e| {
                    format!(
                        "Failed to create JSON report file '{}': {e}",
                        json_path.display()
                    )
                })?;
            let json_bytes = serde_json::to_vec_pretty(&report)
                .map_err(|e| format!("Failed to serialize JSON report: {e}"))?;
            file.write_all(&json_bytes).map_err(|e| {
                format!(
                    "Failed to write JSON report to '{}': {e}",
                    json_path.display()
                )
            })?;
            println!(
                "✓ Structured JSON report written to {}",
                json_path.display()
            );
        }
    } else {
        let partial_batch_size = if 2000 % batch_size != 0 {
            Some(2000 % batch_size)
        } else {
            None
        };
        let actual_request_count = 2000_usize.div_ceil(batch_size);
        let full_batches = if partial_batch_size.is_some() {
            actual_request_count - 1
        } else {
            actual_request_count
        };

        println!(
            "================================================================================"
        );
        println!(
            "KNOTREL STANDALONE BATCH MEASUREMENT SUITE (Scenario: StandaloneBatch / ADR 0013)"
        );
        println!("Build Profile:    {}", build_profile);
        println!("Scenario:         {}", scenario.as_str());
        println!(
            "Trace Identity:   {}",
            StandaloneBatchWorkload::TRACE_IDENTITY
        );
        println!("Topology:         1,000 explicitly created isolated nodes");
        println!(
            "Logical Workload: 2,000 ops (500 links, 500 cuts, 1,000 queries) in 500 reversible rounds"
        );
        println!(
            "Batch Size:       {} (requests: {}, full: {}, partial: {:?})",
            batch_size, actual_request_count, full_batches, partial_batch_size
        );
        println!("Repeated Runs:    {}", runs);
        println!("Engine Selector:  {}", engine.as_str());
        if should_spawn {
            println!("Server Binary:    {}", resolved_server_bin.display());
            if let Some(ref sha) = server_bin_sha256 {
                println!("Server SHA-256:   {}", sha);
            }
        }
        println!(
            "Connection Model: HTTP/1.1 sequential loopback TCP (Connection: close; full round-trip)"
        );
        println!(
            "Warmup:           50 query-only operations, chunk size {batch_warmup_chunk_size}"
        );
        println!(
            "Timing Model:     Request latency: HTTP round-trip (connect + send + recv + close)"
        );
        println!(
            "                  Aggregate window: total elapsed dispatch (includes serialization,"
        );
        println!(
            "                  response parsing, item validation, and bookkeeping; distinct from"
        );
        println!("                  server execution time)");
        println!(
            "================================================================================"
        );
        println!();

        let mut all_batch_runs: Vec<BatchRunMetrics> = Vec::with_capacity(runs);
        let mut last_instance_id = String::new();
        let mut effective_engine_str = String::new();
        let mut is_experimental = false;

        for r in 1..=runs {
            let run_seed = seed + (r as u64 - 1);
            let result = execute_single_batch_measurement_run(
                r,
                run_seed,
                BatchRequestSizes {
                    measured: batch_size,
                    warmup: batch_warmup_chunk_size,
                },
                custom_addr,
                should_spawn,
                engine,
                if should_spawn {
                    Some(resolved_server_bin.as_path())
                } else {
                    None
                },
            )
            .await;

            match result {
                Ok((m, inst_id, eff_eng, exp)) => {
                    last_instance_id = inst_id;
                    effective_engine_str = eff_eng;
                    is_experimental = exp;
                    all_batch_runs.push(m);
                }
                Err(e) => {
                    eprintln!("\n❌ Standalone batch measurement run {r} failed: {e}");
                    std::process::exit(1);
                }
            }
        }

        // Standalone Batch Markdown Report Generation
        let mut out = String::new();
        out.push_str(
            "\n### Knotrel Standalone /v1/batch Measurement Report (Tooling Increment 2)\n\n",
        );
        out.push_str(&format!("- **Build Profile**: `{build_profile}`\n"));
        out.push_str(&format!("- **Scenario**: `{}`\n", scenario.as_str()));
        out.push_str(&format!(
            "- **Warmup Chunk Size**: {batch_warmup_chunk_size} (50 query-only operations)\n"
        ));
        out.push_str(&format!(
            "- **Trace Identity**: `{}`\n",
            StandaloneBatchWorkload::TRACE_IDENTITY
        ));
        out.push_str(&format!(
            "- **Engine Selector**: `{}` (effective: `{effective_engine_str}`)\n",
            engine.as_str()
        ));
        out.push_str("- **Topology**: 1,000 explicitly created isolated nodes\n");
        out.push_str(&format!(
            "- **Logical Workload**: 2,000 ops (500 links, 500 cuts, 1,000 queries; seed {seed})\n"
        ));
        out.push_str(&format!(
            "- **Batch Configuration**: batch_size={batch_size}, requests={actual_request_count}, full_batches={full_batches}, partial_batch={partial_batch_size:?}\n"
        ));
        out.push_str(&format!("- **Benchmark Iterations**: `{runs}`\n"));
        if should_spawn {
            out.push_str(&format!(
                "- **Server Binary**: `{}`\n",
                resolved_server_bin.display()
            ));
        }
        out.push_str("- **Connection Mode**: Sequential loopback TCP (`Connection: close`, measures full connect + request + response per batch)\n");
        out.push_str("- **Timing Boundary**: Request latency measures individual loopback HTTP round-trips (`Connection: close`); aggregate elapsed-window metrics explicitly include client request serialization, response parsing, per-item correctness validation, revision arithmetic verification, and bookkeeping (and are distinct from individual request latency and server-side execution time).\n\n");

        out.push_str("| Run # | Batch Size | Requests | Window (ms) | Logical Ops/s | Window ns/op | Req Latency p50 (µs) | Req Latency p95 (µs) | Verified Items | Final Sampled RSS (MiB) | Sampled Max RSS (MiB)* |\n");
        out.push_str("| :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |\n");

        for m in &all_batch_runs {
            let window_ms = m.window_duration.as_secs_f64() * 1000.0;
            let ops_per_sec = (m.total_logical_operations as f64) / m.window_duration.as_secs_f64();
            let ns_per_op =
                (m.window_duration.as_nanos() as f64) / (m.total_logical_operations as f64);
            let lat_stats = calculate_latency_stats(m.request_latencies.clone());
            let sampled_mib = m.sampled_final_rss_kib as f64 / 1024.0;
            let peak_mib = m.sampled_max_rss_kib as f64 / 1024.0;

            out.push_str(&format!(
                "| **#{}** | `{}` | `{}` | `{:.2}` | `{:.1}` | `{:.1}` | `{:.1}` | `{:.1}` | `{}/{}` | `{:.2}` | `{:.2}` |\n",
                m.run_index,
                m.batch_size,
                m.actual_requests,
                window_ms,
                ops_per_sec,
                ns_per_op,
                lat_stats.median_us,
                lat_stats.p95_us,
                m.verified_items,
                m.total_logical_operations,
                sampled_mib,
                peak_mib
            ));
        }

        if runs > 1 {
            let avg_window = all_batch_runs
                .iter()
                .map(|r| r.window_duration.as_secs_f64() * 1000.0)
                .sum::<f64>()
                / runs as f64;
            let avg_ops_sec = all_batch_runs
                .iter()
                .map(|r| (r.total_logical_operations as f64) / r.window_duration.as_secs_f64())
                .sum::<f64>()
                / runs as f64;
            let avg_ns_op = all_batch_runs
                .iter()
                .map(|r| {
                    (r.window_duration.as_nanos() as f64) / (r.total_logical_operations as f64)
                })
                .sum::<f64>()
                / runs as f64;
            let avg_p50 = all_batch_runs
                .iter()
                .map(|r| calculate_latency_stats(r.request_latencies.clone()).median_us)
                .sum::<f64>()
                / runs as f64;
            let avg_p95 = all_batch_runs
                .iter()
                .map(|r| calculate_latency_stats(r.request_latencies.clone()).p95_us)
                .sum::<f64>()
                / runs as f64;
            let avg_sampled_rss = all_batch_runs
                .iter()
                .map(|r| r.sampled_final_rss_kib as f64 / 1024.0)
                .sum::<f64>()
                / runs as f64;
            let max_peak_rss = all_batch_runs
                .iter()
                .map(|r| r.sampled_max_rss_kib as f64 / 1024.0)
                .fold(0.0_f64, f64::max);

            out.push_str(&format!(
                "| **Mean** | `{}` | `{}` | `{:.2}` | `{:.1}` | `{:.1}` | `{:.1}` | `{:.1}` | `100.00%` | `{:.2}` | `{:.2}` (max)* |\n",
                batch_size,
                actual_request_count,
                avg_window,
                avg_ops_sec,
                avg_ns_op,
                avg_p50,
                avg_p95,
                avg_sampled_rss,
                max_peak_rss
            ));
        }

        out.push_str("\n\\* *Note: Sampled Max RSS is the maximum of process RSS values sampled at 25ms intervals. It is a sampled maximum rather than a guaranteed absolute process peak; brief transient allocations between sampling intervals may not be captured.*\n");

        println!("{out}");

        if let Some(ref path) = output_file {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            file.write_all(out.as_bytes())?;
            println!("✓ Evidence appended to {path}");
        }

        // Standalone Batch JSON Output Generation
        if let Some(ref json_path) = json_out {
            let json_runs: Vec<JsonBatchRunRecord> = all_batch_runs
                .iter()
                .map(|m| {
                    let lat = calculate_latency_stats(m.request_latencies.clone());
                    let window_ms = m.window_duration.as_secs_f64() * 1000.0;
                    let ops_per_sec = (m.total_logical_operations as f64)
                        / m.window_duration.as_secs_f64();
                    let ns_per_op = (m.window_duration.as_nanos() as f64)
                        / (m.total_logical_operations as f64);
                    let reqs_per_sec =
                        (m.actual_requests as f64) / m.window_duration.as_secs_f64();

                    JsonBatchRunRecord {
                        run_index: m.run_index,
                        seed: m.seed,
                        setup: JsonBatchSetupRecord {
                            duration_ns: m.setup_duration.as_nanos() as u64,
                            duration_ms: m.setup_duration.as_secs_f64() * 1000.0,
                            created_nodes: m.created_nodes,
                            initial_state_version: m.initial_state_version.clone(),
                        },
                        warmup: JsonBatchWarmupRecord {
                            request_count: 50_usize.div_ceil(batch_warmup_chunk_size),
                            duration_ns: m.warmup_duration.as_nanos() as u64,
                            duration_ms: m.warmup_duration.as_secs_f64() * 1000.0,
                            query_count: m.warmup_queries,
                            post_warmup_state_version: m.post_warmup_state_version.clone(),
                        },
                        measurement_window: JsonBatchWindowRecord {
                            duration_ns: m.window_duration.as_nanos() as u64,
                            duration_ms: window_ms,
                            total_logical_operations: m.total_logical_operations,
                            effective_mutations: m.effective_mutations,
                            elapsed_window_ns_per_logical_op: ns_per_op,
                            logical_ops_per_sec: ops_per_sec,
                            requests_per_sec: reqs_per_sec,
                        },
                        request_latency: JsonBatchRequestLatencyRecord {
                            total_requests: m.actual_requests,
                            latency_ns: JsonLatencyNs {
                                min: lat.min_ns,
                                mean: lat.mean_ns,
                                p50: lat.median_ns,
                                p95: lat.p95_ns,
                                p99: lat.p99_ns,
                                max: lat.max_ns,
                            },
                            latency_us: JsonLatencyUs {
                                min: lat.min_us,
                                mean: lat.mean_us,
                                p50: lat.median_us,
                                p95: lat.p95_us,
                                p99: lat.p99_us,
                                max: lat.max_us,
                            },
                        },
                        verification: JsonBatchVerificationRecord {
                            all_items_matched: m.verified_items == m.total_logical_operations,
                            total_verified_items: m.verified_items,
                            expected_final_state_version: "2000".to_string(),
                            actual_final_state_version: m.final_state_version.clone(),
                            final_node_count: m.final_node_count,
                            final_edge_count: m.final_edge_count,
                        },
                        memory: JsonMemoryRecord {
                            sampled_final_rss_kib: m.sampled_final_rss_kib,
                            sampled_final_rss_bytes: m.sampled_final_rss_kib * 1024,
                            sampled_max_rss_kib: m.sampled_max_rss_kib,
                            sampled_max_rss_bytes: m.sampled_max_rss_kib * 1024,
                            sampling_interval_ms: 25,
                            note: "Sampled at 25ms intervals via ps; not guaranteed absolute peak RSS",
                        },
                    }
                })
                .collect();

            let json_summary = if runs > 1 {
                let avg_window = all_batch_runs
                    .iter()
                    .map(|r| r.window_duration.as_secs_f64() * 1000.0)
                    .sum::<f64>()
                    / runs as f64;
                let avg_ops_sec = all_batch_runs
                    .iter()
                    .map(|r| (r.total_logical_operations as f64) / r.window_duration.as_secs_f64())
                    .sum::<f64>()
                    / runs as f64;
                let avg_ns_op = all_batch_runs
                    .iter()
                    .map(|r| {
                        (r.window_duration.as_nanos() as f64) / (r.total_logical_operations as f64)
                    })
                    .sum::<f64>()
                    / runs as f64;
                let avg_p50 = all_batch_runs
                    .iter()
                    .map(|r| calculate_latency_stats(r.request_latencies.clone()).median_us)
                    .sum::<f64>()
                    / runs as f64;
                let avg_p95 = all_batch_runs
                    .iter()
                    .map(|r| calculate_latency_stats(r.request_latencies.clone()).p95_us)
                    .sum::<f64>()
                    / runs as f64;
                let avg_sampled_rss = all_batch_runs
                    .iter()
                    .map(|r| r.sampled_final_rss_kib as f64)
                    .sum::<f64>()
                    / runs as f64;
                let max_peak_rss = all_batch_runs
                    .iter()
                    .map(|r| r.sampled_max_rss_kib)
                    .max()
                    .unwrap_or(0);

                Some(JsonBatchSummaryRecord {
                    mean_window_duration_ms: avg_window,
                    mean_logical_ops_per_sec: avg_ops_sec,
                    mean_elapsed_window_ns_per_logical_op: avg_ns_op,
                    mean_request_latency_p50_us: avg_p50,
                    mean_request_latency_p95_us: avg_p95,
                    mean_sampled_final_rss_kib: avg_sampled_rss,
                    max_sampled_max_rss_kib: max_peak_rss,
                })
            } else {
                None
            };

            let server_address_str = if let Some(addr) = custom_addr {
                addr.to_string()
            } else {
                "dynamic_loopback".to_string()
            };

            let report = JsonBatchReport {
                schema_version: "measure_e2e.batch.v1",
                driver: DriverMetadata {
                    name: "measure_e2e",
                    version: env!("CARGO_PKG_VERSION"),
                    build_profile,
                },
                server_provenance: ServerProvenance {
                    binary_path: resolved_server_bin.display().to_string(),
                    binary_sha256: server_bin_sha256,
                    instance_id: last_instance_id,
                },
                configuration: BatchRunConfiguration {
                    scenario: scenario.as_str(),
                    trace_identity: StandaloneBatchWorkload::TRACE_IDENTITY,
                    workload_description: "Deterministic 500-round batch-overhead workload restoring empty-edge topology",
                    topology: "1,000 explicitly created isolated nodes",
                    node_count: StandaloneBatchWorkload::NODE_COUNT,
                    seed,
                    batch_size,
                    total_logical_operations: StandaloneBatchWorkload::TOTAL_OPERATIONS,
                    link_operations: StandaloneBatchWorkload::ROUND_COUNT,
                    cut_operations: StandaloneBatchWorkload::ROUND_COUNT,
                    query_operations: StandaloneBatchWorkload::ROUND_COUNT * 2,
                    actual_request_count,
                    full_batches,
                    partial_batch_size,
                    warmup: "50 query-only batch operations prior to measurement window",
                    warmup_chunk_size: batch_warmup_chunk_size,
                    connection_model: "HTTP/1.1 sequential loopback TCP (Connection: close; full round-trip per batch)",
                    runs,
                    requested_engine: engine.as_str(),
                    effective_engine: effective_engine_str,
                    engine_experimental: is_experimental,
                    spawn_server: should_spawn,
                    server_address: server_address_str,
                },
                timing_boundaries: BatchTimingBoundariesMetadata {
                    units: BatchUnitsDefinition {
                        duration_ns: "Raw integer nanoseconds as measured by std::time::Instant",
                        duration_ms: "Floating-point milliseconds (duration_ns / 1,000,000.0)",
                        request_latency_ns: "Raw integer nanoseconds per HTTP batch request round-trip (connect + send + recv + close)",
                        request_latency_us: "Floating-point microseconds per HTTP batch request (latency_ns / 1,000.0)",
                        elapsed_window_ns_per_logical_op: "Amortized elapsed window nanoseconds per logical operation (window_duration_ns / total_logical_operations). Note: Aggregate elapsed-window metrics explicitly include client request serialization, network transit, response deserialization, item-level correctness validation, revision arithmetic checks, and bookkeeping; they are distinct from individual request latency and pure server-side execution time.",
                        logical_ops_per_sec: "Logical operations per second over the aggregate measurement window (total_logical_operations / window_duration_s). Measures end-to-end client dispatch throughput including serialization, response validation, and bookkeeping; distinct from server-side execution time.",
                        requests_per_sec: "HTTP batch requests per second over timed measurement window (actual_requests / window_duration_s)",
                        memory_kib: "Kibibytes (1024 bytes)",
                        memory_bytes: "Bytes (kib * 1024)",
                    },
                    notes: BatchTimingNotes {
                        latency_concept: "Request latency measures pure loopback HTTP round-trip (connect + send request + receive response body + close connection) per batch request. It reflects network and server batch handling time. Never divide request latency by batch size to fabricate individual-operation latency.",
                        window_concept: "Elapsed window duration measures aggregate end-to-end benchmark time across the entire 2,000-operation trace, explicitly including client request serialization, network transit, response parsing, per-item correctness validation, revision arithmetic checks, and bookkeeping. These aggregate elapsed-window metrics must be distinguished from individual request latency and pure server-side execution time.",
                    },
                },
                runs: json_runs,
                summary: json_summary,
            };

            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(json_path)
                .map_err(|e| {
                    format!(
                        "Failed to create JSON report file '{}': {e}",
                        json_path.display()
                    )
                })?;
            let json_bytes = serde_json::to_vec_pretty(&report)
                .map_err(|e| format!("Failed to serialize JSON report: {e}"))?;
            file.write_all(&json_bytes).map_err(|e| {
                format!(
                    "Failed to write JSON report to '{}': {e}",
                    json_path.display()
                )
            })?;
            println!(
                "✓ Structured JSON report written to {}",
                json_path.display()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_selector_parsing() {
        assert_eq!(
            EngineSelector::parse("compact-bfs").unwrap(),
            EngineSelector::CompactBfs
        );
        assert_eq!(
            EngineSelector::parse("ett").unwrap(),
            EngineSelector::EulerTour
        );
        assert_eq!(EngineSelector::parse("hdt").unwrap(), EngineSelector::Hdt);
        assert!(EngineSelector::parse("invalid").is_err());
        assert!(EngineSelector::parse("").is_err());
    }

    #[test]
    fn sha256_standard_vectors() {
        assert_eq!(
            sha256_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn latency_stats_calculation() {
        let samples = vec![
            Duration::from_nanos(1_000), // 1.0 us
            Duration::from_nanos(2_000), // 2.0 us
            Duration::from_nanos(3_000), // 3.0 us
            Duration::from_nanos(4_000), // 4.0 us
            Duration::from_nanos(5_000), // 5.0 us
        ];
        let stats = calculate_latency_stats(samples);
        assert_eq!(stats.min_ns, 1_000);
        assert_eq!(stats.max_ns, 5_000);
        assert_eq!(stats.median_ns, 3_000);
        assert!((stats.mean_us - 3.0).abs() < 1e-6);
        assert!((stats.median_us - 3.0).abs() < 1e-6);
    }

    #[test]
    fn bounded_stderr_buffer_within_capacity() {
        let mut buf = BoundedStderrBuffer::new(1024);
        buf.push_bytes(b"hello world\n");
        assert_eq!(buf.total_bytes_read, 12);
        assert!(!buf.truncated);
        assert_eq!(buf.to_string_lossy(), "hello world\n");
    }

    #[test]
    fn bounded_stderr_buffer_overflow_truncation() {
        let mut buf = BoundedStderrBuffer::new(512);
        let chunk = vec![b'A'; 200];
        buf.push_bytes(&chunk);
        buf.push_bytes(&chunk);
        buf.push_bytes(&chunk); // total 600 bytes > 512
        assert_eq!(buf.total_bytes_read, 600);
        assert!(buf.truncated);
        let s = buf.to_string_lossy();
        assert!(s.contains("[... earlier stderr truncated; total bytes read: 600 ...]"));
        assert!(s.ends_with(&"A".repeat(200)));
    }

    #[test]
    fn bounded_stderr_buffer_single_oversized_chunk() {
        let mut buf = BoundedStderrBuffer::new(256);
        let chunk = vec![b'B'; 1000];
        buf.push_bytes(&chunk);
        assert_eq!(buf.total_bytes_read, 1000);
        assert!(buf.truncated);
        assert_eq!(buf.buffer.len(), 256);
    }

    #[test]
    fn scenario_parsing() {
        assert_eq!(Scenario::parse("lifecycle").unwrap(), Scenario::Lifecycle);
        assert_eq!(
            Scenario::parse("standalone-batch").unwrap(),
            Scenario::StandaloneBatch
        );
        assert_eq!(Scenario::parse("batch").unwrap(), Scenario::StandaloneBatch);
        assert!(Scenario::parse("unknown").is_err());
    }

    #[test]
    fn deterministic_batch_workload_generation() {
        let wl1 = StandaloneBatchWorkload::generate(42);
        let wl2 = StandaloneBatchWorkload::generate(42);
        assert_eq!(wl1.setup_nodes.len(), 1000);
        assert_eq!(wl1.setup_nodes, wl2.setup_nodes);
        assert_eq!(wl1.trace.len(), 2000);
        assert_eq!(wl1.expected_outcomes.len(), 2000);
        assert_eq!(wl1.trace, wl2.trace);
        assert_eq!(wl1.expected_outcomes, wl2.expected_outcomes);

        let mut link_count = 0;
        let mut cut_count = 0;
        let mut query_count = 0;
        for op in &wl1.trace {
            match op {
                BatchOp::Link { .. } => link_count += 1,
                BatchOp::Cut { .. } => cut_count += 1,
                BatchOp::Connected { .. } => query_count += 1,
                _ => panic!("unexpected op in batch workload trace"),
            }
        }
        assert_eq!(link_count, 500);
        assert_eq!(cut_count, 500);
        assert_eq!(query_count, 1000);

        // Verify structure of 500 rounds: link(u,v), connected(u,v), cut(u,v), connected(u,v)
        let mut used_nodes = std::collections::HashSet::new();
        for r in 0..500 {
            let op0 = &wl1.trace[4 * r];
            let op1 = &wl1.trace[4 * r + 1];
            let op2 = &wl1.trace[4 * r + 2];
            let op3 = &wl1.trace[4 * r + 3];

            let exp0 = &wl1.expected_outcomes[4 * r];
            let exp1 = &wl1.expected_outcomes[4 * r + 1];
            let exp2 = &wl1.expected_outcomes[4 * r + 2];
            let exp3 = &wl1.expected_outcomes[4 * r + 3];

            match (op0, op1, op2, op3) {
                (
                    BatchOp::Link {
                        source: u0,
                        target: v0,
                    },
                    BatchOp::Connected {
                        source: u1,
                        target: v1,
                    },
                    BatchOp::Cut {
                        source: u2,
                        target: v2,
                    },
                    BatchOp::Connected {
                        source: u3,
                        target: v3,
                    },
                ) => {
                    assert_eq!(u0, u1);
                    assert_eq!(u0, u2);
                    assert_eq!(u0, u3);
                    assert_eq!(v0, v1);
                    assert_eq!(v0, v2);
                    assert_eq!(v0, v3);
                    assert_ne!(u0, v0, "endpoints must be distinct");
                    assert!(
                        used_nodes.insert(u0.clone()),
                        "node {u0} must not be reused across rounds"
                    );
                    assert!(
                        used_nodes.insert(v0.clone()),
                        "node {v0} must not be reused across rounds"
                    );
                }
                _ => panic!("round {r} did not match (link, connected, cut, connected)"),
            }

            assert_eq!(*exp0, ExpectedOutcome::Changed(true));
            assert_eq!(*exp1, ExpectedOutcome::Connected(true));
            assert_eq!(*exp2, ExpectedOutcome::Changed(true));
            assert_eq!(*exp3, ExpectedOutcome::Connected(false));
        }
        assert_eq!(used_nodes.len(), 1000);
    }

    #[test]
    fn batch_chunking_across_sizes() {
        let wl = StandaloneBatchWorkload::generate(42);
        for &size in &[1, 8, 64, 256] {
            let chunks: Vec<&[BatchOp]> = wl.trace.chunks(size).collect();
            let total_ops: usize = chunks.iter().map(|c| c.len()).sum();
            assert_eq!(total_ops, 2000);

            let expected_reqs = 2000_usize.div_ceil(size);
            assert_eq!(chunks.len(), expected_reqs);

            let expected_partial = if 2000 % size != 0 {
                Some(2000 % size)
            } else {
                None
            };
            if let Some(partial) = expected_partial {
                assert_eq!(chunks.last().unwrap().len(), partial);
                for c in &chunks[..chunks.len() - 1] {
                    assert_eq!(c.len(), size);
                }
            } else {
                for c in &chunks {
                    assert_eq!(c.len(), size);
                }
            }

            // Flattened operations match trace exactly
            let flattened: Vec<BatchOp> = chunks.iter().flat_map(|c| c.iter().cloned()).collect();
            assert_eq!(flattened, wl.trace);
        }
    }
}
