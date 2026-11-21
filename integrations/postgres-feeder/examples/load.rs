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

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
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
    server: Child,
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
        let port = free_port();
        let server = Command::new(&s.knotrel_bin)
            .env_clear()
            .env("KNOTREL_ADDR", format!("127.0.0.1:{port}"))
            .env("KNOTREL_MODE", "source-managed")
            .env("KNOTREL_TOPOLOGY_LIFECYCLE", "true")
            .env("KNOTREL_MAX_NODES", "unlimited")
            .env("KNOTREL_MAX_EDGES", "unlimited")
            .env("KNOTREL_METRICS_ENABLED", "true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn knotrel-server");
        let base = format!("http://127.0.0.1:{port}");
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
        let _ = self.server.kill().await;
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
    tokio::spawn(async move {
        let mut samples = Vec::new();
        let Ok(mut child) = Command::new("top")
            .args(["-l", "0", "-s", "1", "-n", "0"])
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
    let cpu_stop = Arc::new(AtomicBool::new(false));
    let cpu = spawn_cpu_sampler(cpu_stop.clone());
    let clients = spawn_clients(&obs.base, cell.clients, n, stop.clone(), record.clone());
    tokio::time::sleep(WARMUP_L2).await;
    record.store(true, Ordering::Relaxed);
    let metrics_before = phase_metrics(&obs.base).await;
    let start = Instant::now();
    obs.start_feeder(s, &s.admin_url);
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
    let metrics_after = phase_metrics(&obs.base).await;
    // Keep sampling one more second so a short catch-up still gets a sample
    // that ends after it.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    cpu_stop.store(true, Ordering::Relaxed);
    let all_cpu = cpu.await.unwrap_or_default();
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
    record.store(false, Ordering::Relaxed);
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
    json!({
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
    })
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
    let s = Settings {
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
    };
    std::fs::create_dir_all(&s.out).unwrap();
    let results_path = s.out.join("results.jsonl");
    let run_start = Instant::now();
    let mut results = Vec::new();
    for rep in 1..=s.reps {
        for (i, cell) in CELLS.iter().enumerate() {
            let key = format!("{} {} C={}", cell.scenario, cell.graph, cell.clients);
            if s.cells
                .as_ref()
                .is_some_and(|c| !c.iter().any(|k| k == &key))
            {
                continue;
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
            let record = json!({
                "scenario": cell.scenario, "graph": cell.graph, "clients": cell.clients, "rep": rep,
                "window_secs": s.window.as_secs(), "backlog": s.backlog,
                "metrics": metrics,
            });
            // Flatten metrics for evaluation.
            let mut flat = record.clone();
            if let (Some(obj), Some(m)) = (flat.as_object_mut(), record["metrics"].as_object()) {
                for (k, v) in m {
                    obj.insert(k.clone(), v.clone());
                }
            }
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
    let summary = evaluate(&results);
    let summary_text = serde_json::to_string_pretty(&json!({
        "measurement_secs": run_start.elapsed().as_secs_f64(),
        "summary": summary,
    }))
    .unwrap();
    std::fs::write(s.out.join("summary.json"), &summary_text).unwrap();
    println!("{summary_text}");
}
