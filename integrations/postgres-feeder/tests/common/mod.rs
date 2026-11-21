//! Test harness: a real PostgreSQL server (from `KNOTREL_PG_ADMIN_URL`), a
//! real `knotrel-server` child process and the feeder binary as a child
//! process.
//!
//! Each test gets its own database, feeder role and slot prefix, so tests can
//! run in parallel against one PostgreSQL server.

#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio_postgres::{Client, NoTls};

/// Registered topology: node → active, (source, target) → enabled.
pub type Topology = (BTreeMap<u64, bool>, BTreeMap<(u64, u64), bool>);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The admin URL, or `None` to skip (unless the tests are required).
pub fn admin_url() -> Option<String> {
    match std::env::var("KNOTREL_PG_ADMIN_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            if std::env::var("KNOTREL_PG_TESTS_REQUIRED").is_ok_and(|v| v == "1") {
                panic!("KNOTREL_PG_ADMIN_URL is required (KNOTREL_PG_TESTS_REQUIRED=1)");
            }
            eprintln!("skipped: set KNOTREL_PG_ADMIN_URL (see scripts/start-test-postgres.sh)");
            None
        }
    }
}

fn random() -> u64 {
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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Connects with a URL, spawning the connection task.
pub async fn connect(url: &str) -> Client {
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

/// A Knotrel server child process.
pub struct Server {
    child: Option<Child>,
    pub port: u16,
}

impl Server {
    pub async fn start(port: u16) -> Self {
        let bin = std::env::var("KNOTREL_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| repo_root().join("target/debug/knotrel-server"));
        Self::start_binary(&bin, port).await
    }

    /// Starts a specific server binary (for example a previous release).
    pub async fn start_binary(bin: &std::path::Path, port: u16) -> Self {
        assert!(
            bin.exists(),
            "{} not found: run `cargo build --bin knotrel-server` at the repository root or set KNOTREL_BIN",
            bin.display()
        );
        let child = Command::new(bin)
            .env_clear()
            .env("KNOTREL_ADDR", format!("127.0.0.1:{port}"))
            .env("KNOTREL_MODE", "source-managed")
            .env("KNOTREL_TOPOLOGY_LIFECYCLE", "true")
            .env("KNOTREL_INSPECTION", "true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn knotrel-server");
        let server = Self {
            child: Some(child),
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if reqwest::get(format!("http://127.0.0.1:{port}/health"))
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return server;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("knotrel-server did not start");
    }

    pub async fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill().await;
            let _ = c.wait().await;
        }
    }

    /// Stops the server with SIGTERM and waits for the drain; returns its exit
    /// code.
    pub async fn terminate(&mut self) -> Option<i32> {
        let mut c = self.child.take()?;
        let pid = c.id()?;
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        match tokio::time::timeout(Duration::from_secs(20), c.wait()).await {
            Ok(status) => status.ok().and_then(|s| s.code()),
            Err(_) => {
                let _ = c.kill().await;
                None
            }
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// One test's environment.
pub struct Env {
    pub name: String,
    admin_url: String,
    pub admin: Client,
    pub db: String,
    pub db_url: String,
    pub role: String,
    password: String,
    pub prefix: String,
    pub server: Server,
    feeder: Option<Child>,
    pub feeder_log: PathBuf,
    feeder_runs: u32,
    pub extra_env: Vec<(String, String)>,
}

impl Env {
    /// Creates the database, schema, publication and feeder role, and starts
    /// a Knotrel server. Returns `None` if the tests are skipped.
    pub async fn new(name: &str) -> Option<Self> {
        let admin_url = admin_url()?;
        let suffix = format!("{:08x}", random() & 0xFFFF_FFFF);
        let db = format!("kt_{name}_{suffix}");
        let role = format!("kf_{suffix}");
        let password = format!("{:016x}{:016x}", random(), random());
        let prefix = format!("kf_{suffix}");
        let root_admin = connect(&admin_url).await;
        root_admin
            .batch_execute(&format!("CREATE DATABASE {db}"))
            .await
            .unwrap();
        let db_url = with_database(&admin_url, &db);
        let admin = connect(&db_url).await;
        let sql = |f: &str| {
            std::fs::read_to_string(repo_root().join("integrations/postgres-feeder/sql").join(f))
                .unwrap()
        };
        admin.batch_execute(&sql("schema.sql")).await.unwrap();
        admin.batch_execute(&sql("publication.sql")).await.unwrap();
        admin
            .batch_execute(&format!(
                "CREATE ROLE {role} LOGIN REPLICATION PASSWORD '{password}';
                 GRANT CONNECT ON DATABASE {db} TO {role};
                 GRANT USAGE ON SCHEMA knotrel_graph TO {role};
                 GRANT SELECT ON knotrel_graph.nodes, knotrel_graph.edges TO {role};"
            ))
            .await
            .unwrap();
        let log_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/feeder-test-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let server = Server::start(free_port()).await;
        Some(Self {
            name: name.to_string(),
            admin_url,
            admin,
            db,
            db_url,
            role,
            password,
            prefix,
            server,
            feeder: None,
            feeder_log: log_dir.join(format!("{name}-{suffix}.log")),
            feeder_runs: 0,
            extra_env: Vec::new(),
        })
    }

    fn feeder_command(&self, args: &[&str], fault: Option<&str>) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_knotrel-pg-feeder"));
        let (host, port) = pg_host_port(&self.admin_url);
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "info")
            .env("KNOTREL_PG_HOST", host)
            .env("KNOTREL_PG_PORT", port.to_string())
            .env("KNOTREL_PG_USER", &self.role)
            .env("KNOTREL_PG_PASSWORD", &self.password)
            .env("KNOTREL_PG_DATABASE", &self.db)
            .env("KNOTREL_PG_SLOT_PREFIX", &self.prefix)
            .env("KNOTREL_URL", self.server.url())
            .env("KNOTREL_FEEDER_CATCH_UP_IDLE_MS", "200")
            .env("KNOTREL_FEEDER_SLOT_CHECK_SECS", "2")
            .env("KNOTREL_FEEDER_LEASE_MS", "5000");
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        if let Some(f) = fault {
            cmd.env("KNOTREL_PG_FEEDER_FAULT", f);
        }
        cmd
    }

    fn log_file(&mut self) -> std::fs::File {
        self.feeder_runs += 1;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.feeder_log)
            .unwrap();
        use std::io::Write;
        writeln!(f, "===== feeder run {} =====", self.feeder_runs).unwrap();
        f
    }

    /// Starts the feeder (optionally with a fault point armed).
    pub fn start_feeder(&mut self, fault: Option<&str>) {
        assert!(self.feeder.is_none(), "feeder already running");
        let log = self.log_file();
        let child = self
            .feeder_command(&[], fault)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn feeder");
        self.feeder = Some(child);
    }

    /// Runs a feeder subcommand to completion; returns (exit code, output).
    pub async fn feeder_command_output(
        &self,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> (i32, String) {
        let mut cmd = self.feeder_command(args, None);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().await.expect("run feeder");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Process ID of the running feeder.
    pub fn feeder_pid(&self) -> u32 {
        self.feeder
            .as_ref()
            .and_then(Child::id)
            .expect("feeder running")
    }

    /// Waits for the feeder to exit; returns its exit code (`None` if killed
    /// by a signal, as an abort is).
    pub async fn wait_feeder(&mut self, timeout: Duration) -> Option<i32> {
        let mut child = self.feeder.take().expect("feeder running");
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => status.unwrap().code(),
            Err(_) => {
                self.dump_log();
                panic!("feeder did not exit within {timeout:?}")
            }
        }
    }

    pub async fn kill_feeder(&mut self) {
        if let Some(mut c) = self.feeder.take() {
            let _ = c.kill().await;
            let _ = c.wait().await;
        }
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.feeder_log).unwrap_or_default()
    }

    pub fn dump_log(&self) {
        eprintln!(
            "----- feeder log {} -----\n{}",
            self.feeder_log.display(),
            self.log()
        );
    }

    /// Stops the server with SIGTERM and starts `bin` on the same port.
    pub async fn replace_server(&mut self, bin: &std::path::Path) -> Option<i32> {
        let code = self.server.terminate().await;
        self.server = Server::start_binary(bin, self.server.port).await;
        code
    }

    pub async fn restart_server(&mut self) {
        self.server.kill().await;
        self.server = Server::start(self.server.port).await;
    }

    /// Executes SQL as the admin in the test database.
    pub async fn sql(&self, sql: &str) {
        self.admin
            .batch_execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    pub async fn stats(&self) -> Value {
        get_json(&format!("{}/v1/stats", self.server.url())).await
    }

    /// This test's slots: (name, active, wal_status, confirmed_flush_lsn).
    pub async fn slots(&self) -> Vec<(String, bool, Option<String>, Option<String>)> {
        self.admin
            .query(
                "SELECT slot_name::text, active, wal_status, confirmed_flush_lsn::text \
                 FROM pg_replication_slots WHERE left(slot_name::text, length($1)) = $1",
                &[&format!("{}_", self.prefix)],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
            .collect()
    }

    /// Waits until the feeder has confirmed every WAL position written so far
    /// and Knotrel is ready, then compares the graphs (criterion C1).
    pub async fn converge(&self) {
        let target: String = self
            .admin
            .query_one("SELECT pg_current_wal_lsn()::text", &[])
            .await
            .unwrap()
            .get(0);
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let caught_up: bool = self
                .admin
                .query_one(
                    "SELECT coalesce(bool_or(pg_wal_lsn_diff(confirmed_flush_lsn, $2::text::pg_lsn) >= 0), false) \
                     FROM pg_replication_slots WHERE left(slot_name::text, length($1)) = $1",
                    &[&format!("{}_", self.prefix), &target],
                )
                .await
                .unwrap()
                .get(0);
            let stats = self.stats().await;
            if caught_up && stats["sync_status"] == "ready" {
                break;
            }
            if Instant::now() > deadline {
                self.dump_log();
                panic!(
                    "no convergence: caught_up={caught_up}, stats={stats}, slots={:?}",
                    self.slots().await
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.compare().await;
    }

    /// Compares Knotrel's registered topology with the tables.
    pub async fn compare(&self) {
        let before = self.stats().await;
        let knotrel = knotrel_topology(&self.server.url()).await;
        let after = self.stats().await;
        assert_eq!(
            (
                &before["instance_id"],
                &before["generation"],
                &before["state_version"],
                &before["source_position"]
            ),
            (
                &after["instance_id"],
                &after["generation"],
                &after["state_version"],
                &after["source_position"]
            ),
            "Knotrel changed while it was read"
        );
        let source = self.source_topology().await;
        if knotrel != source {
            self.dump_log();
            let missing_nodes: Vec<_> = source
                .0
                .iter()
                .filter(|(k, v)| knotrel.0.get(k) != Some(v))
                .take(10)
                .collect();
            let extra_nodes: Vec<_> = knotrel
                .0
                .iter()
                .filter(|(k, v)| source.0.get(k) != Some(v))
                .take(10)
                .collect();
            let missing_edges: Vec<_> = source
                .1
                .iter()
                .filter(|(k, v)| knotrel.1.get(k) != Some(v))
                .take(10)
                .collect();
            let extra_edges: Vec<_> = knotrel
                .1
                .iter()
                .filter(|(k, v)| source.1.get(k) != Some(v))
                .take(10)
                .collect();
            panic!(
                "graphs differ (nodes {} vs {}, edges {} vs {}): source-only nodes {missing_nodes:?}, \
                 knotrel-only nodes {extra_nodes:?}, source-only edges {missing_edges:?}, knotrel-only edges {extra_edges:?}",
                source.0.len(),
                knotrel.0.len(),
                source.1.len(),
                knotrel.1.len()
            );
        }
        let position = after["source_position"].as_str().unwrap_or("");
        assert!(
            position.ends_with(";end"),
            "after convergence Knotrel must be at a transaction boundary, found {position}"
        );
    }

    pub async fn source_topology(&self) -> Topology {
        let nodes = self
            .admin
            .query("SELECT id, active FROM knotrel_graph.nodes", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get::<_, i64>(0) as u64, r.get(1)))
            .collect();
        let edges = self
            .admin
            .query(
                "SELECT source, target, enabled FROM knotrel_graph.edges",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (
                    (r.get::<_, i64>(0) as u64, r.get::<_, i64>(1) as u64),
                    r.get(2),
                )
            })
            .collect();
        (nodes, edges)
    }

    /// Drops the slots, the database and the role.
    pub async fn cleanup(mut self) {
        self.kill_feeder().await;
        self.server.kill().await;
        let root = connect(&self.admin_url).await;
        let _ = root
            .execute(
                "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
                 WHERE left(slot_name::text, length($1)) = $1 AND active_pid IS NOT NULL",
                &[&format!("{}_", self.prefix)],
            )
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = root
            .execute(
                "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
                 WHERE left(slot_name::text, length($1)) = $1",
                &[&format!("{}_", self.prefix)],
            )
            .await;
        let _ = root
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE); DROP ROLE IF EXISTS {};",
                self.db, self.role
            ))
            .await;
    }
}

fn pg_host_port(url: &str) -> (String, u16) {
    let after_at = url.rsplit_once('@').map(|(_, r)| r).unwrap_or(url);
    let hostport = after_at.split('/').next().unwrap();
    match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap()),
        None => (hostport.to_string(), 5432),
    }
}

