//! Load tool for roadmap 0.9.3: feeder progress under query load.
//!
//! Implements the approved protocol of
//! `docs/integrations/feeder-under-load.md`: scenarios L1 (steady freshness)
//! and L2 (catch-up) on graphs G10k and G100k, with query clients that back off
//! on `503`, a rate-limited PostgreSQL writer and freshness markers. Every
//! observation uses a fresh database, a fresh Knotrel server and a fresh
//! feeder. Results are written as JSON lines, then summarized and evaluated
//! against criteria Q1 to Q4.
//!
//! ```text
//! KNOTREL_BIN=<knotrel-server> FEEDER_BIN=<knotrel-pg-feeder> \
//! KNOTREL_PG_ADMIN_URL=<admin url> LOAD_OUT=<dir> \
//!   cargo run --release --example load
//! ```
//!
//! `LOAD_REPS`, `LOAD_WINDOW_SECS`, `LOAD_BACKLOG` and `LOAD_CELLS` exist only
//! for smoke runs of the tool; a protocol run leaves them unset, and every
//! observation records the values it used.
//!
//! `LOAD_ENGINE` (`compact-bfs`, the default, or `cluster-forest`) selects the
//! local server's engine for the CF5 qualification of v1.1.0 (both run with
//! lifecycle support, as the protocol requires); every observation records it.
//!
//! # R2 two-Mac variant (proposed, opt-in)
//!
//! The section "R2 two-Mac variant" of the protocol document adds three arms
//! of scenario L2 G100k. They are selected with `LOAD_ARM`; when `LOAD_ARM`
//! and every other `LOAD_REMOTE_*`/`LOAD_CALIBRATION` variable are unset, the
//! tool behaves exactly as above.
//!
//! - `LOAD_ARM=S`: everything on this host (host K), loopback. Cells
//!   `L2 G100k C=0,16,64`. Adds per-process CPU accounting during catch-up,
//!   which calibrates arm U.
//! - `LOAD_ARM=T`: this host is host L. Every observation starts a fresh
//!   `knotrel-server` on host K over SSH (`LOAD_REMOTE_SSH=<destination>`),
//!   bound to `LOAD_REMOTE_ADDR` (host K's link address) and
//!   `LOAD_REMOTE_PORT` (default 18080) with `KNOTREL_ALLOW_NON_LOOPBACK=true`;
//!   `KNOTREL_BIN` is then the server's path on host K. The remote server's
//!   output is written to `server-<tag>.stderr.log`, and host K's CPU is
//!   sampled with `top` over SSH alongside this host's.
//! - `LOAD_ARM=U`: as T for `C=16,64`, plus busy-loop processes on host K
//!   (`LOAD_REMOTE_BURN_BIN`, the path of this example's binary on host K) at
//!   a duty cycle calibrated from arm S's `results.jsonl`
//!   (`LOAD_CALIBRATION`).
//!
//! Every arm measures 1,000 TCP round trips to the server's address before
//! its first and after its last observation (`link-check.json`), stops
//! starting observations after 45 minutes, and writes a descriptive summary
//! without the Q1–Q4 verdicts. `LOAD_R2_DECIDE=<dir>` applies the variant's
//! decision rule to `<dir>/S`, `<dir>/T` and `<dir>/U`. `LOAD_BURN_DUTY` runs
//! the busy loop itself (used on host K by arm U).

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

const EXTRA_BASE: i64 = 10_000_000;
const MARKER_BASE: i64 = 20_000_000;
const MARKERS: i64 = 100;
const WARMUP_L1: Duration = Duration::from_secs(15);
const WARMUP_L2: Duration = Duration::from_secs(5);
const WRITER_TXNS_PER_SEC: u64 = 200;
const CHANGES_PER_TXN: u64 = 5;
const MARKER_INTERVAL: Duration = Duration::from_millis(200);
const READY_TIMEOUT_L2: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug)]
struct Cell {
    scenario: &'static str,
    graph: &'static str,
    clients: usize,
}

const CELLS: &[Cell] = &[
    Cell {
        scenario: "L1",
        graph: "G100k",
        clients: 0,
    },
    Cell {
        scenario: "L1",
        graph: "G100k",
        clients: 16,
    },
    Cell {
        scenario: "L1",
        graph: "G100k",
        clients: 64,
    },
    Cell {
        scenario: "L1",
        graph: "G10k",
        clients: 16,
    },
    Cell {
        scenario: "L2",
        graph: "G100k",
        clients: 0,
    },
    Cell {
        scenario: "L2",
        graph: "G100k",
        clients: 16,
    },
    Cell {
        scenario: "L2",
        graph: "G100k",
        clients: 64,
    },
];

fn graph_size(graph: &str) -> (i64, i64) {
    match graph {
        "G10k" => (10_000, 30_000),
        _ => (100_000, 300_000),
    }
}

struct Settings {
    knotrel_bin: PathBuf,
    feeder_bin: PathBuf,
    admin_url: String,
    out: PathBuf,
    reps: usize,
    window: Duration,
    backlog: u64,
    cells: Option<Vec<String>>,
    /// The R2 variant's settings; `None` keeps the approved single-host tool.
    r2: Option<R2>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// xorshift64*: deterministic per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: i64) -> i64 {
        (self.next() % n as u64) as i64
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

async fn connect(url: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .unwrap_or_else(|e| panic!("cannot connect to PostgreSQL: {e}"));
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

fn with_database(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("URL has a database");
    format!("{base}/{db}")
}

fn host_port(url: &str) -> (String, u16) {
    let after = url.rsplit_once('@').map(|(_, r)| r).unwrap_or(url);
    let hp = after.split('/').next().unwrap();
    match hp.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap()),
        None => (hp.to_string(), 5432),
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn median(values: &[f64]) -> Option<f64> {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return None;
    }
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// One observation's environment.
struct Obs {
    admin_root: Client,
    admin: Client,
    db: String,
    role: String,
    password: String,
    prefix: String,
    server: Server,
    base: String,
    feeder: Option<Child>,
    feeder_log: PathBuf,
}

impl Obs {
    async fn setup(s: &Settings, cell: Cell, tag: &str) -> Self {
        let suffix = format!("{:08x}", rand_u64() & 0xFFFF_FFFF);
        let db = format!("kl_{suffix}");
        let role = format!("kl_{suffix}");
        let password = format!("{:016x}{:016x}", rand_u64(), rand_u64());
        let prefix = format!("kl_{suffix}");
        let admin_root = connect(&s.admin_url).await;
        admin_root
            .batch_execute(&format!("CREATE DATABASE {db}"))
            .await
            .unwrap();
        let admin = connect(&with_database(&s.admin_url, &db)).await;
        let sql_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sql");
        for f in ["schema.sql", "publication.sql"] {
            admin
                .batch_execute(&std::fs::read_to_string(sql_dir.join(f)).unwrap())
                .await
                .unwrap();
        }
        admin
            .batch_execute(&format!(
                "CREATE ROLE {role} LOGIN REPLICATION PASSWORD '{password}';
                 GRANT CONNECT ON DATABASE {db} TO {role};
                 GRANT USAGE ON SCHEMA knotrel_graph TO {role};
                 GRANT SELECT ON knotrel_graph.nodes, knotrel_graph.edges TO {role};"
            ))
            .await
            .unwrap();
        let (n, e) = graph_size(cell.graph);
        admin
            .batch_execute(&format!(
                "SELECT setseed(0.42);
                 INSERT INTO knotrel_graph.nodes SELECT g, random() >= 0.01 FROM generate_series(0, {last}) g;
                 INSERT INTO knotrel_graph.nodes SELECT g, true FROM generate_series({MARKER_BASE}, {mlast}) g;
                 INSERT INTO knotrel_graph.edges
                   SELECT a, b, random() >= 0.01 FROM (
                     SELECT DISTINCT ON (a, b) a, b, rn FROM (
                       SELECT least(x, y) a, greatest(x, y) b, rn FROM (
                         SELECT (random() * {last})::int8 x, (random() * {last})::int8 y, g rn
                         FROM generate_series(1, {gen}) g) r
                       WHERE x <> y) s
                     ORDER BY a, b, rn) d
                   ORDER BY rn LIMIT {e};
                 ANALYZE knotrel_graph.nodes; ANALYZE knotrel_graph.edges;",
                last = n - 1,
                mlast = MARKER_BASE + MARKERS - 1,
                gen = e * 11 / 10,
            ))
            .await
            .unwrap();
        let (server, base) = match s.r2.as_ref().and_then(|r| r.remote.as_ref()) {
            None => {
                let port = free_port();
                let server = start_local_server(&s.knotrel_bin, port);
                (Server::Local(server), format!("http://127.0.0.1:{port}"))
            }
            Some(remote) => {
                let log = s.out.join(format!("server-{tag}.stderr.log"));
                let server = RemoteProcess::start_server(remote, &s.knotrel_bin, &log).await;
                (Server::Remote(server), base_url(remote.socket_addr()))
            }
        };
        let feeder_log = s.out.join(format!("feeder-{tag}.log"));
        let mut obs = Self {
            admin_root,
            admin,
            db,
            role,
            password,
            prefix,
            server,
            base,
            feeder: None,
            feeder_log,
        };
        wait_http(&format!("{}/health", obs.base), Duration::from_secs(10)).await;
        obs.start_feeder(s, &s.admin_url.clone());
        obs.wait_ready(Duration::from_secs(300)).await;
        obs
    }

    fn start_feeder(&mut self, s: &Settings, admin_url: &str) {
        let (host, port) = host_port(admin_url);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.feeder_log)
            .unwrap();
        let child = Command::new(&s.feeder_bin)
            .env_clear()
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "info")
            .env("KNOTREL_PG_HOST", host)
            .env("KNOTREL_PG_PORT", port.to_string())
            .env("KNOTREL_PG_USER", &self.role)
            .env("KNOTREL_PG_PASSWORD", &self.password)
            .env("KNOTREL_PG_DATABASE", &self.db)
            .env("KNOTREL_PG_SLOT_PREFIX", &self.prefix)
            .env("KNOTREL_URL", &self.base)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn feeder");
        self.feeder = Some(child);
    }

    async fn stop_feeder(&mut self) {
        if let Some(mut child) = self.feeder.take() {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
            let _ = child.kill().await;
        }
    }

    async fn ready(&self) -> bool {
        reqwest::get(format!("{}/ready", self.base))
            .await
            .is_ok_and(|r| r.status().as_u16() == 200)
    }

    async fn wait_ready(&self, timeout: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.ready().await {
                return Some(start.elapsed());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    async fn retained_bytes(&self) -> Option<i64> {
        self.admin
            .query_opt(
                "SELECT pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)::int8 \
                 FROM pg_replication_slots WHERE left(slot_name::text, length($1)) = $1 AND active",
                &[&format!("{}_", self.prefix)],
            )
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get(0))
    }

    async fn cleanup(mut self) {
        self.stop_feeder().await;
        self.server.stop().await;
        let _ = self
            .admin_root
            .execute(
                "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
                 WHERE left(slot_name::text, length($1)) = $1 AND active_pid IS NOT NULL",
                &[&format!("{}_", self.prefix)],
            )
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = self
            .admin_root
            .execute(
                "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
                 WHERE left(slot_name::text, length($1)) = $1",
                &[&format!("{}_", self.prefix)],
            )
            .await;
        let _ = self
            .admin_root
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE); DROP ROLE IF EXISTS {};",
                self.db, self.role
            ))
            .await;
    }
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    h.finish()
}

async fn wait_http(url: &str, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if reqwest::get(url)
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{url} did not answer");
}

/// SQL for one writer transaction (5 changes; protocol "Writer").
fn writer_txn(rng: &mut Rng, n: i64) -> String {
    let mut sql = String::from("BEGIN;");
    for _ in 0..CHANGES_PER_TXN {
        let r = rng.below(n);
        let p = rng.unit();
        let stmt = if p < 0.30 {
            format!("UPDATE knotrel_graph.nodes SET active = NOT active WHERE id = {r};")
        } else if p < 0.60 {
            format!(
                "UPDATE knotrel_graph.edges SET enabled = NOT enabled WHERE (source, target) = \
                 (SELECT source, target FROM knotrel_graph.edges WHERE source >= {r} ORDER BY source, target LIMIT 1);"
            )
        } else if p < 0.75 {
            let b = rng.below(n);
            let (s, t) = (r.min(b), r.max(b));
            if s == t {
                continue;
            }
            format!(
                "INSERT INTO knotrel_graph.edges SELECT {s}, {t}, true \
                 WHERE EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = {s}) \
                   AND EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = {t}) ON CONFLICT DO NOTHING;"
            )
        } else if p < 0.90 {
            format!(
                "DELETE FROM knotrel_graph.edges WHERE (source, target) = \
                 (SELECT source, target FROM knotrel_graph.edges WHERE source >= {r} ORDER BY source, target LIMIT 1);"
            )
        } else if p < 0.95 {
            format!(
                "INSERT INTO knotrel_graph.nodes VALUES ({}, true) ON CONFLICT DO NOTHING;",
                EXTRA_BASE + r % 10_000
            )
        } else {
            format!(
                "DELETE FROM knotrel_graph.nodes WHERE id = {};",
                EXTRA_BASE + r % 10_000
            )
        };
        sql.push_str(&stmt);
    }
    sql.push_str("COMMIT;");
    sql
}

