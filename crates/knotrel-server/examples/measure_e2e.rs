//! End-to-End Reproducible Measurements for Knotrel Feature 0.2.6.
//!
//! Evaluates the complete external feeder lifecycle against a real `knotrel-server` process:
//! - Atomic snapshot import duration and throughput.
//! - Ordered catch-up event streaming throughput.
//! - Total Time-to-Ready.
//! - Synchronized query latency distribution (p50, p95, p99, mean) with pure query timing.
//! - Independent BFS ground-truth verification and exact agreement percentage.
//! - Sampled maximum process RSS (polled every 25ms; brief transient allocations between sampling intervals may not be captured) and final sampled process RSS.
//! - Abrupt termination and authoritative rebuild recovery duration.
//! - Repeated-run benchmark support (`--runs <N>`) with statistical aggregation.
//!
//! # Build & Run Instructions
//!
//! Fast bounded smoke run (debug build, unoptimized):
//! ```bash
//! cargo run --example measure_e2e -- --dataset small --spawn-server
//! ```
//!
//! Release benchmark (optimized):
//! ```bash
//! cargo run --release --example measure_e2e -- --dataset medium --spawn-server --runs 3
//! ```

#[path = "../tests/common/feeder.rs"]
mod feeder;

use feeder::{ExternalFeederClient, ExternalSourceFixture, ExternalSourceOracle};
use std::{
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Process guard that terminates the child process on drop.
struct ServerGuard {
    child: Option<Child>,
}

impl ServerGuard {
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

fn get_free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr")
}

fn find_server_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("KNOTREL_BIN") {
        return std::path::PathBuf::from(path);
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
            return candidate;
        }
    }
    let fallback = if cfg!(debug_assertions) {
        std::path::PathBuf::from("target/debug/knotrel-server")
    } else {
        std::path::PathBuf::from("target/release/knotrel-server")
    };
    if !fallback.exists() {
        eprintln!(
            "Notice: '{}' not found. Ensure server is compiled with:\n  cargo build --bin knotrel-server --example measure_e2e",
            fallback.display()
        );
    }
    fallback
}

fn spawn_server(address: SocketAddr) -> Child {
    let binary = find_server_binary();
    let mut cmd = Command::new(&binary);
    cmd.env_clear()
        .env("KNOTREL_ADDR", address.to_string())
        .env("KNOTREL_MODE", "source-managed")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().unwrap_or_else(|e| {
        panic!(
            "failed to spawn knotrel-server binary from '{}': {e}\nPlease build with: cargo build --bin knotrel-server --example measure_e2e",
            binary.display()
        )
    })
}