pub async fn get_json(url: &str) -> Value {
    let r = reqwest::get(url)
        .await
        .unwrap_or_else(|e| panic!("GET {url}: {e}"));
    r.json().await.unwrap()
}

/// Reads the full registered topology through the inspection routes.
pub async fn knotrel_topology(base: &str) -> Topology {
    let mut nodes = BTreeMap::new();
    let mut after = String::new();
    loop {
        let url = if after.is_empty() {
            format!("{base}/v1/nodes?limit=1000")
        } else {
            format!("{base}/v1/nodes?limit=1000&after={after}")
        };
        let page = get_json(&url).await;
        for n in page["nodes"].as_array().unwrap() {
            nodes.insert(
                n["node"].as_str().unwrap().parse().unwrap(),
                n["active"].as_bool().unwrap(),
            );
        }
        match page["next_after"].as_str() {
            Some(next) => after = next.to_string(),
            None => break,
        }
    }
    let mut edges = BTreeMap::new();
    let mut cursor: Option<(String, Option<String>)> = None;
    loop {
        let url = match &cursor {
            None => format!("{base}/v1/edges?limit=1000"),
            Some((s, Some(t))) => {
                format!("{base}/v1/edges?limit=1000&after_source={s}&after_target={t}")
            }
            Some((s, None)) => format!("{base}/v1/edges?limit=1000&after_source={s}"),
        };
        let page = get_json(&url).await;
        for e in page["edges"].as_array().unwrap() {
            edges.insert(
                (
                    e["source"].as_str().unwrap().parse().unwrap(),
                    e["target"].as_str().unwrap().parse().unwrap(),
                ),
                e["enabled"].as_bool().unwrap(),
            );
        }
        match page["next_after_source"].as_str() {
            Some(s) => {
                cursor = Some((
                    s.to_string(),
                    page["next_after_target"].as_str().map(str::to_string),
                ))
            }
            None => break,
        }
    }
    (nodes, edges)
}