#[derive(Default)]
struct QueryStats {
    latencies_ms: Vec<f64>,
    refused: u64,
    errors: u64,
}

fn backoff(attempt: u32, start_ms: f64, rng: &mut Rng) -> Duration {
    let base = (start_ms * 2f64.powi(attempt.min(10) as i32)).min(200.0);
    let jitter = 0.5 + rng.unit();
    Duration::from_micros((base * jitter * 1000.0) as u64)
}

fn spawn_clients(
    base: &str,
    clients: usize,
    n: i64,
    stop: Arc<AtomicBool>,
    record: Arc<AtomicBool>,
) -> Vec<tokio::task::JoinHandle<QueryStats>> {
    (0..clients)
        .map(|c| {
            let url = format!("{base}/v1/batch");
            let stop = stop.clone();
            let record = record.clone();
            tokio::spawn(async move {
                let http = reqwest::Client::builder()
                    .pool_max_idle_per_host(1)
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap();
                let mut rng = Rng(0x5EED_0000 + c as u64);
                let mut stats = QueryStats::default();
                let mut attempt = 0;
                while !stop.load(Ordering::Relaxed) {
                    let ops: Vec<Value> = (0..8)
                        .map(|_| {
                            json!({"op": "connected", "source": rng.below(n).to_string(),
                                   "target": rng.below(n).to_string()})
                        })
                        .collect();
                    let t = Instant::now();
                    let res = http
                        .post(&url)
                        .json(&json!({"operations": ops}))
                        .send()
                        .await;
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    let on = record.load(Ordering::Relaxed);
                    match res.map(|r| r.status().as_u16()) {
                        Ok(200) => {
                            attempt = 0;
                            if on {
                                stats.latencies_ms.push(ms);
                            }
                        }
                        Ok(503) => {
                            if on {
                                stats.refused += 1;
                            }
                            tokio::time::sleep(backoff(attempt, 10.0, &mut rng)).await;
                            attempt += 1;
                        }
                        _ => {
                            if on {
                                stats.errors += 1;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
                stats
            })
        })
        .collect()
}

fn summarize_queries(all: Vec<QueryStats>, window: Duration) -> Value {
    let mut lat: Vec<f64> = all.iter().flat_map(|s| s.latencies_ms.clone()).collect();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let refused: u64 = all.iter().map(|s| s.refused).sum();
    let errors: u64 = all.iter().map(|s| s.errors).sum();
    json!({
        "completed": lat.len(),
        "requests_per_sec": lat.len() as f64 / window.as_secs_f64(),
        "p50_ms": percentile(&lat, 50.0),
        "p99_ms": percentile(&lat, 99.0),
        "refused_503": refused,
        "errors": errors,
    })
}

/// Marker: flip a marker node, then poll Knotrel until it reflects the flip.
/// Returns (commit time since window start in seconds, lag in ms or None).
async fn marker(
    pg: Arc<Mutex<Client>>,
    base: String,
    id: i64,
    window_start: Instant,
    seed: u64,
) -> (f64, Option<f64>) {
    let expected: bool = {
        let c = pg.lock().await;
        c.query_one(
            "UPDATE knotrel_graph.nodes SET active = NOT active WHERE id = $1 RETURNING active",
            &[&id],
        )
        .await
        .unwrap()
        .get(0)
    };
    let committed = Instant::now();
    let at = committed
        .saturating_duration_since(window_start)
        .as_secs_f64()
        - if committed < window_start {
            window_start.duration_since(committed).as_secs_f64()
        } else {
            0.0
        };
    let http = reqwest::Client::new();
    let mut rng = Rng(seed);
    let mut attempt = 0;
    while committed.elapsed() < Duration::from_secs(30) {
        let res = http
            .post(format!("{base}/v1/operations"))
            .json(&json!({"op": "node_state", "node": id.to_string()}))
            .send()
            .await;
        match res {
            Ok(r) if r.status().as_u16() == 200 => {
                attempt = 0;
                let body: Value = r.json().await.unwrap_or(Value::Null);
                if body["active"].as_bool() == Some(expected) {
                    return (at, Some(committed.elapsed().as_secs_f64() * 1000.0));
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(r) if r.status().as_u16() == 503 => {
                tokio::time::sleep(backoff(attempt, 1.0, &mut rng)).await;
                attempt += 1;
            }
            _ => tokio::time::sleep(Duration::from_millis(2)).await,
        }
    }
    (at, None)
}

/// Sums and counts of the three graph-job phase histograms per `kind`.
async fn phase_metrics(base: &str) -> BTreeMap<String, f64> {
    let text = match reqwest::get(format!("{base}/metrics")).await {
        Ok(r) => r.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    };
    let mut out = BTreeMap::new();
    for line in text.lines() {
        for family in [
            "knotrel_graph_job_queue_seconds",
            "knotrel_graph_lock_wait_seconds",
            "knotrel_graph_job_execution_seconds",
        ] {
            for suffix in ["_sum", "_count"] {
                let prefix = format!("{family}{suffix}{{");
                if let Some(rest) = line.strip_prefix(&prefix)
                    && let Some((labels, value)) = rest.split_once("} ")
                    && let Some(kind) = labels
                        .split(',')
                        .find_map(|l| l.strip_prefix("kind=\"").and_then(|v| v.strip_suffix('"')))
                    && let Ok(v) = value.trim().parse::<f64>()
                {
                    *out.entry(format!("{family}{suffix}|{kind}")).or_insert(0.0) += v;
                }
            }
        }
    }
    out
}

/// Mean milliseconds per job of each phase and kind between two scrapes.
fn phase_breakdown(before: &BTreeMap<String, f64>, after: &BTreeMap<String, f64>) -> Value {
    let mut kinds = serde_json::Map::new();
    for kind in ["feeder_events", "batch", "operation"] {
        let mut phases = serde_json::Map::new();
        for (name, family) in [
            ("admission_queue_ms", "knotrel_graph_job_queue_seconds"),
            ("lock_wait_ms", "knotrel_graph_lock_wait_seconds"),
            ("execution_ms", "knotrel_graph_job_execution_seconds"),
        ] {
            let d = |suffix: &str| {
                let key = format!("{family}{suffix}|{kind}");
                after.get(&key).copied().unwrap_or(0.0) - before.get(&key).copied().unwrap_or(0.0)
            };
            let (sum, count) = (d("_sum"), d("_count"));
            phases.insert(
                name.into(),
                json!({"jobs": count, "total_s": sum,
                       "mean_ms": if count > 0.0 { Some(sum * 1000.0 / count) } else { None }}),
            );
        }
        kinds.insert(kind.into(), Value::Object(phases));
    }
    Value::Object(kinds)
}

/// Host CPU busy fraction, sampled every second by `top` (macOS) while
/// `stop` is false. Returns the samples (percent busy over all CPUs).
fn spawn_cpu_sampler(stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<Vec<(Instant, f64)>> {
    let mut top = Command::new("top");
    top.args(TOP_ARGS);
    spawn_cpu_sampler_with(stop, top)
}

/// `top` in logging mode: one sample per second, no process list.
const TOP_ARGS: [&str; 6] = ["-l", "0", "-s", "1", "-n", "0"];

/// As [`spawn_cpu_sampler`], with `top` started by `command` (for example
/// over SSH on host K). Sample times are the local arrival times.
fn spawn_cpu_sampler_with(
    stop: Arc<AtomicBool>,
    mut command: Command,
) -> tokio::task::JoinHandle<Vec<(Instant, f64)>> {
    tokio::spawn(async move {
        let mut samples = Vec::new();
        let Ok(mut child) = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        else {
            return samples;
        };
        let Some(stdout) = child.stdout.take() else {
            return samples;
        };
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut first = true;
        while !stop.load(Ordering::Relaxed) {
            match tokio::time::timeout(Duration::from_millis(1500), lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    if let Some(idle) = line
                        .strip_prefix("CPU usage:")
                        .and_then(|l| l.split(',').find(|f| f.contains("idle")))
                        .and_then(|f| f.trim().split('%').next())
                        .and_then(|v| v.trim().parse::<f64>().ok())
                    {
                        // The first `top` sample covers the time since boot.
                        if !first {
                            samples.push((Instant::now(), 100.0 - idle));
                        }
                        first = false;
                    }
                }
                Ok(_) => break,
                Err(_) => {}
            }
        }
        let _ = child.kill().await;
        samples
    })
}

fn count_feeder_refusals(log: &str) -> usize {
    log.lines()
        .filter(|l| l.contains("events refused") && (l.contains("503") || l.contains("busy")))
        .count()
}

async fn run_l1(s: &Settings, cell: Cell, obs: &Obs) -> Value {
    let (n, _) = graph_size(cell.graph);
    let stop = Arc::new(AtomicBool::new(false));
    let record = Arc::new(AtomicBool::new(false));
    let clients = spawn_clients(&obs.base, cell.clients, n, stop.clone(), record.clone());

    // Writer: 200 transactions/s.
    let writer_pg = connect(&with_database(&s.admin_url, &obs.db)).await;
    let writer_stop = stop.clone();
    let writer_txns = Arc::new(AtomicU64::new(0));
    let writer_count = writer_txns.clone();
    let writer = tokio::spawn(async move {
        let mut rng = Rng(0xFEED);
        let mut tick =
            tokio::time::interval(Duration::from_micros(1_000_000 / WRITER_TXNS_PER_SEC));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !writer_stop.load(Ordering::Relaxed) {
            tick.tick().await;
            if writer_pg
                .batch_execute(&writer_txn(&mut rng, n))
                .await
                .is_ok()
            {
                writer_count.fetch_add(1, Ordering::Relaxed);
            } else {
                let _ = writer_pg.batch_execute("ROLLBACK").await;
            }
        }
    });

    tokio::time::sleep(WARMUP_L1).await;
    let window_start = Instant::now();
    record.store(true, Ordering::Relaxed);
    let txns_at_start = writer_txns.load(Ordering::Relaxed);
    let retained_start = obs.retained_bytes().await;

    // Markers and samplers during the window.
    let marker_pg = Arc::new(Mutex::new(
        connect(&with_database(&s.admin_url, &obs.db)).await,
    ));
    let mut markers = Vec::new();
    let mut samples = Vec::new();
    let mut k = 0_i64;
    let mut next_sample = window_start;
    while window_start.elapsed() < s.window {
        if Instant::now() >= next_sample {
            let stats: Value = match reqwest::get(format!("{}/v1/stats", obs.base)).await {
                Ok(r) => r.json().await.unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            samples.push(json!({
                "t": window_start.elapsed().as_secs_f64(),
                "source_position": stats["source_position"],
                "sync_status": stats["sync_status"],
                "retained_bytes": obs.retained_bytes().await,
            }));
            next_sample += Duration::from_secs(1);
        }
        markers.push(tokio::spawn(marker(
            marker_pg.clone(),
            obs.base.clone(),
            MARKER_BASE + k % MARKERS,
            window_start,
            k as u64 + 1,
        )));
        k += 1;
        tokio::time::sleep(MARKER_INTERVAL).await;
    }
    let window = window_start.elapsed();
    record.store(false, Ordering::Relaxed);
    let txns = writer_txns.load(Ordering::Relaxed) - txns_at_start;
    let retained_end = obs.retained_bytes().await;
    let mut lags: Vec<(f64, Option<f64>)> = Vec::new();
    for m in markers {
        lags.push(m.await.unwrap());
    }
    stop.store(true, Ordering::Relaxed);
    let _ = writer.await;
    let mut query_stats = Vec::new();
    for c in clients {
        query_stats.push(c.await.unwrap());
    }

    let timeouts = lags.iter().filter(|(_, l)| l.is_none()).count();
    let mut all: Vec<f64> = lags.iter().map(|(_, l)| l.unwrap_or(30_000.0)).collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let w = s.window.as_secs_f64();
    let first: Vec<f64> = lags
        .iter()
        .filter(|(t, _)| *t < 10.0)
        .map(|(_, l)| l.unwrap_or(30_000.0))
        .collect();
    let last: Vec<f64> = lags
        .iter()
        .filter(|(t, _)| *t >= w - 10.0)
        .map(|(_, l)| l.unwrap_or(30_000.0))
        .collect();
    let log = std::fs::read_to_string(&obs.feeder_log).unwrap_or_default();
    json!({
        "markers": {
            "count": all.len(),
            "timeouts": timeouts,
            "p50_ms": percentile(&all, 50.0),
            "p99_ms": percentile(&all, 99.0),
            "max_ms": all.last(),
            "first10_median_ms": median(&first),
            "last10_median_ms": median(&last),
        },
        "retained_bytes": {"start": retained_start, "end": retained_end},
        "writer": {"txns": txns, "txns_per_sec": txns as f64 / window.as_secs_f64(),
                   "changes_per_sec": (txns * CHANGES_PER_TXN) as f64 / window.as_secs_f64()},
        "queries": summarize_queries(query_stats, window),
        "feeder_503_refusals": count_feeder_refusals(&log),
        "samples": samples,
    })
}

async fn run_l2(s: &Settings, cell: Cell, obs: &mut Obs) -> Value {
    let (n, _) = graph_size(cell.graph);
    obs.stop_feeder().await;
    let log_before = std::fs::read_to_string(&obs.feeder_log)
        .unwrap_or_default()
        .len();
    // Backlog: unthrottled writer until `backlog` changes.
    let writer_pg = connect(&with_database(&s.admin_url, &obs.db)).await;
    let mut rng = Rng(0xBAC4);
    let txns_needed = s.backlog / CHANGES_PER_TXN;
    let backlog_start = Instant::now();
    let mut done = 0;
    while done < txns_needed {
        if writer_pg
            .batch_execute(&writer_txn(&mut rng, n))
            .await
            .is_ok()
        {
            done += 1;
        } else {
            let _ = writer_pg.batch_execute("ROLLBACK").await;
        }
    }
    let backlog_secs = backlog_start.elapsed().as_secs_f64();
    // Knotrel stays `ready` until the stopped feeder's lease lapses. Wait for
    // `stale`, so that `ready` is reached again only through the barrier the
    // restarted feeder declares after catching up.
    let lapse_start = Instant::now();
    while obs.ready().await {
        assert!(
            lapse_start.elapsed() < Duration::from_secs(60),
            "Knotrel stayed ready after the feeder stopped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let record = Arc::new(AtomicBool::new(false));
    // R2 arm U: the calibrated CPU load on host K runs from before the
    // warm-up until after the catch-up.
    let burners = match &s.r2 {
        Some(r2) => start_burners(r2, cell, &s.out).await,
        None => None,
    };
    let cpu_stop = Arc::new(AtomicBool::new(false));
    let cpu = spawn_cpu_sampler(cpu_stop.clone());
    let remote_cpu =
        s.r2.as_ref()
            .and_then(|r| r.remote.as_ref())
            .map(|remote| spawn_cpu_sampler_with(cpu_stop.clone(), remote_top_command(remote)));
    let clients = spawn_clients(&obs.base, cell.clients, n, stop.clone(), record.clone());
    tokio::time::sleep(WARMUP_L2).await;
    record.store(true, Ordering::Relaxed);
    let ps_before = match &s.r2 {
        Some(r2) => Some(ps_snapshots(r2).await),
        None => None,
    };
    let metrics_before = phase_metrics(&obs.base).await;
    let start = Instant::now();
    obs.start_feeder(s, &s.admin_url);
    let feeder_pid = obs.feeder.as_ref().and_then(|c| c.id());
    // Ready is reached when the restarted feeder's barrier is accepted.
    let mut to_ready = None;
    while start.elapsed() < READY_TIMEOUT_L2 {
        let log = std::fs::read_to_string(&obs.feeder_log).unwrap_or_default();
        if log[log_before.min(log.len())..].contains("caught up: barrier accepted") {
            to_ready = Some(start.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        to_ready.is_none() || obs.ready().await,
        "barrier accepted but Knotrel is not ready"
    );
    let elapsed = start.elapsed();
    record.store(false, Ordering::Relaxed);
    let metrics_after = phase_metrics(&obs.base).await;
    let ps_after = match &s.r2 {
        Some(r2) => Some(ps_snapshots(r2).await),
        None => None,
    };
    // Keep sampling one more second so a short catch-up still gets a sample
    // that ends after it.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    cpu_stop.store(true, Ordering::Relaxed);
    let all_cpu = cpu.await.unwrap_or_default();
    let remote_cpu = match remote_cpu {
        Some(handle) => Some(handle.await.unwrap_or_default()),
        None => None,
    };
    let burner_pids = burners.as_ref().map(|b| b.pids()).unwrap_or_default();
    if let Some(mut b) = burners {
        b.stop().await;
    }
    let warmup_cpu: Vec<f64> = all_cpu
        .iter()
        .filter(|(t, _)| *t <= start)
        .map(|(_, v)| *v)
        .collect();
    let cpu_samples: Vec<f64> = all_cpu
        .iter()
        .filter(|(t, _)| *t > start && *t <= start + elapsed + Duration::from_millis(1100))
        .map(|(_, v)| *v)
        .collect();
    stop.store(true, Ordering::Relaxed);
    let mut query_stats = Vec::new();
    for c in clients {
        query_stats.push(c.await.unwrap());
    }
    let log = std::fs::read_to_string(&obs.feeder_log).unwrap_or_default();
    let tail = &log[log_before.min(log.len())..];
    let events = tail
        .lines()
        .filter(|l| l.contains("caught up: barrier accepted"))
        .filter_map(|l| {
            l.split_whitespace()
                .find_map(|f| f.strip_prefix("applied="))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .next_back();
    let secs = to_ready.map(|d| d.as_secs_f64());
    let mut out = json!({
        "backlog_changes": s.backlog,
        "backlog_write_secs": backlog_secs,
        "events": events,
        "secs_to_ready": secs,
        "events_per_sec": match (events, secs) { (Some(e), Some(t)) if t > 0.0 => Some(e as f64 / t), _ => None },
        "queries": summarize_queries(query_stats, elapsed),
        "feeder_503_refusals": count_feeder_refusals(tail),
        // The feeder waits 200 ms after each refusal before it retries.
        "feeder_admission_backoff_s_estimate": count_feeder_refusals(tail) as f64 * 0.2,
        "phases": phase_breakdown(&metrics_before, &metrics_after),
        "host_cpu_busy_percent_warmup": {"samples": warmup_cpu.len(), "median": median(&warmup_cpu)},
        "host_cpu_busy_percent": {
            "samples": cpu_samples.len(),
            "median": median(&cpu_samples),
            "max": cpu_samples.iter().cloned().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
        },
    });
    if let (Some(r2), Some(obj)) = (&s.r2, out.as_object_mut()) {
        let end = start + elapsed + Duration::from_millis(1100);
        if let Some(remote) = &remote_cpu {
            let (warmup, catch_up) = cpu_windows(remote, start, end);
            obj.insert("remote_host_cpu_busy_percent_warmup".into(), warmup);
            obj.insert("remote_host_cpu_busy_percent".into(), catch_up);
        }
        if let (Some(before), Some(after)) = (ps_before, ps_after) {
            let window = after.at.duration_since(before.at).as_secs_f64();
            let remote = r2.remote.is_some();
            let server_pid = obs.server.pid();
            let local = Roles {
                knotrel: if remote { None } else { server_pid },
                feeder: feeder_pid,
                load_tool: Some(std::process::id()),
                burners: Vec::new(),
                postgres_patterns: r2.postgres_patterns.clone(),
            };
            let local = process_cpu(&before.local, &after.local, &local, window);
            let remote_groups = match (&before.remote, &after.remote) {
                (Some(b), Some(a)) => {
                    let roles = Roles {
                        knotrel: server_pid,
                        feeder: None,
                        load_tool: None,
                        burners: burner_pids.clone(),
                        postgres_patterns: Vec::new(),
                    };
                    Some(process_cpu(b, a, &roles, window))
                }
                _ => None,
            };
            let non_knotrel = if remote {
                remote_groups.as_ref().map(|g| g.cores("burners"))
            } else {
                Some(local.cores("postgres") + local.cores("feeder") + local.cores("load_tool"))
            };
            obj.insert(
                "process_cpu".into(),
                json!({
                    "window_secs": window,
                    "local": local.to_json(),
                    "remote": remote_groups.as_ref().map(ProcessCpu::to_json),
                }),
            );
            obj.insert("host_k_non_knotrel_cores".into(), json!(non_knotrel));
        }
        if let Some(plan) = r2.burn.get(&cell.clients) {
            obj.insert(
                "burn".into(),
                json!({"processes": plan.processes, "duty": plan.duty,
                       "target_cores": plan.target_cores, "capped": plan.capped,
                       "pids": burner_pids.len()}),
            );
        }
    }
    out
}

fn cell_key(c: &Value) -> String {
    format!(
        "{} {} C={}",
        c["scenario"].as_str().unwrap(),
        c["graph"].as_str().unwrap(),
        c["clients"]
    )
}

fn get(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_f64()
}

/// Per-cell medians and ranges, then the criteria of the approved protocol.
fn evaluate(results: &[Value]) -> Value {
    let mut cells: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for r in results {
        cells.entry(cell_key(r)).or_default().push(r);
    }
    let stat = |key: &str, path: &[&str]| -> Value {
        let vals: Vec<f64> = cells
            .get(key)
            .map(|v| v.iter().filter_map(|r| get(r, path)).collect())
            .unwrap_or_default();
        let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        json!({"median": median(&vals), "min": if vals.is_empty() { None } else { Some(min) },
               "max": if vals.is_empty() { None } else { Some(max) }, "n": vals.len()})
    };
    let mut summary = serde_json::Map::new();
    for key in cells.keys() {
        let mut m = serde_json::Map::new();
        if key.starts_with("L1") {
            for (name, path) in [
                ("marker_p50_ms", &["markers", "p50_ms"][..]),
                ("marker_p99_ms", &["markers", "p99_ms"][..]),
                ("marker_max_ms", &["markers", "max_ms"][..]),
                ("marker_timeouts", &["markers", "timeouts"][..]),
                ("first10_median_ms", &["markers", "first10_median_ms"][..]),
                ("last10_median_ms", &["markers", "last10_median_ms"][..]),
                ("writer_changes_per_sec", &["writer", "changes_per_sec"][..]),
                ("query_rps", &["queries", "requests_per_sec"][..]),
                ("query_p99_ms", &["queries", "p99_ms"][..]),
                ("query_503", &["queries", "refused_503"][..]),
                ("feeder_503_refusals", &["feeder_503_refusals"][..]),
            ] {
                m.insert(name.into(), stat(key, path));
            }
        } else {
            for (name, path) in [
                ("events", &["events"][..]),
                ("secs_to_ready", &["secs_to_ready"][..]),
                ("events_per_sec", &["events_per_sec"][..]),
                ("query_rps", &["queries", "requests_per_sec"][..]),
                ("query_p99_ms", &["queries", "p99_ms"][..]),
                ("feeder_503_refusals", &["feeder_503_refusals"][..]),
            ] {
                m.insert(name.into(), stat(key, path));
            }
        }
        summary.insert(key.clone(), Value::Object(m));
    }
    let med = |key: &str, name: &str| -> Option<f64> {
        summary.get(key)?.get(name)?.get("median")?.as_f64()
    };
    // Q1
    let q1_p99 = med("L1 G100k C=16", "marker_p99_ms");
    let q1_max = med("L1 G100k C=16", "marker_max_ms");
    let q1 = matches!((q1_p99, q1_max), (Some(p), Some(m)) if p <= 1000.0 && m <= 3000.0);
    // Q2: every L1 observation, per observation.
    let mut q2 = true;
    let mut q2_detail = Vec::new();
    for r in results.iter().filter(|r| r["scenario"] == "L1") {
        let first = get(r, &["markers", "first10_median_ms"]);
        let last = get(r, &["markers", "last10_median_ms"]);
        let rs = get(r, &["retained_bytes", "start"]);
        let re = get(r, &["retained_bytes", "end"]);
        let lag_ok = matches!((first, last), (Some(f), Some(l)) if l <= 2.0 * f + 50.0);
        let wal_ok = matches!((rs, re), (Some(s), Some(e)) if e <= s + 16.0 * 1024.0 * 1024.0);
        q2 &= lag_ok && wal_ok;
        q2_detail.push(
            json!({"cell": cell_key(r), "rep": r["rep"], "lag_ok": lag_ok, "wal_ok": wal_ok}),
        );
    }
    // Q3
    let r0 = med("L2 G100k C=0", "events_per_sec");
    let r16 = med("L2 G100k C=16", "events_per_sec");
    let r64 = med("L2 G100k C=64", "events_per_sec");
    let q3 = matches!((r0, r16, r64), (Some(a), Some(b), Some(c)) if b >= 0.5 * a && c >= 0.25 * a);
    // Q4
    let t64 = med("L2 G100k C=64", "secs_to_ready");
    let q4 = matches!(t64, Some(t) if t <= 60.0);
    json!({
        "cells": summary,
        "criteria": {
            "Q1": {"pass": q1, "marker_p99_ms": q1_p99, "marker_max_ms": q1_max,
                   "rule": "L1 G100k C=16: p99 <= 1000 ms and max <= 3000 ms"},
            "Q2": {"pass": q2, "observations": q2_detail,
                   "rule": "every L1 observation: last10 median <= 2 x first10 median + 50 ms, retained WAL end <= start + 16 MiB"},
            "Q3": {"pass": q3, "rate_c0": r0, "rate_c16": r16, "rate_c64": r64,
                   "ratio_c16": r16.zip(r0).map(|(b, a)| b / a), "ratio_c64": r64.zip(r0).map(|(c, a)| c / a),
                   "rule": "L2 G100k: C=16 >= 50 % and C=64 >= 25 % of C=0"},
            "Q4": {"pass": q4, "secs_to_ready_c64": t64, "rule": "L2 G100k C=64: time to ready <= 60 s"},
        }
    })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Some((duty, max)) = parse_burn(&env).unwrap_or_else(|e| panic!("{e}")) {
        burn(duty, max);
        return;
    }
    if let Some(dir) = env("LOAD_R2_DECIDE") {
        let decision = decide_dir(Path::new(&dir));
        let text = serde_json::to_string_pretty(&decision).unwrap();
        std::fs::write(Path::new(&dir).join("decision.json"), &text).unwrap();
        println!("{text}");
        return;
    }
    let mut s = Settings {
        knotrel_bin: PathBuf::from(env("KNOTREL_BIN").expect("KNOTREL_BIN")),
        feeder_bin: PathBuf::from(env("FEEDER_BIN").expect("FEEDER_BIN")),
        admin_url: env("KNOTREL_PG_ADMIN_URL").expect("KNOTREL_PG_ADMIN_URL"),
        out: PathBuf::from(env("LOAD_OUT").expect("LOAD_OUT")),
        reps: env("LOAD_REPS").map(|v| v.parse().unwrap()).unwrap_or(3),
        window: Duration::from_secs(
            env("LOAD_WINDOW_SECS")
                .map(|v| v.parse().unwrap())
                .unwrap_or(60),
        ),
        backlog: env("LOAD_BACKLOG")
            .map(|v| v.parse().unwrap())
            .unwrap_or(100_000),
        cells: env("LOAD_CELLS").map(|v| v.split(',').map(str::to_string).collect()),
        r2: parse_r2(&env).unwrap_or_else(|e| panic!("{e}")),
    };
    std::fs::create_dir_all(&s.out).unwrap();
    if let Some(r2) = s.r2.as_mut() {
        prepare_r2(r2, &s.out).await;
    }
    let link_before = match &s.r2 {
        Some(r2) => Some(link_check(&s, r2, "before").await),
        None => None,
    };
    let results_path = s.out.join("results.jsonl");
    let run_start = Instant::now();
    let mut results = Vec::new();
    let mut budget_exceeded = false;
    'reps: for rep in 1..=s.reps {
        for (i, cell) in CELLS.iter().enumerate() {
            let key = format!("{} {} C={}", cell.scenario, cell.graph, cell.clients);
            if s.cells
                .as_ref()
                .is_some_and(|c| !c.iter().any(|k| k == &key))
            {
                continue;
            }
            if let Some(r2) = &s.r2 {
                if !arm_includes(r2.arm, cell) {
                    continue;
                }
                if run_start.elapsed() >= ARM_BUDGET {
                    eprintln!("budget of {} s exceeded: stopping", ARM_BUDGET.as_secs());
                    budget_exceeded = true;
                    break 'reps;
                }
            }
            let tag = format!("r{rep}-c{i}");
            eprintln!(
                "[{:>6.0}s] rep {rep} cell {key}",
                run_start.elapsed().as_secs_f64()
            );
            let mut obs = Obs::setup(&s, *cell, &tag).await;
            let metrics = if cell.scenario == "L1" {
                run_l1(&s, *cell, &obs).await
            } else {
                run_l2(&s, *cell, &mut obs).await
            };
            obs.cleanup().await;
            let mut record = json!({
                "scenario": cell.scenario, "graph": cell.graph, "clients": cell.clients, "rep": rep,
                "window_secs": s.window.as_secs(), "backlog": s.backlog,
                "engine": load_engine(),
                "metrics": metrics,
            });
            if let (Some(r2), Some(obj)) = (&s.r2, record.as_object_mut()) {
                obj.insert("arm".into(), json!(r2.arm.name()));
                obj.insert("layout".into(), json!(r2.layout()));
                obj.insert(
                    "server_addr".into(),
                    json!(r2.remote.as_ref().map(|r| r.socket_addr().to_string())),
                );
            }
            // Flatten metrics for evaluation.
            let flat = flatten(&record);
            let line = serde_json::to_string(&record).unwrap();
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&results_path)
                .unwrap();
            writeln!(f, "{line}").unwrap();
            results.push(flat);
        }
    }
    if let Some(r2) = &s.r2 {
        let link_after = link_check(&s, r2, "after").await;
        let link = json!({"before": link_before, "after": link_after,
                          "rule": "valid if the median round trip is <= 0.5 ms"});
        std::fs::write(
            s.out.join("link-check.json"),
            serde_json::to_string_pretty(&link).unwrap(),
        )
        .unwrap();
        let summary_text = serde_json::to_string_pretty(&json!({
            "arm": r2.arm.name(),
            "layout": r2.layout(),
            "measurement_secs": run_start.elapsed().as_secs_f64(),
            "budget_secs": ARM_BUDGET.as_secs(),
            "budget_exceeded": budget_exceeded,
            "link_check": link,
            "calibration": r2.calibration,
            "summary": r2_summary(&results),
        }))
        .unwrap();
        std::fs::write(s.out.join("summary.json"), &summary_text).unwrap();
        println!("{summary_text}");
        return;
    }
    let summary = evaluate(&results);
    let summary_text = serde_json::to_string_pretty(&json!({
        "measurement_secs": run_start.elapsed().as_secs_f64(),
        "summary": summary,
    }))
    .unwrap();
    std::fs::write(s.out.join("summary.json"), &summary_text).unwrap();
    println!("{summary_text}");
}

// ---------------------------------------------------------------------------
// R2 two-Mac variant (proposed; see the section "R2 two-Mac variant" of
// `docs/integrations/feeder-under-load.md`). Nothing below runs unless
// `LOAD_ARM`, `LOAD_BURN_DUTY` or `LOAD_R2_DECIDE` is set.
// ---------------------------------------------------------------------------

/// Server settings of the approved protocol, besides the bind address.
const SERVER_ENV: [(&str, &str); 5] = [
    ("KNOTREL_MODE", "source-managed"),
    ("KNOTREL_TOPOLOGY_LIFECYCLE", "true"),
    ("KNOTREL_MAX_NODES", "unlimited"),
    ("KNOTREL_MAX_EDGES", "unlimited"),
    ("KNOTREL_METRICS_ENABLED", "true"),
];
/// Measurement budget of one R2 arm; no observation starts after it.
const ARM_BUDGET: Duration = Duration::from_secs(45 * 60);
/// TCP round trips per link check.
const LINK_ROUND_TRIPS: usize = 1_000;
/// Validity threshold of the link check (median round trip).
const LINK_MAX_MEDIAN_MS: f64 = 0.5;
/// Default port of the remote server on host K.
const DEFAULT_REMOTE_PORT: u16 = 18_080;
/// A busy-loop process exits on its own after this long if it is orphaned.
const DEFAULT_BURN_MAX_SECS: u64 = 900;
/// Period of the busy loop's duty cycle.
const BURN_PERIOD: Duration = Duration::from_millis(10);
/// Executable names counted as PostgreSQL on host K in arm S: the container's
/// virtual machine (Docker Desktop and similar) or a native server.
const DEFAULT_POSTGRES_PATTERNS: [&str; 7] = [
    "com.apple.Virtualization.VirtualMachine",
    "com.docker.krun",
    "com.docker.hyperkit",
    "qemu-system",
    "vfkit",
    "krunkit",
    "postgres",
];

/// The engine of the local server: `LOAD_ENGINE` (CF5 arms: `compact-bfs` or
/// `cluster-forest`, the two lifecycle engines), `compact-bfs` when unset,
/// which is the approved protocol's engine. Any other value stops the tool.
fn load_engine() -> String {
    let engine = std::env::var("LOAD_ENGINE").unwrap_or_else(|_| "compact-bfs".to_owned());
    assert!(
        matches!(engine.as_str(), "compact-bfs" | "cluster-forest"),
        "LOAD_ENGINE must be compact-bfs or cluster-forest (the protocol runs with lifecycle)"
    );
    engine
}

/// The server process of the approved single-host protocol on `127.0.0.1`.
fn start_local_server(bin: &Path, port: u16) -> Child {
    Command::new(bin)
        .env_clear()
        .env("KNOTREL_ADDR", format!("127.0.0.1:{port}"))
        .env("KNOTREL_ENGINE", load_engine())
        .envs(SERVER_ENV)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn knotrel-server")
}

fn base_url(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// The Knotrel server of one observation.
enum Server {
    Local(Child),
    Remote(RemoteProcess),
}

impl Server {
    /// The server's process ID on the host that runs it.
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Local(child) => child.id(),
            Self::Remote(remote) => remote.pids().first().copied(),
        }
    }

    async fn stop(&mut self) {
        match self {
            Self::Local(child) => {
                let _ = child.kill().await;
            }
            Self::Remote(remote) => remote.stop().await,
        }
    }
}

/// One of the variant's three arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// Control: everything on host K, loopback.
    S,
    /// Separated: Knotrel on host K, everything else on host L.
    T,
    /// As T, plus the calibrated CPU load on host K.
    U,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
        }
    }

    /// Query-client counts of the arm (scenario L2, graph G100k).
    fn clients(self) -> &'static [usize] {
        match self {
            Self::S | Self::T => &[0, 16, 64],
            Self::U => &[16, 64],
        }
    }
}

fn arm_includes(arm: Arm, cell: &Cell) -> bool {
    cell.scenario == "L2" && cell.graph == "G100k" && arm.clients().contains(&cell.clients)
}

/// Host K as seen from host L.
#[derive(Clone, Debug, PartialEq)]
struct Remote {
    /// SSH destination of host K (`user@host` or a `~/.ssh/config` alias).
    ssh: String,
    /// Host K's link address: the server binds it and every client uses it.
    addr: IpAddr,
    port: u16,
    /// Path on host K of this example's binary, for arm U's busy loops.
    burn_bin: Option<String>,
}

impl Remote {
    fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.addr, self.port)
    }
}

/// Arm U's CPU load for one cell.
#[derive(Clone, Debug, PartialEq)]
struct BurnPlan {
    target_cores: f64,
    processes: usize,
    duty: f64,
    /// The target exceeded host K's CPU count and was capped to it.
    capped: bool,
}

/// The R2 variant's settings.
#[derive(Debug, PartialEq)]
struct R2 {
    arm: Arm,
    remote: Option<Remote>,
    calibration_path: Option<PathBuf>,
    postgres_patterns: Vec<String>,
    /// Arm U's load per client count, set by [`prepare_r2`].
    burn: BTreeMap<usize, BurnPlan>,
    /// The calibration record, set by [`prepare_r2`] for arm U.
    calibration: Option<Value>,
}

impl R2 {
    fn layout(&self) -> &'static str {
        if self.remote.is_some() {
            "separated"
        } else {
            "single-host"
        }
    }
}

const R2_VARS: [&str; 6] = [
    "LOAD_REMOTE_SSH",
    "LOAD_REMOTE_ADDR",
    "LOAD_REMOTE_PORT",
    "LOAD_REMOTE_BURN_BIN",
    "LOAD_CALIBRATION",
    "LOAD_PG_PROCESS_PATTERNS",
];

/// Parses the variant's settings. `Ok(None)` when none is set, so the
/// approved single-host tool runs unchanged; any `LOAD_REMOTE_*`,
/// `LOAD_CALIBRATION` or `LOAD_PG_PROCESS_PATTERNS` without `LOAD_ARM` is an
/// error rather than silently ignored.
fn parse_r2(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<R2>, String> {
    let Some(arm) = get("LOAD_ARM") else {
        if let Some(var) = R2_VARS.iter().find(|v| get(v).is_some()) {
            return Err(format!("{var} is set but LOAD_ARM is not"));
        }
        return Ok(None);
    };
    let arm = match arm.as_str() {
        "S" => Arm::S,
        "T" => Arm::T,
        "U" => Arm::U,
        other => return Err(format!("LOAD_ARM must be S, T or U, not {other:?}")),
    };
    let postgres_patterns = match get("LOAD_PG_PROCESS_PATTERNS") {
        Some(v) => v
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        None => DEFAULT_POSTGRES_PATTERNS.map(str::to_string).to_vec(),
    };
    let calibration_path = get("LOAD_CALIBRATION").map(PathBuf::from);
    let remote = if arm == Arm::S {
        if let Some(var) = R2_VARS[..5].iter().find(|v| get(v).is_some()) {
            return Err(format!("{var} must not be set for arm S (single host)"));
        }
        None
    } else {
        let ssh = get("LOAD_REMOTE_SSH").ok_or("LOAD_REMOTE_SSH is required for arms T and U")?;
        if ssh.starts_with('-') || ssh.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!(
                "LOAD_REMOTE_SSH is not an SSH destination: {ssh:?}"
            ));
        }
        let addr =
            get("LOAD_REMOTE_ADDR").ok_or("LOAD_REMOTE_ADDR is required for arms T and U")?;
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("LOAD_REMOTE_ADDR must be an IP address, not {addr:?}"))?;
        if addr.is_unspecified() {
            return Err("LOAD_REMOTE_ADDR must be host K's link address, not 0.0.0.0 or ::".into());
        }
        let port = match get("LOAD_REMOTE_PORT") {
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or(format!("LOAD_REMOTE_PORT must be 1..=65535, not {p:?}"))?,
            None => DEFAULT_REMOTE_PORT,
        };
        let burn_bin = get("LOAD_REMOTE_BURN_BIN");
        match arm {
            Arm::U if burn_bin.is_none() => {
                return Err("LOAD_REMOTE_BURN_BIN is required for arm U".into());
            }
            Arm::U if calibration_path.is_none() => {
                return Err("LOAD_CALIBRATION (arm S results.jsonl) is required for arm U".into());
            }
            Arm::T if burn_bin.is_some() || calibration_path.is_some() => {
                return Err("LOAD_REMOTE_BURN_BIN and LOAD_CALIBRATION apply to arm U only".into());
            }
            _ => {}
        }
        Some(Remote {
            ssh,
            addr,
            port,
            burn_bin,
        })
    };
    Ok(Some(R2 {
        arm,
        remote,
        calibration_path,
        postgres_patterns,
        burn: BTreeMap::new(),
        calibration: None,
    }))
}

/// Parses the busy-loop mode: `LOAD_BURN_DUTY` in (0, 1] and
/// `LOAD_BURN_MAX_SECS` (default 900).
fn parse_burn(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<(f64, Duration)>, String> {
    let Some(duty) = get("LOAD_BURN_DUTY") else {
        return Ok(None);
    };
    let duty: f64 = duty
        .parse()
        .ok()
        .filter(|d: &f64| *d > 0.0 && *d <= 1.0)
        .ok_or(format!("LOAD_BURN_DUTY must be in (0, 1], not {duty:?}"))?;
    let max = match get("LOAD_BURN_MAX_SECS") {
        Some(v) => v.parse::<u64>().ok().filter(|s| *s > 0).ok_or(format!(
            "LOAD_BURN_MAX_SECS must be a positive integer, not {v:?}"
        ))?,
        None => DEFAULT_BURN_MAX_SECS,
    };
    Ok(Some((duty, Duration::from_secs(max))))
}

/// Busy loop at `duty` of every 10 ms period until `max` has elapsed.
///
/// Each period spins for `duty × 10 ms`, then sleeps to the period's end, so
/// one process uses about `duty` of one CPU. Schedule overruns are not
/// caught up (the next period restarts from now), so the average is at most
/// `duty`. The process is killed by arm U at the end of each observation;
/// `max` only bounds an orphaned process.
fn burn(duty: f64, max: Duration) {
    let start = Instant::now();
    let busy = BURN_PERIOD.mul_f64(duty);
    let mut period_start = start;
    while start.elapsed() < max {
        let busy_until = period_start + busy;
        while Instant::now() < busy_until {
            std::hint::spin_loop();
        }
        period_start += BURN_PERIOD;
        let now = Instant::now();
        if period_start > now {
            std::thread::sleep(period_start - now);
        } else {
            period_start = now;
        }
    }
}

/// Quotes `s` as one POSIX shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Arguments of `ssh` that run `command` on `dest`, non-interactively. The
/// remote command runs under `/bin/sh` whatever the login shell is.
fn ssh_args(dest: &str, command: &str) -> Vec<String> {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=3",
        "-T",
        dest,
    ]
    .into_iter()
    .map(str::to_string)
    .chain([format!("exec /bin/sh -c {}", sh_quote(command))])
    .collect()
}

fn ssh_command(dest: &str, command: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(ssh_args(dest, command));
    c
}

/// Remote script for one server: prints its PID, then runs until the SSH
/// session's standard input closes (normal stop, or this tool exiting or
/// being killed), then stops the server with `SIGTERM`. Its output goes to
/// the session's standard error.
fn server_script(addr: SocketAddr, bin: &str) -> String {
    let env: Vec<String> = [
        ("KNOTREL_ADDR", addr.to_string()),
        ("KNOTREL_ALLOW_NON_LOOPBACK", "true".to_string()),
    ]
    .into_iter()
    .chain(SERVER_ENV.map(|(k, v)| (k, v.to_string())))
    .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
    .collect();
    format!(
        "env -i {} {} </dev/null >&2 & pid=$!; echo \"$pid\"; read line; \
         kill -TERM \"$pid\" 2>/dev/null; wait \"$pid\"",
        env.join(" "),
        sh_quote(bin)
    )
}

/// Remote script for arm U's load: `processes` busy loops, PIDs printed on
/// one line, all stopped when the SSH session's standard input closes.
fn burn_script(bin: &str, plan: &BurnPlan) -> String {
    format!(
        "pids=; i=0; while [ \"$i\" -lt {n} ]; do \
         env -i LOAD_BURN_DUTY={duty:.4} LOAD_BURN_MAX_SECS={max} {bin} </dev/null >/dev/null 2>&1 & \
         pids=\"$pids $!\"; i=$((i+1)); done; echo $pids; read line; kill -TERM $pids 2>/dev/null; wait",
        n = plan.processes,
        duty = plan.duty,
        max = DEFAULT_BURN_MAX_SECS,
        bin = sh_quote(bin),
    )
}

fn remote_top_command(remote: &Remote) -> Command {
    let mut c = ssh_command(&remote.ssh, &format!("exec top {}", TOP_ARGS.join(" ")));
    c.stdin(Stdio::null());
    c
}

/// A process tree on host K tied to one SSH session.
struct RemoteProcess {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    first_line: String,
}

impl RemoteProcess {
    async fn start(dest: &str, script: &str, stderr: Stdio) -> Self {
        use tokio::io::AsyncBufReadExt;
        let mut child = ssh_command(dest, script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn ssh");
        let stdin = child.stdin.take();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let first_line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .unwrap_or_else(|| panic!("no answer from host K over SSH ({dest})"));
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Self {
            child,
            stdin,
            first_line,
        }
    }

    /// A fresh server on host K. Fails if something already answers on the
    /// address: it could be a server left over from an earlier observation.
    async fn start_server(remote: &Remote, bin: &Path, stderr_log: &Path) -> Self {
        let addr = remote.socket_addr();
        let in_use = tokio::task::spawn_blocking(move || {
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
        })
        .await
        .unwrap();
        assert!(!in_use, "{addr} already accepts connections on host K");
        let log = std::fs::File::create(stderr_log).unwrap();
        let script = server_script(addr, &bin.to_string_lossy());
        Self::start(&remote.ssh, &script, Stdio::from(log)).await
    }

    /// PIDs on host K printed by the remote script.
    fn pids(&self) -> Vec<u32> {
        parse_pids(&self.first_line)
    }

    async fn stop(&mut self) {
        drop(self.stdin.take());
        let _ = tokio::time::timeout(Duration::from_secs(15), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

fn parse_pids(line: &str) -> Vec<u32> {
    line.split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

async fn ssh_output(dest: &str, command: &str) -> Option<String> {
    let out = ssh_command(dest, command)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// CPU time per process: PID → (CPU seconds, executable).
type PsMap = BTreeMap<u32, (f64, String)>;

const PS_COMMAND: &str = "ps -A -o pid=,time=,comm=";

/// Parses `ps -o time` (`[[dd-]hh:]mm:ss[.cc]`) into seconds.
fn parse_cpu_time(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut secs = 0.0;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + secs)
}

/// Parses the output of [`PS_COMMAND`].
fn parse_ps(text: &str) -> PsMap {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let secs = parse_cpu_time(fields.next()?)?;
            let comm = fields.collect::<Vec<_>>().join(" ");
            Some((pid, (secs, comm)))
        })
        .collect()
}

/// Simultaneous `ps` snapshots of this host and, in remote mode, host K.
struct Snapshots {
    at: Instant,
    local: PsMap,
    remote: Option<PsMap>,
}

async fn ps_snapshots(r2: &R2) -> Snapshots {
    let local = async {
        let out = Command::new("sh")
            .args(["-c", PS_COMMAND])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await;
        out.map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default()
    };
    let remote = async {
        match &r2.remote {
            Some(r) => ssh_output(&r.ssh, PS_COMMAND).await.map(|t| parse_ps(&t)),
            None => None,
        }
    };
    let (local, remote) = tokio::join!(local, remote);
    Snapshots {
        at: Instant::now(),
        local,
        remote,
    }
}

/// Which process plays which role in a CPU breakdown.
struct Roles {
    knotrel: Option<u32>,
    feeder: Option<u32>,
    load_tool: Option<u32>,
    burners: Vec<u32>,
    postgres_patterns: Vec<String>,
}

/// CPU seconds per role between two snapshots.
struct ProcessCpu {
    window_secs: f64,
    groups: BTreeMap<&'static str, f64>,
    /// The largest processes of the group `other`.
    top_other: Vec<(String, f64)>,
}

impl ProcessCpu {
    /// Mean CPUs used by `group` over the window.
    fn cores(&self, group: &str) -> f64 {
        if self.window_secs > 0.0 {
            self.groups.get(group).copied().unwrap_or(0.0) / self.window_secs
        } else {
            0.0
        }
    }

    fn to_json(&self) -> Value {
        let groups: serde_json::Map<String, Value> = self
            .groups
            .iter()
            .map(|(g, s)| {
                (
                    (*g).to_string(),
                    json!({"cpu_secs": s, "cores": self.cores(g)}),
                )
            })
            .collect();
        json!({"groups": groups, "top_other": self.top_other.iter()
            .map(|(c, s)| json!({"command": c, "cpu_secs": s})).collect::<Vec<_>>()})
    }
}

/// Attributes the CPU time used between `before` and `after` to roles.
///
/// A process absent from `before`, or whose PID now names another
/// executable, started in the window and counts from zero. A process that
/// exited in the window is not counted (none of the measured roles exits
/// during a catch-up). PostgreSQL is matched by executable name, because in
/// a container it is the virtual machine's process. Cost: O(p log p) for p
/// processes.
fn process_cpu(before: &PsMap, after: &PsMap, roles: &Roles, window_secs: f64) -> ProcessCpu {
    let mut groups: BTreeMap<&'static str, f64> = BTreeMap::new();
    for g in [
        "knotrel",
        "feeder",
        "load_tool",
        "postgres",
        "burners",
        "other",
    ] {
        groups.insert(g, 0.0);
    }
    let mut other = Vec::new();
    for (pid, (secs, comm)) in after {
        let start = match before.get(pid) {
            Some((b, c)) if c == comm => *b,
            _ => 0.0,
        };
        let used = (secs - start).max(0.0);
        let pid = Some(*pid);
        let name = comm.rsplit('/').next().unwrap_or(comm);
        let group = if pid == roles.knotrel {
            "knotrel"
        } else if pid == roles.feeder {
            "feeder"
        } else if pid == roles.load_tool {
            "load_tool"
        } else if roles.burners.iter().any(|b| Some(*b) == pid) {
            "burners"
        } else if roles
            .postgres_patterns
            .iter()
            .any(|p| name.starts_with(p.as_str()))
        {
            "postgres"
        } else {
            if used > 0.0 {
                other.push((comm.clone(), used));
            }
            "other"
        };
        *groups.get_mut(group).unwrap() += used;
    }
    other.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    other.truncate(5);
    ProcessCpu {
        window_secs,
        groups,
        top_other: other,
    }
}

/// Warm-up and catch-up statistics of a CPU sample series, with the same
/// windows as the local host's.
fn cpu_windows(all: &[(Instant, f64)], start: Instant, end: Instant) -> (Value, Value) {
    let warmup: Vec<f64> = all
        .iter()
        .filter(|(t, _)| *t <= start)
        .map(|(_, v)| *v)
        .collect();
    let catch_up: Vec<f64> = all
        .iter()
        .filter(|(t, _)| *t > start && *t <= end)
        .map(|(_, v)| *v)
        .collect();
    let max = catch_up
        .iter()
        .cloned()
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    (
        json!({"samples": warmup.len(), "median": median(&warmup)}),
        json!({"samples": catch_up.len(), "median": median(&catch_up), "max": max}),
    )
}

/// Arm U's load from arm S's results: per client count, the median over arm
/// S's observations of host K's non-Knotrel CPU (PostgreSQL, feeder, load
/// tool) during catch-up, in CPUs.
fn calibration_from_results(
    text: &str,
    clients: &[usize],
) -> Result<BTreeMap<usize, Value>, String> {
    let mut by_c: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for (i, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let r: Value = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        if r["arm"] != "S" {
            return Err(format!("line {}: not an arm S observation", i + 1));
        }
        let c = r["clients"]
            .as_u64()
            .ok_or(format!("line {}: no clients", i + 1))? as usize;
        let cores = r["metrics"]["host_k_non_knotrel_cores"]
            .as_f64()
            .ok_or(format!("line {}: no host_k_non_knotrel_cores", i + 1))?;
        by_c.entry(c).or_default().push(cores);
    }
    let mut out = BTreeMap::new();
    for c in clients {
        let cores = by_c
            .get(c)
            .filter(|v| !v.is_empty())
            .ok_or(format!("no arm S observation with C={c}"))?;
        out.insert(
            *c,
            json!({"observations": cores.len(), "cores": cores, "median_cores": median(cores)}),
        );
    }
    Ok(out)
}

/// Busy-loop processes for `cores` CPUs on a host with `cpus` CPUs:
/// ⌈cores⌉ processes at duty `cores / ⌈cores⌉`, capped at `cpus` processes
/// at full duty.
fn burn_plan(cores: f64, cpus: usize) -> BurnPlan {
    if cores.is_nan() || cores <= 0.0 || cpus == 0 {
        return BurnPlan {
            target_cores: cores,
            processes: 0,
            duty: 0.0,
            capped: false,
        };
    }
    let capped = cores > cpus as f64;
    let processes = (cores.ceil() as usize).clamp(1, cpus);
    let duty = ((cores / processes as f64).min(1.0) * 10_000.0).round() / 10_000.0;
    BurnPlan {
        target_cores: cores,
        processes,
        duty,
        capped,
    }
}

/// Arm U: reads the calibration and host K's CPU count, and records both.
async fn prepare_r2(r2: &mut R2, out: &Path) {
    let (Some(path), Some(remote)) = (&r2.calibration_path, &r2.remote) else {
        return;
    };
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read LOAD_CALIBRATION {}: {e}", path.display()));
    let per_c = calibration_from_results(&text, r2.arm.clients())
        .unwrap_or_else(|e| panic!("LOAD_CALIBRATION {}: {e}", path.display()));
    let cpus: usize = ssh_output(&remote.ssh, "sysctl -n hw.ncpu")
        .await
        .and_then(|s| s.trim().parse().ok())
        .expect("cannot read host K's CPU count over SSH");
    let mut plans = serde_json::Map::new();
    for (c, cal) in &per_c {
        let plan = burn_plan(cal["median_cores"].as_f64().unwrap_or(0.0), cpus);
        plans.insert(
            c.to_string(),
            json!({"from_arm_s": cal, "processes": plan.processes, "duty": plan.duty,
                   "target_cores": plan.target_cores, "capped": plan.capped}),
        );
        r2.burn.insert(*c, plan);
    }
    let record = json!({
        "source": path.display().to_string(),
        "host_k_cpus": cpus,
        "burn_period_ms": BURN_PERIOD.as_millis() as u64,
        "per_clients": plans,
        "limit": "reproduces CPU demand only, not memory, cache or I/O behavior",
    });
    std::fs::write(
        out.join("calibration.json"),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    r2.calibration = Some(record);
}

async fn start_burners(r2: &R2, cell: Cell, out: &Path) -> Option<RemoteProcess> {
    let plan = r2.burn.get(&cell.clients)?;
    let remote = r2.remote.as_ref()?;
    if plan.processes == 0 {
        return None;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out.join("burn.stderr.log"))
        .unwrap();
    let script = burn_script(remote.burn_bin.as_deref()?, plan);
    Some(RemoteProcess::start(&remote.ssh, &script, Stdio::from(log)).await)
}

/// 1,000 TCP connection set-ups (one round trip each: `connect` returns when
/// the SYN-ACK arrives, before the server application sees the connection)
/// to `addr`, in milliseconds, and the number of failures.
fn tcp_round_trips(addr: SocketAddr, n: usize) -> (Vec<f64>, usize) {
    let mut rtts = Vec::with_capacity(n);
    let mut failures = 0;
    for _ in 0..n {
        let t = Instant::now();
        match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            Ok(stream) => {
                rtts.push(t.elapsed().as_secs_f64() * 1000.0);
                drop(stream);
            }
            Err(_) => failures += 1,
        }
    }
    (rtts, failures)
}

fn link_stats(mut rtts: Vec<f64>, failures: usize) -> Value {
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = median(&rtts);
    json!({
        "round_trips": rtts.len(),
        "failures": failures,
        "median_ms": med,
        "p99_ms": percentile(&rtts, 99.0),
        "max_ms": rtts.last(),
        "valid": med.is_some_and(|m| m <= LINK_MAX_MEDIAN_MS),
    })
}

/// Link check of one arm against a fresh server at the measured address.
async fn link_check(s: &Settings, r2: &R2, label: &str) -> Value {
    let (mut server, addr) = match &r2.remote {
        None => {
            let port = free_port();
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            (
                Server::Local(start_local_server(&s.knotrel_bin, port)),
                addr,
            )
        }
        Some(remote) => {
            let log = s.out.join(format!("server-link-{label}.stderr.log"));
            let server = RemoteProcess::start_server(remote, &s.knotrel_bin, &log).await;
            (Server::Remote(server), remote.socket_addr())
        }
    };
    wait_http(
        &format!("{}/health", base_url(addr)),
        Duration::from_secs(10),
    )
    .await;
    let (rtts, failures) =
        tokio::task::spawn_blocking(move || tcp_round_trips(addr, LINK_ROUND_TRIPS))
            .await
            .unwrap();
    server.stop().await;
    let mut stats = link_stats(rtts, failures);
    stats["addr"] = json!(addr.to_string());
    stats["at_unix_secs"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    stats
}

/// A results record with its metrics also at the top level.
fn flatten(record: &Value) -> Value {
    let mut flat = record.clone();
    if let (Some(obj), Some(m)) = (flat.as_object_mut(), record["metrics"].as_object()) {
        for (k, v) in m {
            obj.insert(k.clone(), v.clone());
        }
    }
    flat
}

fn stat_of(vals: &[f64]) -> Value {
    let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    json!({"median": median(vals), "min": if vals.is_empty() { None } else { Some(min) },
           "max": if vals.is_empty() { None } else { Some(max) }, "n": vals.len()})
}

/// Descriptive per-cell summary of one arm (no Q verdict: arm S re-anchors
/// the variant and is not a new Q3 evaluation).
fn r2_summary(results: &[Value]) -> Value {
    let mut cells: BTreeMap<usize, Vec<&Value>> = BTreeMap::new();
    for r in results {
        cells
            .entry(r["clients"].as_u64().unwrap_or(0) as usize)
            .or_default()
            .push(r);
    }
    let fields: [(&str, &[&str]); 11] = [
        ("events_per_sec", &["events_per_sec"]),
        ("secs_to_ready", &["secs_to_ready"]),
        (
            "local_cpu_busy_median",
            &["host_cpu_busy_percent", "median"],
        ),
        (
            "remote_cpu_busy_median",
            &["remote_host_cpu_busy_percent", "median"],
        ),
        ("host_k_non_knotrel_cores", &["host_k_non_knotrel_cores"]),
        (
            "feeder_admission_queue_ms",
            &["phases", "feeder_events", "admission_queue_ms", "mean_ms"],
        ),
        (
            "feeder_lock_wait_ms",
            &["phases", "feeder_events", "lock_wait_ms", "mean_ms"],
        ),
        (
            "feeder_execution_ms",
            &["phases", "feeder_events", "execution_ms", "mean_ms"],
        ),
        ("feeder_503_refusals", &["feeder_503_refusals"]),
        ("query_p99_ms", &["queries", "p99_ms"]),
        ("events", &["events"]),
    ];
    let rate0 = cells.get(&0).and_then(|v| {
        median(
            &v.iter()
                .filter_map(|r| get(r, &["events_per_sec"]))
                .collect::<Vec<_>>(),
        )
    });
    let mut out = serde_json::Map::new();
    for (c, rs) in &cells {
        let mut m = serde_json::Map::new();
        for (name, path) in fields {
            let vals: Vec<f64> = rs.iter().filter_map(|r| get(r, path)).collect();
            m.insert(name.into(), stat_of(&vals));
        }
        let rate = m["events_per_sec"]["median"].as_f64();
        m.insert(
            "ratio_to_c0".into(),
            json!(rate.zip(rate0).map(|(r, r0)| r / r0)),
        );
        out.insert(format!("L2 G100k C={c}"), Value::Object(m));
    }
    Value::Object(out)
}

/// Outcome of the variant's decision rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Supported,
    NotSupported,
    InsufficientEvidence,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Supported => "hypothesis supported",
            Self::NotSupported => "hypothesis not supported",
            Self::InsufficientEvidence => "insufficient evidence",
        }
    }
}

/// Largest accepted spread (max / min) of one arm's three catch-up rates for
/// one client count; above it the client count's result is insufficient
/// evidence.
const MAX_SPREAD: f64 = 1.25;

/// One client count's inputs to the decision rule: each arm's per-observation
/// absolute catch-up rates (backlog events / seconds to `ready`) and host K's
/// CPU busy percentage during catch-up in arm T (per observation, the median
/// of its per-second samples).
#[derive(Clone, Debug, Default)]
struct RuleInput {
    s: Vec<f64>,
    t: Vec<f64>,
    u: Vec<f64>,
    k_cpu_t: Vec<f64>,
}

/// Spread of a set of rates: max / min, `None` if empty or not positive.
fn spread(v: &[f64]) -> Option<f64> {
    let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (!v.is_empty() && min > 0.0).then(|| max / min)
}

/// The decision rule for one client count (section "R2 two-Mac variant",
/// "Decision rule"):
///
/// - a_X = median of arm X's three rates; G = a_T − a_S;
///   r_U = (a_T − a_U) / G, defined only when G > 0.
/// - Any arm with a spread above [`MAX_SPREAD`] (or missing data): insufficient.
/// - Supported: a_T ≥ 2·a_S, r_U ≥ 0.5 and host K's median CPU in T ≤ 80 %.
/// - Not supported: a_T < 1.2·a_S, or r_U ≤ 0.25.
/// - Otherwise insufficient.
///
/// Supported and not supported exclude each other: supported needs
/// a_T ≥ 2·a_S (so not a_T < 1.2·a_S) and r_U ≥ 0.5 (so not r_U ≤ 0.25).
/// When G ≤ 0, a_T ≤ a_S < 1.2·a_S, so the result is not supported without
/// r_U.
fn cell_outcome(r: &RuleInput) -> (Outcome, Value) {
    let (a_s, a_t, a_u, k) = (median(&r.s), median(&r.t), median(&r.u), median(&r.k_cpu_t));
    let spreads = json!({"S": spread(&r.s), "T": spread(&r.t), "U": spread(&r.u)});
    let mut detail = json!({
        "observations": {"S": r.s, "T": r.t, "U": r.u},
        "a_S": a_s, "a_T": a_t, "a_U": a_u, "spread": spreads,
        "host_k_cpu_busy_median_T": k,
    });
    let (Some(a_s), Some(a_t), Some(a_u), Some(k)) = (a_s, a_t, a_u, k) else {
        detail["reason"] = json!("missing data");
        return (Outcome::InsufficientEvidence, detail);
    };
    let g = a_t - a_s;
    let r_u = (g > 0.0).then(|| (a_t - a_u) / g);
    detail["gain_G"] = json!(g);
    detail["recovered_share_r_U"] = json!(r_u);
    detail["t_over_s"] = json!(if a_s > 0.0 { Some(a_t / a_s) } else { None });
    let spread_ok = [&r.s, &r.t, &r.u]
        .iter()
        .all(|v| spread(v).is_some_and(|x| x <= MAX_SPREAD));
    let outcome = if !spread_ok {
        detail["reason"] = json!(format!("an arm's spread (max/min) exceeds {MAX_SPREAD}"));
        Outcome::InsufficientEvidence
    } else if a_t >= 2.0 * a_s && r_u.is_some_and(|x| x >= 0.5) && k <= 80.0 {
        Outcome::Supported
    } else if a_t < 1.2 * a_s || r_u.is_some_and(|x| x <= 0.25) {
        Outcome::NotSupported
    } else {
        Outcome::InsufficientEvidence
    };
    detail["outcome"] = json!(outcome.name());
    (outcome, detail)
}

/// Overall outcome: supported or not supported only if `C` = 16 and 64 agree;
/// any disagreement, or an insufficient client count, is insufficient
/// evidence.
fn r2_outcome(cells: &[Outcome; 2]) -> Outcome {
    match cells {
        [a, b] if a == b => *a,
        _ => Outcome::InsufficientEvidence,
    }
}

/// Reads `<dir>/<arm>/results.jsonl` and `link-check.json` for S, T and U,
/// checks validity and applies [`r2_outcome`].
fn decide_dir(dir: &Path) -> Value {
    let mut arms = BTreeMap::new();
    for arm in ["S", "T", "U"] {
        let d = dir.join(arm);
        let results: Vec<Value> = std::fs::read_to_string(d.join("results.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|r| flatten(&r))
            .collect();
        let link = std::fs::read_to_string(d.join("link-check.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let invalid = std::fs::read_to_string(d.join("invalid.txt"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        arms.insert(arm, (results, link, invalid));
    }
    decide(&arms)
}

/// The decision from loaded arms: (flattened results, link check, recorded
/// invalidation cause).
fn decide(arms: &BTreeMap<&str, (Vec<Value>, Value, Option<String>)>) -> Value {
    let mut reasons = Vec::new();
    let empty = (Vec::new(), Value::Null, None);
    let values = |arm: &str, c: usize, path: &[&str]| -> Vec<f64> {
        let (results, _, _) = arms.get(arm).unwrap_or(&empty);
        results
            .iter()
            .filter(|r| r["clients"].as_u64() == Some(c as u64))
            .filter_map(|r| get(r, path))
            .collect()
    };
    for (arm, clients) in [("S", Arm::S), ("T", Arm::T), ("U", Arm::U)] {
        let (results, link, invalid) = arms.get(arm).unwrap_or(&empty);
        if let Some(cause) = invalid {
            reasons.push(format!("arm {arm} invalid: {cause}"));
        }
        for when in ["before", "after"] {
            if link[when]["valid"].as_bool() != Some(true) {
                reasons.push(format!(
                    "arm {arm}: link check {when} missing or median > {LINK_MAX_MEDIAN_MS} ms"
                ));
            }
        }
        for c in clients.clients() {
            let complete = results
                .iter()
                .filter(|r| r["clients"].as_u64() == Some(*c as u64))
                .filter(|r| r["arm"] == arm && r["secs_to_ready"].as_f64().is_some())
                .count();
            if complete != 3 {
                reasons.push(format!(
                    "arm {arm} C={c}: {complete} completed observations, 3 required"
                ));
            }
        }
    }
    let ev = &["events_per_sec"][..];
    let k = &["remote_host_cpu_busy_percent", "median"][..];
    let mut per_c = serde_json::Map::new();
    let mut outcomes = [Outcome::InsufficientEvidence; 2];
    for (i, c) in [16usize, 64].into_iter().enumerate() {
        let input = RuleInput {
            s: values("S", c, ev),
            t: values("T", c, ev),
            u: values("U", c, ev),
            k_cpu_t: values("T", c, k),
        };
        let (outcome, detail) = cell_outcome(&input);
        outcomes[i] = outcome;
        per_c.insert(format!("C={c}"), detail);
    }
    let rule_outcome = r2_outcome(&outcomes);
    let outcome = if reasons.is_empty() {
        rule_outcome
    } else {
        Outcome::InsufficientEvidence
    };
    json!({
        "outcome": outcome.name(),
        "rule_outcome_before_validity": rule_outcome.name(),
        "agreement": outcomes[0] == outcomes[1],
        "validity_failures": reasons,
        "per_clients": per_c,
        "rule": "per C in {16, 64}: a_X = median of arm X's 3 catch-up rates, G = a_T - a_S, r_U = (a_T - a_U) / G if G > 0; \
                 any arm spread max/min > 1.25 -> insufficient; supported: a_T >= 2 a_S and r_U >= 0.5 and host K CPU median in T <= 80 %; \
                 not supported: a_T < 1.2 a_S or r_U <= 0.25; otherwise insufficient. \
                 Overall: supported or not supported only if C=16 and C=64 agree and every arm is valid; otherwise insufficient evidence",
        "scope": "supported shows that CPU demand of the kind arm U generates suffices to explain most of the gap; \
                  it does not show that it is the same contention as in arm S (memory bandwidth, caches, scheduler, PostgreSQL I/O are not reproduced)",
        "note": "diagnostic only; Q3's classification is unchanged by any outcome",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn no_r2_variable_keeps_the_single_host_tool() {
        assert_eq!(parse_r2(&vars(&[])), Ok(None));
        assert_eq!(parse_burn(&vars(&[])), Ok(None));
    }

    #[test]
    fn r2_variables_without_arm_are_rejected() {
        for var in R2_VARS {
            let err = parse_r2(&vars(&[(var, "x")])).unwrap_err();
            assert!(err.contains(var), "{err}");
        }
    }

    #[test]
    fn arm_s_is_single_host() {
        let r2 = parse_r2(&vars(&[("LOAD_ARM", "S")])).unwrap().unwrap();
        assert_eq!(r2.arm, Arm::S);
        assert_eq!(r2.remote, None);
        assert_eq!(r2.layout(), "single-host");
        assert_eq!(r2.postgres_patterns.len(), DEFAULT_POSTGRES_PATTERNS.len());
        let err = parse_r2(&vars(&[("LOAD_ARM", "S"), ("LOAD_REMOTE_SSH", "k")])).unwrap_err();
        assert!(err.contains("arm S"), "{err}");
    }

    #[test]
    fn arm_t_needs_the_remote_host() {
        let ok = parse_r2(&vars(&[
            ("LOAD_ARM", "T"),
            ("LOAD_REMOTE_SSH", "rt@host-k"),
            ("LOAD_REMOTE_ADDR", "169.254.10.2"),
        ]))
        .unwrap()
        .unwrap();
        let remote = ok.remote.unwrap();
        assert_eq!(remote.port, DEFAULT_REMOTE_PORT);
        assert_eq!(remote.socket_addr().to_string(), "169.254.10.2:18080");
        assert_eq!(ok.calibration_path, None);
        for (pairs, needle) in [
            (vec![("LOAD_ARM", "T")], "LOAD_REMOTE_SSH"),
            (
                vec![("LOAD_ARM", "T"), ("LOAD_REMOTE_SSH", "k")],
                "LOAD_REMOTE_ADDR",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "-oProxyCommand=x"),
                    ("LOAD_REMOTE_ADDR", "10.0.0.2"),
                ],
                "SSH destination",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "a b"),
                    ("LOAD_REMOTE_ADDR", "10.0.0.2"),
                ],
                "SSH destination",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "k"),
                    ("LOAD_REMOTE_ADDR", "host-k.local"),
                ],
                "IP address",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "k"),
                    ("LOAD_REMOTE_ADDR", "0.0.0.0"),
                ],
                "link address",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "k"),
                    ("LOAD_REMOTE_ADDR", "10.0.0.2"),
                    ("LOAD_REMOTE_PORT", "0"),
                ],
                "LOAD_REMOTE_PORT",
            ),
            (
                vec![
                    ("LOAD_ARM", "T"),
                    ("LOAD_REMOTE_SSH", "k"),
                    ("LOAD_REMOTE_ADDR", "10.0.0.2"),
                    ("LOAD_CALIBRATION", "s.jsonl"),
                ],
                "arm U only",
            ),
            (vec![("LOAD_ARM", "X")], "S, T or U"),
        ] {
            let err = parse_r2(&vars(&pairs)).unwrap_err();
            assert!(err.contains(needle), "{pairs:?}: {err}");
        }
    }

    #[test]
    fn arm_u_needs_burn_binary_and_calibration() {
        let base = [
            ("LOAD_ARM", "U"),
            ("LOAD_REMOTE_SSH", "k"),
            ("LOAD_REMOTE_ADDR", "fe80::1"),
            ("LOAD_REMOTE_PORT", "9000"),
        ];
        let err = parse_r2(&vars(&base)).unwrap_err();
        assert!(err.contains("LOAD_REMOTE_BURN_BIN"), "{err}");
        let mut with_bin = base.to_vec();
        with_bin.push(("LOAD_REMOTE_BURN_BIN", "/k/load"));
        let err = parse_r2(&vars(&with_bin)).unwrap_err();
        assert!(err.contains("LOAD_CALIBRATION"), "{err}");
        with_bin.push(("LOAD_CALIBRATION", "/l/S/results.jsonl"));
        with_bin.push(("LOAD_PG_PROCESS_PATTERNS", " vm , pg ,,"));
        let r2 = parse_r2(&vars(&with_bin)).unwrap().unwrap();
        assert_eq!(
            r2.remote.as_ref().unwrap().socket_addr().to_string(),
            "[fe80::1]:9000"
        );
        assert_eq!(r2.postgres_patterns, vec!["vm", "pg"]);
        assert_eq!(r2.layout(), "separated");
    }

    #[test]
    fn arm_cells_are_l2_g100k_only() {
        let keys = |arm| {
            CELLS
                .iter()
                .filter(|c| arm_includes(arm, c))
                .map(|c| format!("{} {} C={}", c.scenario, c.graph, c.clients))
                .collect::<Vec<_>>()
        };
        let all = ["L2 G100k C=0", "L2 G100k C=16", "L2 G100k C=64"];
        assert_eq!(keys(Arm::S), all);
        assert_eq!(keys(Arm::T), all);
        assert_eq!(keys(Arm::U), ["L2 G100k C=16", "L2 G100k C=64"]);
    }

    #[test]
    fn burn_settings_are_validated() {
        assert_eq!(
            parse_burn(&vars(&[("LOAD_BURN_DUTY", "0.75")])),
            Ok(Some((0.75, Duration::from_secs(DEFAULT_BURN_MAX_SECS))))
        );
        assert_eq!(
            parse_burn(&vars(&[
                ("LOAD_BURN_DUTY", "1"),
                ("LOAD_BURN_MAX_SECS", "5")
            ])),
            Ok(Some((1.0, Duration::from_secs(5))))
        );
        for bad in ["0", "1.5", "-0.1", "x", "NaN"] {
            assert!(
                parse_burn(&vars(&[("LOAD_BURN_DUTY", bad)])).is_err(),
                "{bad}"
            );
        }
        assert!(
            parse_burn(&vars(&[
                ("LOAD_BURN_DUTY", "0.5"),
                ("LOAD_BURN_MAX_SECS", "0")
            ]))
            .is_err()
        );
    }

    #[test]
    fn burn_runs_for_its_bound() {
        let t = Instant::now();
        burn(0.5, Duration::from_millis(50));
        let e = t.elapsed();
        assert!(
            e >= Duration::from_millis(50) && e < Duration::from_secs(2),
            "{e:?}"
        );
    }

    #[test]
    fn shell_quoting_survives_single_quotes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn ssh_arguments_are_non_interactive_and_use_sh() {
        let args = ssh_args("rt@host-k", "echo 'x'");
        assert_eq!(args[..2], ["-o", "BatchMode=yes"]);
        assert!(args.contains(&"-T".to_string()));
        let n = args.len();
        assert_eq!(args[n - 2], "rt@host-k");
        assert_eq!(args[n - 1], r"exec /bin/sh -c 'echo '\''x'\'''");
    }

    #[test]
    fn server_script_has_protocol_settings_and_bind_address() {
        let s = server_script(
            "169.254.10.2:18080".parse().unwrap(),
            "/opt/k/knotrel-server",
        );
        assert!(s.starts_with(
            "env -i KNOTREL_ADDR='169.254.10.2:18080' KNOTREL_ALLOW_NON_LOOPBACK='true' "
        ));
        for (k, v) in SERVER_ENV {
            assert!(s.contains(&format!("{k}='{v}'")), "{k}");
        }
        assert!(s.contains(
            "'/opt/k/knotrel-server' </dev/null >&2 & pid=$!; echo \"$pid\"; read line;"
        ));
        assert!(s.ends_with("kill -TERM \"$pid\" 2>/dev/null; wait \"$pid\""));
        let v6 = server_script("[fe80::1]:9000".parse().unwrap(), "/b");
        assert!(v6.contains("KNOTREL_ADDR='[fe80::1]:9000'"));
    }

    /// Runs the generated scripts with the local `/bin/sh` (no SSH): the
    /// server script must print the PID, keep running, and stop its child
    /// when its standard input closes.
    #[tokio::test]
    async fn remote_scripts_run_under_sh_and_stop_on_stdin_close() {
        use tokio::io::AsyncBufReadExt;
        let script = server_script("127.0.0.1:1".parse().unwrap(), "/bin/sleep")
            .replace("'/bin/sleep'", "'/bin/sleep' 30");
        let mut child = Command::new("/bin/sh")
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let pid = parse_pids(&lines.next_line().await.unwrap().unwrap());
        assert_eq!(pid.len(), 1);
        drop(stdin);
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        assert!(status.is_ok(), "script did not stop on stdin close");

        let plan = burn_plan(1.5, 8);
        let script = burn_script("/bin/sleep", &plan).replace("'/bin/sleep'", "'/bin/sleep' 30");
        let mut child = Command::new("/bin/sh")
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let pids = parse_pids(&lines.next_line().await.unwrap().unwrap());
        assert_eq!(pids.len(), 2);
        drop(stdin);
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        assert!(status.is_ok(), "burn script did not stop on stdin close");
    }

    #[test]
    fn burn_script_starts_the_planned_processes() {
        let s = burn_script("/k/load", &burn_plan(2.5, 8));
        assert!(s.contains("-lt 3 ]"), "{s}");
        assert!(s.contains("LOAD_BURN_DUTY=0.8333 "), "{s}");
        assert!(s.contains("'/k/load' </dev/null"), "{s}");
    }

    #[test]
    fn cpu_times_parse() {
        assert_eq!(parse_cpu_time("0:01.50"), Some(1.5));
        assert_eq!(parse_cpu_time("12:34.56"), Some(754.56));
        assert_eq!(parse_cpu_time("125:00.00"), Some(7500.0));
        assert_eq!(parse_cpu_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_cpu_time("2-00:00:01"), Some(172_801.0));
        assert_eq!(parse_cpu_time("x"), None);
    }

    #[test]
    fn ps_output_parses_paths_with_spaces() {
        let m = parse_ps(
            "  1   0:10.00 /sbin/launchd\n 42 1:00.25 /Applications/Docker.app/Contents/My App\nbad\n",
        );
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[&42],
            (
                60.25,
                "/Applications/Docker.app/Contents/My App".to_string()
            )
        );
    }

    #[test]
    fn process_cpu_attributes_roles() {
        let before = parse_ps(
            "10 0:01.00 /k/knotrel-server\n20 0:05.00 /x/com.apple.Virtualization.VirtualMachine\n\
             30 0:02.00 /l/load\n40 0:00.50 /usr/bin/other\n50 0:09.00 /old/reused\n",
        );
        let after = parse_ps(
            "10 0:03.00 /k/knotrel-server\n20 0:08.00 /x/com.apple.Virtualization.VirtualMachine\n\
             30 0:04.00 /l/load\n40 0:00.75 /usr/bin/other\n50 0:01.00 /new/feeder\n60 0:01.00 /k/load\n",
        );
        let roles = Roles {
            knotrel: Some(10),
            feeder: Some(50),
            load_tool: Some(30),
            burners: vec![60],
            postgres_patterns: DEFAULT_POSTGRES_PATTERNS.map(str::to_string).to_vec(),
        };
        let p = process_cpu(&before, &after, &roles, 2.0);
        assert_eq!(p.groups["knotrel"], 2.0);
        assert_eq!(p.groups["postgres"], 3.0);
        assert_eq!(p.groups["load_tool"], 2.0);
        // PID 50 was reused by a new executable: counted from zero.
        assert_eq!(p.groups["feeder"], 1.0);
        assert_eq!(p.groups["burners"], 1.0);
        assert_eq!(p.groups["other"], 0.25);
        assert_eq!(p.cores("postgres"), 1.5);
        assert_eq!(p.top_other, vec![("/usr/bin/other".to_string(), 0.25)]);
        assert_eq!(p.to_json()["groups"]["knotrel"]["cores"], 1.0);
    }

    #[test]
    fn cpu_windows_split_at_start() {
        let t0 = Instant::now();
        let s = |ms: u64, v: f64| (t0 + Duration::from_millis(ms), v);
        let all = [
            s(0, 10.0),
            s(1000, 20.0),
            s(2000, 90.0),
            s(3000, 70.0),
            s(9000, 1.0),
        ];
        let (w, c) = cpu_windows(
            &all,
            t0 + Duration::from_millis(1500),
            t0 + Duration::from_millis(3500),
        );
        assert_eq!(w["median"], 15.0);
        assert_eq!(c["median"], 80.0);
        assert_eq!(c["max"], 90.0);
        assert_eq!(c["samples"], 2);
    }

    #[test]
    fn calibration_takes_the_median_per_client_count() {
        let line = |c: u64, cores: f64| {
            json!({"arm": "S", "clients": c, "metrics": {"host_k_non_knotrel_cores": cores}})
                .to_string()
        };
        let text = [
            line(0, 0.5),
            line(16, 3.0),
            line(16, 4.0),
            line(16, 3.5),
            line(64, 6.0),
        ]
        .join("\n");
        let cal = calibration_from_results(&text, &[16, 64]).unwrap();
        assert_eq!(cal[&16]["median_cores"], 3.5);
        assert_eq!(cal[&16]["observations"], 3);
        assert_eq!(cal[&64]["median_cores"], 6.0);
        assert!(
            calibration_from_results(&line(16, 1.0), &[16, 64])
                .unwrap_err()
                .contains("C=64")
        );
        let t = json!({"arm": "T", "clients": 16, "metrics": {}}).to_string();
        assert!(
            calibration_from_results(&t, &[16])
                .unwrap_err()
                .contains("arm S")
        );
        let missing = json!({"arm": "S", "clients": 16, "metrics": {}}).to_string();
        assert!(calibration_from_results(&missing, &[16]).is_err());
    }

    #[test]
    fn burn_plan_splits_cores_into_processes() {
        let p = burn_plan(3.5, 8);
        assert_eq!((p.processes, p.duty, p.capped), (4, 0.875, false));
        let p = burn_plan(4.0, 8);
        assert_eq!((p.processes, p.duty), (4, 1.0));
        let p = burn_plan(0.3, 8);
        assert_eq!((p.processes, p.duty), (1, 0.3));
        let p = burn_plan(11.0, 8);
        assert_eq!((p.processes, p.duty, p.capped), (8, 1.0, true));
        assert_eq!(burn_plan(0.0, 8).processes, 0);
        assert_eq!(burn_plan(f64::NAN, 8).processes, 0);
    }

    #[test]
    fn link_stats_apply_the_validity_threshold() {
        let ok = link_stats((1..=1000).map(|i| i as f64 / 10_000.0).collect(), 0);
        assert_eq!(ok["round_trips"], 1000);
        assert_eq!(ok["valid"], true);
        assert_eq!(ok["p99_ms"], 0.099);
        let slow = link_stats(vec![0.6; 1000], 0);
        assert_eq!(slow["valid"], false);
        let none = link_stats(Vec::new(), 1000);
        assert_eq!(none["valid"], false);
        assert_eq!(none["failures"], 1000);
    }

    #[test]
    fn tcp_round_trips_against_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (rtts, failures) = tcp_round_trips(addr, 20);
        assert_eq!((rtts.len(), failures), (20, 0));
        drop(listener);
    }

    /// Three observations around each median (spread 1.1 when `wide` is
    /// false), host K CPU `k` in every arm T observation.
    fn input(a_s: f64, a_t: f64, a_u: f64, k: f64) -> RuleInput {
        let three = |m: f64| vec![m * 0.95, m, m * 1.045];
        RuleInput {
            s: three(a_s),
            t: three(a_t),
            u: three(a_u),
            k_cpu_t: vec![k, k, k],
        }
    }

    fn outcome(r: &RuleInput) -> Outcome {
        cell_outcome(r).0
    }

    #[test]
    fn cell_rule_thresholds() {
        use Outcome::*;
        // a_T = 2 a_S, r_U = 0.5, K at 80 %: every boundary on the supported side.
        let sup = input(100.0, 200.0, 150.0, 80.0);
        assert_eq!(outcome(&sup), Supported);
        let (_, d) = cell_outcome(&sup);
        assert_eq!(d["gain_G"], 100.0);
        assert_eq!(d["recovered_share_r_U"], 0.5);
        // Just outside each supported threshold.
        assert_eq!(
            outcome(&input(100.0, 199.0, 140.0, 50.0)),
            InsufficientEvidence
        );
        assert_eq!(
            outcome(&input(100.0, 200.0, 150.1, 50.0)),
            InsufficientEvidence
        );
        assert_eq!(
            outcome(&input(100.0, 200.0, 120.0, 80.1)),
            InsufficientEvidence
        );
        // Not supported: no meaningful gain, or U keeps at least 3/4 of it.
        assert_eq!(outcome(&input(100.0, 119.0, 50.0, 50.0)), NotSupported);
        assert_eq!(outcome(&input(100.0, 90.0, 50.0, 50.0)), NotSupported);
        assert_eq!(outcome(&input(100.0, 300.0, 250.0, 50.0)), NotSupported);
        assert_eq!(
            outcome(&input(100.0, 300.0, 249.0, 50.0)),
            InsufficientEvidence
        );
        // Between the thresholds.
        assert_eq!(
            outcome(&input(100.0, 150.0, 120.0, 50.0)),
            InsufficientEvidence
        );
        // r_U can exceed 1 (U below S): still supported.
        assert_eq!(outcome(&input(100.0, 250.0, 80.0, 50.0)), Supported);
    }

    #[test]
    fn spread_bound_makes_a_client_count_insufficient() {
        let mut r = input(100.0, 300.0, 110.0, 50.0);
        assert_eq!(outcome(&r), Outcome::Supported);
        r.t = vec![241.0, 300.0, 301.0];
        assert_eq!(outcome(&r), Outcome::Supported, "spread 1.249");
        r.t = vec![239.0, 300.0, 300.0];
        let (o, d) = cell_outcome(&r);
        assert_eq!(o, Outcome::InsufficientEvidence);
        assert!(d["reason"].as_str().unwrap().contains("spread"));
        r.t = vec![];
        assert_eq!(outcome(&r), Outcome::InsufficientEvidence);
        assert_eq!(spread(&[2.0, 1.0, 1.5]), Some(2.0));
        assert_eq!(spread(&[0.0, 1.0]), None);
    }

    #[test]
    fn overall_outcome_needs_agreement() {
        use Outcome::*;
        assert_eq!(r2_outcome(&[Supported, Supported]), Supported);
        assert_eq!(r2_outcome(&[NotSupported, NotSupported]), NotSupported);
        assert_eq!(r2_outcome(&[Supported, NotSupported]), InsufficientEvidence);
        assert_eq!(
            r2_outcome(&[Supported, InsufficientEvidence]),
            InsufficientEvidence
        );
        assert_eq!(
            r2_outcome(&[InsufficientEvidence, InsufficientEvidence]),
            InsufficientEvidence
        );
    }

    fn arm_records(arm: &str, rates: &[(u64, f64)], k_cpu: f64) -> Vec<Value> {
        let mut v = Vec::new();
        for (c, r) in rates {
            for _ in 0..3 {
                v.push(flatten(&json!({"arm": arm, "clients": c, "metrics": {
                    "events_per_sec": r, "secs_to_ready": 1.0,
                    "remote_host_cpu_busy_percent": {"median": k_cpu}}})));
            }
        }
        v
    }

    fn valid_link() -> Value {
        json!({"before": {"valid": true}, "after": {"valid": true}})
    }

    #[test]
    fn decide_applies_validity_before_the_rule() {
        let mut arms = BTreeMap::new();
        arms.insert(
            "S",
            (
                arm_records("S", &[(0, 1000.0), (16, 100.0), (64, 50.0)], 0.0),
                valid_link(),
                None,
            ),
        );
        arms.insert(
            "T",
            (
                arm_records("T", &[(0, 900.0), (16, 300.0), (64, 150.0)], 60.0),
                valid_link(),
                None,
            ),
        );
        arms.insert(
            "U",
            (
                arm_records("U", &[(16, 120.0), (64, 60.0)], 0.0),
                valid_link(),
                None,
            ),
        );
        let d = decide(&arms);
        assert_eq!(d["outcome"], "hypothesis supported", "{d}");
        assert_eq!(d["per_clients"]["C=16"]["a_T"], 300.0);
        assert_eq!(d["agreement"], true);

        let mut bad_link = arms.clone();
        bad_link.get_mut("T").unwrap().1 =
            json!({"before": {"valid": true}, "after": {"valid": false}});
        let d = decide(&bad_link);
        assert_eq!(d["outcome"], "insufficient evidence");
        assert_eq!(d["rule_outcome_before_validity"], "hypothesis supported");

        let mut invalid = arms.clone();
        invalid.get_mut("U").unwrap().2 = Some("thermal warning".into());
        assert_eq!(decide(&invalid)["outcome"], "insufficient evidence");

        let mut short = arms.clone();
        short.get_mut("S").unwrap().0.pop();
        let d = decide(&short);
        assert_eq!(d["outcome"], "insufficient evidence");
        assert!(
            d["validity_failures"]
                .to_string()
                .contains("arm S C=64: 2 completed")
        );
    }

    #[test]
    fn r2_summary_reports_ratio_to_c0() {
        let results = arm_records("S", &[(0, 1000.0), (16, 250.0)], 10.0);
        let s = r2_summary(&results);
        assert_eq!(s["L2 G100k C=16"]["ratio_to_c0"], 0.25);
        assert_eq!(s["L2 G100k C=16"]["events_per_sec"]["n"], 3);
        assert_eq!(s["L2 G100k C=0"]["remote_cpu_busy_median"]["median"], 10.0);
    }
}