async fn wait_server_listening(address: SocketAddr, timeout_dur: Duration) -> bool {
    let deadline = Instant::now() + timeout_dur;
    while Instant::now() < deadline {
        if let Ok(mut stream) = tokio::net::TcpStream::connect(address).await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let req =
                format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
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

#[derive(Debug, Clone, Copy)]
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
        // (node_count, edge_count, event_count, default_queries)
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

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct LatencyStats {
    min_us: f64,
    mean_us: f64,
    median_us: f64,
    p95_us: f64,
    p99_us: f64,
    max_us: f64,
}

fn calculate_latency_stats(mut samples: Vec<Duration>) -> LatencyStats {
    if samples.is_empty() {
        return LatencyStats {
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
    let sum_us: f64 = samples.iter().map(|d| d.as_secs_f64() * 1_000_000.0).sum();
    let mean_us = sum_us / count as f64;

    let p50_idx = ((count as f64) * 0.50).min((count - 1) as f64) as usize;
    let p95_idx = ((count as f64) * 0.95).min((count - 1) as f64) as usize;
    let p99_idx = ((count as f64) * 0.99).min((count - 1) as f64) as usize;

    LatencyStats {
        min_us: samples[0].as_secs_f64() * 1_000_000.0,
        mean_us,
        median_us: samples[p50_idx].as_secs_f64() * 1_000_000.0,
        p95_us: samples[p95_idx].as_secs_f64() * 1_000_000.0,
        p99_us: samples[p99_idx].as_secs_f64() * 1_000_000.0,
        max_us: samples[count - 1].as_secs_f64() * 1_000_000.0,
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RunMetrics {
    run_index: usize,
    snap_duration_ms: f64,
    snap_elements_per_sec: f64,
    catchup_duration_ms: f64,
    catchup_events_per_sec: f64,
    barrier_duration_ms: f64,
    total_time_to_ready_ms: f64,
    query_qps: f64,
    latency: LatencyStats,
    oracle_agreement_pct: f64,
    verified_queries: usize,
    total_queries: usize,
    sampled_rss_kib: u64,
    peak_rss_kib: u64,
    recovery_duration_ms: Option<f64>,
}

async fn execute_single_measurement_run(
    run_index: usize,
    dataset_tier: DatasetTier,
    seed: u64,
    query_count: usize,
    batch_size: usize,
    custom_addr: Option<SocketAddr>,
    should_spawn: bool,
) -> Result<RunMetrics, Box<dyn std::error::Error>> {
    let (node_count, edge_count, event_count, _) = dataset_tier.params();

    let address = if let Some(addr) = custom_addr {
        addr
    } else if should_spawn {
        get_free_address()
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
        println!("🚀 [Run {run_index}] Spawning knotrel-server on {address}...");
        let child = spawn_server(address);
        let pid = child.id();
        // Immediately guard the child process
        server_guard = Some(ServerGuard { child: Some(child) });
        if !wait_server_listening(address, Duration::from_secs(5)).await {
            eprintln!("Error: timed out waiting for server to listen on {address}");
            std::process::exit(1);
        }
        rss_monitor = Some(start_rss_monitor(pid));
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

    // 3. Connect Client & Discover Server
    let mut client = ExternalFeederClient::new(address, &format!("feeder-bench-{run_index}"));
    let disc_id = client.discover().await?;
    println!("✓ [Run {run_index}] Discovered server instance: {disc_id}");

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
        eprintln!(
            "Snapshot import failed: HTTP {} {}",
            imp_res.status, imp_res.body
        );
        std::process::exit(1);
    }
    let snap_elements = fixture.snapshot.nodes.len() + fixture.snapshot.edges.len();
    let snap_elem_per_sec = (snap_elements as f64) / snap_duration.as_secs_f64();

    // 6. Measure Catch-Up Streaming Throughput
    let catchup_start = Instant::now();
    let mut batch_count = 0;
    for chunk in fixture.events.chunks(batch_size) {
        batch_count += 1;
        let evt_res = client.apply_events(chunk).await?;
        if evt_res.status != 200 {
            eprintln!(
                "Batch {} failed: HTTP {} {}",
                batch_count, evt_res.status, evt_res.body
            );
            std::process::exit(1);
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
        eprintln!(
            "Barrier declaration failed: HTTP {} {}",
            barrier_res.status, barrier_res.body
        );
        std::process::exit(1);
    }
    let total_sync_duration = total_sync_start.elapsed();

    // Verify readiness
    let ready_res = client.get_ready().await?;
    if ready_res.status != 200 {
        eprintln!("Server readiness check failed: HTTP {}", ready_res.status);
        std::process::exit(1);
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
            eprintln!(
                "Error: unexpected HTTP response status {}: {}",
                q_res.status, q_res.body
            );
            std::process::exit(1);
        }

        let json = q_res.json();
        let knotrel_conn = json
            .get("connected")
            .and_then(|c| c.as_bool())
            .unwrap_or_else(|| {
                eprintln!(
                    "Error: malformed query response, missing boolean 'connected' field: {}",
                    q_res.body
                );
                std::process::exit(1);
            });

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
            eprintln!(
                "CRITICAL: Oracle disagreement on connected({u}, {v})! Knotrel={knotrel_conn}, Oracle={oracle_conn}"
            );
            std::process::exit(1);
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
    let mut recovery_duration_ms: Option<f64> = None;
    if should_spawn {
        client.stop_heartbeat().await;
        if let Some(mut guard) = server_guard.take() {
            guard.kill(); // Kill previous server
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let recovery_start = Instant::now();
        let new_child = spawn_server(address);
        let _recovered_guard = ServerGuard {
            child: Some(new_child),
        };

        if !wait_server_listening(address, Duration::from_secs(5)).await {
            eprintln!("Failed to restart server on {address}");
            std::process::exit(1);
        }

        let mut rec_client = ExternalFeederClient::new(address, &format!("feeder-rec-{run_index}"));
        rec_client.discover().await?;
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
            eprintln!("Rebuild import failed: HTTP {}", imp_rec.status);
            std::process::exit(1);
        }

        for chunk in fixture.events.chunks(batch_size) {
            rec_client.apply_events(chunk).await?;
        }

        let b_rec = rec_client.declare_barrier(last_seq, &last_pos).await?;
        if b_rec.status != 200 {
            eprintln!("Rebuild barrier failed: HTTP {}", b_rec.status);
            std::process::exit(1);
        }

        let ready_rec = rec_client.get_ready().await?;
        if ready_rec.status != 200 {
            eprintln!("Rebuilt server not ready: HTTP {}", ready_rec.status);
            std::process::exit(1);
        }

        let rec_dur = recovery_start.elapsed();
        recovery_duration_ms = Some(rec_dur.as_secs_f64() * 1000.0);
        rec_client.stop_heartbeat().await;
    } else {
        client.stop_heartbeat().await;
    }

    Ok(RunMetrics {
        run_index,
        snap_duration_ms: snap_duration.as_secs_f64() * 1000.0,
        snap_elements_per_sec: snap_elem_per_sec,
        catchup_duration_ms: catchup_duration.as_secs_f64() * 1000.0,
        catchup_events_per_sec,
        barrier_duration_ms: barrier_duration.as_secs_f64() * 1000.0,
        total_time_to_ready_ms: total_sync_duration.as_secs_f64() * 1000.0,
        query_qps,
        latency: q_stats,
        oracle_agreement_pct: agreement_pct,
        verified_queries: verified_correct,
        total_queries: query_count,
        sampled_rss_kib,
        peak_rss_kib,
        recovery_duration_ms,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut dataset_tier = DatasetTier::Small;
    let mut seed = 42_u64;
    let mut custom_queries: Option<usize> = None;
    let mut batch_size = 100_usize;
    let mut custom_addr: Option<SocketAddr> = None;
    let mut should_spawn = false;
    let mut runs = 1_usize;
    let mut output_file: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
            "--help" | "-h" => {
                println!("Knotrel End-to-End Measurement Suite (Feature 0.2.6)");
                println!("Usage: cargo run --example measure_e2e -- [options]");
                println!();
                println!("Options:");
                println!(
                    "  --dataset <small|medium|large>   Synthetic dataset tier (default: small)"
                );
                println!(
                    "  --seed <u64>                     RNG seed for synthetic fixture (default: 42)"
                );
                println!(
                    "  --queries <usize>                Number of queries to benchmark (default per tier)"
                );
                println!(
                    "  --batch-size <usize>             Batch size for event streaming (default: 100)"
                );
                println!(
                    "  --runs <usize>                   Number of repeated measurement runs (default: 1)"
                );
                println!(
                    "  --output-file <path>             Path to append or write Markdown evidence table"
                );
                println!("  --spawn-server                   Spawn child knotrel-server process");
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

    let (node_count, edge_count, event_count, default_queries) = dataset_tier.params();
    let query_count = custom_queries.unwrap_or(default_queries);

    let build_profile = if cfg!(debug_assertions) {
        "debug (unoptimized smoke validation build - not release evidence)"
    } else {
        "release (optimized performance build)"
    };

    println!("================================================================================");
    println!("KNOTREL END-TO-END MEASUREMENT SUITE (Feature 0.2.6)");
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
    println!(
        "Connection Model: HTTP/1.1 sequential loopback TCP (Connection: close; full round-trip)"
    );
    println!("Warmup:           50 warmup queries prior to timing window");
    println!(
        "Timing Model:     Pure HTTP query loop; Oracle BFS verification executed out-of-band"
    );
    println!("================================================================================");
    println!();

    let mut all_runs: Vec<RunMetrics> = Vec::with_capacity(runs);

    for r in 1..=runs {
        let m = execute_single_measurement_run(
            r,
            dataset_tier,
            seed + (r as u64 - 1),
            query_count,
            batch_size,
            custom_addr,
            should_spawn,
        )
        .await?;
        all_runs.push(m);
    }

    // Markdown Report Generation
    let mut out = String::new();
    out.push_str("\n### Knotrel End-to-End Measurement Report (Feature 0.2.6)\n\n");
    out.push_str(&format!("- **Build Profile**: `{build_profile}`\n"));
    out.push_str(&format!(
        "- **Dataset Tier**: `{}` (`{}` nodes, `{}` edges, `{}` events, `{}` queries)\n",
        dataset_tier.name(),
        node_count,
        edge_count,
        event_count,
        query_count
    ));
    out.push_str(&format!("- **Benchmark Iterations**: `{runs}`\n"));
    out.push_str("- **Connection Mode**: Sequential loopback TCP (`Connection: close`, measures full connect + request + response per query)\n");
    out.push_str("- **Timing Boundary**: Timed HTTP query loop separated from ground-truth BFS oracle verification\n\n");

    out.push_str("| Run # | Import (ms) | Catch-up (ms) | Time-to-Ready (ms) | Query QPS | Latency p50 (µs) | Latency p95 (µs) | Oracle Agreement | Final Sampled RSS (MiB) | Sampled Max RSS (MiB)* | Restart Recovery (ms) |\n");
    out.push_str("| :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |\n");

    for m in &all_runs {
        let rec_str = m
            .recovery_duration_ms
            .map(|d| format!("{d:.2}"))
            .unwrap_or_else(|| "N/A".to_string());
        let sampled_mib = m.sampled_rss_kib as f64 / 1024.0;
        let peak_mib = m.peak_rss_kib as f64 / 1024.0;

        out.push_str(&format!(
            "| **#{}** | `{:.2}` | `{:.2}` | `{:.2}` | `{:.1}` | `{:.1}` | `{:.1}` | `{}/{} ({:.2}%)` | `{:.2}` | `{:.2}` | `{}` |\n",
            m.run_index,
            m.snap_duration_ms,
            m.catchup_duration_ms,
            m.total_time_to_ready_ms,
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
        let avg_snap = all_runs.iter().map(|r| r.snap_duration_ms).sum::<f64>() / runs as f64;
        let avg_catchup = all_runs.iter().map(|r| r.catchup_duration_ms).sum::<f64>() / runs as f64;
        let avg_ttr = all_runs
            .iter()
            .map(|r| r.total_time_to_ready_ms)
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
            let sum_rec: f64 = all_runs.iter().filter_map(|r| r.recovery_duration_ms).sum();
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

    if let Some(path) = output_file {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(out.as_bytes())?;
        println!("✓ Evidence appended to {path}");
    }

    Ok(())
}