/// Background writers making random graph transactions until stopped.
pub struct Writers {
    stop: Arc<AtomicBool>,
    handles: Vec<tokio::task::JoinHandle<()>>,
    pub commits: Arc<AtomicU64>,
}

impl Writers {
    /// Starts `n` writers on node IDs `0..id_range`.
    pub async fn start(env: &Env, n: usize, id_range: i64, seed: u64) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let commits = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for w in 0..n {
            let client = connect(&env.db_url).await;
            let stop = stop.clone();
            let commits = commits.clone();
            let mut rng = Rng(seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(w as u64 + 1));
            handles.push(tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    let _ = random_transaction(&client, &mut rng, id_range, &commits).await;
                }
            }));
        }
        Self {
            stop,
            handles,
            commits,
        }
    }

    pub async fn stop(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.handles {
            let _ = h.await;
        }
        self.commits.load(Ordering::Relaxed)
    }
}

/// xorshift64*: deterministic, for test data only.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: i64) -> i64 {
        (self.next() % n as u64) as i64
    }
}

async fn random_transaction(
    client: &Client,
    rng: &mut Rng,
    range: i64,
    commits: &AtomicU64,
) -> Result<(), tokio_postgres::Error> {
    client.batch_execute("BEGIN").await?;
    let ops = 1 + rng.below(5);
    let result: Result<(), tokio_postgres::Error> = async {
        for _ in 0..ops {
            let a = rng.below(range);
            let b = rng.below(range);
            let flag = rng.below(4) != 0;
            let (s, t) = (a.min(b), a.max(b));
            match rng.below(10) {
                0 | 1 => {
                    client
                        .execute(
                            "INSERT INTO knotrel_graph.nodes VALUES ($1, $2) ON CONFLICT DO NOTHING",
                            &[&a, &flag],
                        )
                        .await?;
                }
                2 => {
                    client
                        .execute(
                            "UPDATE knotrel_graph.nodes SET active = NOT active WHERE id = $1",
                            &[&a],
                        )
                        .await?;
                }
                3 => {
                    client
                        .execute("DELETE FROM knotrel_graph.nodes WHERE id = $1", &[&a])
                        .await?;
                }
                4 | 5 if s != t => {
                    client
                        .execute(
                            "INSERT INTO knotrel_graph.edges \
                             SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = $1) \
                               AND EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = $2) \
                             ON CONFLICT DO NOTHING",
                            &[&s, &t, &flag],
                        )
                        .await?;
                }
                6 => {
                    client
                        .execute(
                            "UPDATE knotrel_graph.edges SET enabled = NOT enabled \
                             WHERE source = $1 AND target > $1 AND target <= $1 + 20",
                            &[&a],
                        )
                        .await?;
                }
                7 => {
                    client
                        .execute(
                            "DELETE FROM knotrel_graph.edges WHERE source = $1 AND target IN \
                             (SELECT target FROM knotrel_graph.edges WHERE source = $1 LIMIT 1)",
                            &[&a],
                        )
                        .await?;
                }
                8 => {
                    // Key update of a node without edges.
                    client
                        .execute(
                            "UPDATE knotrel_graph.nodes SET id = $2 WHERE id = $1 \
                             AND NOT EXISTS (SELECT 1 FROM knotrel_graph.edges WHERE source = $1 OR target = $1) \
                             AND NOT EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = $2)",
                            &[&a, &b],
                        )
                        .await?;
                }
                _ => {
                    // Key update of an edge: move its target.
                    client
                        .execute(
                            "UPDATE knotrel_graph.edges SET target = $2 \
                             WHERE source = $1 AND target = (SELECT min(target) FROM knotrel_graph.edges WHERE source = $1) \
                               AND $2 > $1 \
                               AND EXISTS (SELECT 1 FROM knotrel_graph.nodes WHERE id = $2) \
                               AND NOT EXISTS (SELECT 1 FROM knotrel_graph.edges WHERE source = $1 AND target = $2)",
                            &[&s, &t],
                        )
                        .await?;
                }
            }
        }
        Ok(())
    }
    .await;
    if result.is_ok() && rng.below(10) != 0 {
        client.batch_execute("COMMIT").await?;
        commits.fetch_add(1, Ordering::Relaxed);
    } else {
        let _ = client.batch_execute("ROLLBACK").await;
    }
    result
}

/// Loads `nodes` nodes and about `edges` random edges.
pub async fn preload(env: &Env, nodes: i64, edges: i64) {
    env.sql(&format!(
        "INSERT INTO knotrel_graph.nodes SELECT g, g % 11 <> 0 FROM generate_series(0, {}) g;
         INSERT INTO knotrel_graph.edges
           SELECT DISTINCT least(a, b), greatest(a, b), (a + b) % 13 <> 0
           FROM (SELECT (random() * {m})::int8 a, (random() * {m})::int8 b FROM generate_series(1, {edges})) r
           WHERE a <> b ON CONFLICT DO NOTHING;",
        nodes - 1,
        m = nodes - 1
    ))
    .await;
}

/// Waits until `cond` holds or panics after `timeout`.
pub async fn wait_for<F, Fut>(what: &str, timeout: Duration, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    while !cond().await {
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
