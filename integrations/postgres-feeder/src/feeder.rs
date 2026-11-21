//! The feeder state machine: bootstrap, resume, stream, rebuild.
//!
//! Every step follows the design document
//! (`docs/integrations/postgresql-feeder.md`); section numbers below refer to
//! it.

use crate::config::Config;
use crate::fault;
use crate::knotrel::{Event, Knotrel, Reply, Session, Uncertain};
use crate::mapping::{GraphOp, Relations, Translation};
use crate::pgoutput::{self, Message};
use crate::position::{Lineage, Position, event_id};
use crate::postgres::{Control, PgError};
use pgwire_replication::{Lsn, ReplicationClient, ReplicationConfig, ReplicationEvent, TlsConfig};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Delay after which a non-empty batch is sent even if more events follow.
const MAX_BATCH_DELAY: Duration = Duration::from_millis(50);
/// Idle delay after which a non-empty batch is sent.
const FLUSH_IDLE: Duration = Duration::from_millis(5);
/// Longest backoff between retries.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Why the feeder stopped for good. Each kind has its own exit code.
#[derive(Debug)]
pub enum Fatal {
    /// Invalid configuration or unusable server settings (exit code 2).
    Config(String),
    /// Unsupported schema or value (exit code 3).
    Schema(String),
    /// Knotrel capacity exceeded (exit code 4).
    Capacity(String),
    /// Another feeder took over the Knotrel server (exit code 5).
    Fenced(String),
    /// Anything else (exit code 1).
    Other(String),
}

impl Fatal {
    /// Process exit code.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Other(_) => 1,
            Self::Config(_) => 2,
            Self::Schema(_) => 3,
            Self::Capacity(_) => 4,
            Self::Fenced(_) => 5,
        }
    }
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(m) | Self::Schema(m) | Self::Capacity(m) | Self::Other(m) => {
                f.write_str(m)
            }
            Self::Fenced(m) => write!(f, "fenced: {m}"),
        }
    }
}

/// Why a streaming session ended without stopping the feeder.
#[derive(Debug)]
enum Restart {
    /// Knotrel lost its state (new instance): rebuild on the new instance.
    KnotrelRestarted,
    /// Rebuild from a new snapshot (replace import), for the given reason.
    Rebuild(String),
    /// Transient failure: reconnect and resume.
    Reconnect(String),
}

#[derive(Debug)]
enum Exit {
    Fatal(Fatal),
    Restart(Restart),
    Shutdown,
}

impl From<Fatal> for Exit {
    fn from(f: Fatal) -> Self {
        Self::Fatal(f)
    }
}

fn reconnect(reason: impl Into<String>) -> Exit {
    Exit::Restart(Restart::Reconnect(reason.into()))
}

fn rebuild(reason: impl Into<String>) -> Exit {
    Exit::Restart(Restart::Rebuild(reason.into()))
}

fn pg_exit(e: PgError) -> Exit {
    match e {
        PgError::Connection(m) => reconnect(m),
        PgError::Unsupported(m) => Exit::Fatal(Fatal::Schema(m)),
    }
}

/// A random 64-bit value (not cryptographic; used for names only).
fn random_u64() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    h.finish()
}

/// Strictly increasing feeder epochs from the wall clock (assumption A4).
#[derive(Debug, Default)]
struct EpochClock {
    last: u64,
}

impl EpochClock {
    fn next(&mut self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last = now.max(self.last + 1);
        self.last
    }
}

/// Knotrel's applied frontier, as reported by a session registration.
#[derive(Clone, Debug)]
struct Frontier {
    generation: u64,
    applied_seq: u64,
    applied_pos: Option<String>,
    status: String,
}

impl Frontier {
    fn from_reply(r: &Reply) -> Result<Self, Exit> {
        Ok(Self {
            generation: r.u64("generation").ok_or_else(|| {
                Exit::Fatal(Fatal::Other("session answer without generation".into()))
            })?,
            applied_seq: r.u64("applied_delivery_sequence").unwrap_or(0),
            applied_pos: r.str("applied_source_position").map(str::to_string),
            status: r.str("status").unwrap_or("").to_string(),
        })
    }
}

/// Classifies a non-success Knotrel answer that every route can give.
fn common_failure(r: &Reply) -> Option<Exit> {
    if r.is("stale_instance_id") {
        return Some(Exit::Restart(Restart::KnotrelRestarted));
    }
    if r.status == 503 || r.is("busy") || r.is("import_in_progress") || r.is("shutting_down") {
        return Some(reconnect(format!("Knotrel unavailable: {}", r.describe())));
    }
    None
}

struct Backoff {
    current: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            current: Duration::from_millis(100),
        }
    }

    fn reset(&mut self) {
        self.current = Duration::from_millis(100);
    }

    async fn wait(&mut self, shutdown: &mut watch::Receiver<bool>) -> bool {
        let d = self.current;
        self.current = (self.current * 2).min(MAX_BACKOFF);
        tokio::select! {
            _ = tokio::time::sleep(d) => false,
            _ = shutdown.changed() => true,
        }
    }
}

/// What to do with the current Knotrel state.
enum Plan {
    Resume(Position),
    Rebuild(String),
    GapThenRebuild(String),
}

/// The feeder, across streaming sessions.
pub struct Feeder {
    cfg: Config,
    knotrel: Knotrel,
    control: Control,
    session_id: String,
    clock: EpochClock,
    backoff: Backoff,
    /// A rebuild is required before resuming is allowed again.
    force_rebuild: Option<String>,
}

/// Preflight checks shared by `run` and `check` (design document, section
/// 4.3 and assumption A1).
///
/// # Errors
///
/// [`Fatal::Config`] or [`Fatal::Schema`] naming the problem.
pub async fn preflight(cfg: &Config) -> Result<(Control, Knotrel), Fatal> {
    let knotrel = Knotrel::new(&cfg.knotrel_url, cfg.request_timeout, cfg.import_timeout)
        .map_err(Fatal::Config)?;
    let control = Control::connect(&cfg.pg)
        .await
        .map_err(|e| Fatal::Config(e.to_string()))?;
    let version = control
        .check_server(cfg.allow_unbounded_wal)
        .await
        .map_err(|e| Fatal::Config(e.to_string()))?;
    control
        .validate_schema(&cfg.tables, &cfg.publication)
        .await
        .map_err(|e| match e {
            PgError::Unsupported(m) => Fatal::Schema(m),
            PgError::Connection(m) => Fatal::Config(m),
        })?;
    let info = knotrel
        .info()
        .await
        .map_err(|e| Fatal::Config(format!("cannot reach Knotrel at {}: {e}", cfg.knotrel_url)))?;
    if info.str("mode") != Some("source_managed") {
        return Err(Fatal::Config(
            "Knotrel is not in source-managed mode (set KNOTREL_MODE=source-managed)".into(),
        ));
    }
    if info
        .body
        .get("topology_lifecycle")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(Fatal::Config(
            "Knotrel lifecycle support is off (set KNOTREL_TOPOLOGY_LIFECYCLE=true); the \
             feeder maps nodes.active and edges.enabled to lifecycle flags"
                .into(),
        ));
    }
    tracing::info!(
        postgres = ?cfg.pg,
        server_version_num = version,
        publication = %cfg.publication,
        knotrel = %cfg.knotrel_url,
        "preflight passed"
    );
    Ok((control, knotrel))
}

impl Feeder {
    /// Runs the preflight checks and builds the feeder.
    ///
    /// # Errors
    ///
    /// See [`preflight`].
    pub async fn new(cfg: Config) -> Result<Self, Fatal> {
        let (control, knotrel) = preflight(&cfg).await?;
        Ok(Self {
            cfg,
            knotrel,
            control,
            session_id: format!("pgfeeder-{:016x}", random_u64()),
            clock: EpochClock::default(),
            backoff: Backoff::new(),
            force_rebuild: None,
        })
    }

    /// Runs until `shutdown` becomes true or a fatal error occurs.
    ///
    /// # Errors
    ///
    /// The [`Fatal`] condition that stopped the feeder.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), Fatal> {
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            match self.session_cycle(&mut shutdown).await {
                Exit::Shutdown => return Ok(()),
                Exit::Fatal(f) => return Err(f),
                Exit::Restart(Restart::KnotrelRestarted) => {
                    tracing::warn!(
                        "Knotrel instance changed: its graph is gone; rebuilding on the new instance"
                    );
                    self.backoff.reset();
                }
                Exit::Restart(Restart::Rebuild(reason)) => {
                    tracing::warn!(%reason, "rebuild required");
                    self.force_rebuild = Some(reason);
                    if self.backoff.wait(&mut shutdown).await {
                        return Ok(());
                    }
                }
                Exit::Restart(Restart::Reconnect(reason)) => {
                    tracing::warn!(%reason, "reconnecting");
                    if self.backoff.wait(&mut shutdown).await {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn ensure_control(&mut self) -> Result<(), Exit> {
        if self.control.is_closed() {
            self.control = Control::connect(&self.cfg.pg).await.map_err(pg_exit)?;
        }
        Ok(())
    }

    /// Registers a new epoch on the current Knotrel instance.
    async fn register(&mut self) -> Result<(Session, Frontier), Exit> {
        let info = self
            .knotrel
            .info()
            .await
            .map_err(|e| reconnect(e.to_string()))?;
        let instance_id = info
            .str("instance_id")
            .ok_or_else(|| {
                reconnect(format!(
                    "GET /v1/info without instance_id: {}",
                    info.describe()
                ))
            })?
            .to_string();
        let session = Session {
            instance_id,
            session_id: self.session_id.clone(),
            epoch: self.clock.next(),
        };
        let r = self
            .knotrel
            .session(&session, self.cfg.lease.as_millis() as u64)
            .await
            .map_err(|e| reconnect(e.to_string()))?;
        if r.ok() {
            let frontier = Frontier::from_reply(&r)?;
            tracing::info!(
                instance = %session.instance_id,
                epoch = session.epoch,
                generation = frontier.generation,
                applied = frontier.applied_seq,
                status = %frontier.status,
                "feeder session registered"
            );
            return Ok((session, frontier));
        }
        if r.is("fenced_feeder_session") {
            return Err(Exit::Fatal(Fatal::Fenced(format!(
                "Knotrel rejected epoch {}: a feeder with a higher epoch owns the server",
                session.epoch
            ))));
        }
        Err(common_failure(&r).unwrap_or_else(|| {
            Exit::Fatal(Fatal::Other(format!(
                "session registration failed: {}",
                r.describe()
            )))
        }))
    }

    async fn session_cycle(&mut self, shutdown: &mut watch::Receiver<bool>) -> Exit {
        if let Err(e) = self.ensure_control().await {
            return e;
        }
        let (session, frontier) = match self.register().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        let plan = match &self.force_rebuild {
            Some(reason) => Plan::Rebuild(reason.clone()),
            None => match self.decide(&frontier).await {
                Ok(p) => p,
                Err(e) => return e,
            },
        };
        let stream = match plan {
            Plan::Resume(position) => self.resume(session, frontier, position).await,
            Plan::GapThenRebuild(reason) => {
                if let Err(e) = self.declare_gap(&session, &frontier, &reason).await {
                    return e;
                }
                self.force_rebuild = Some(reason.clone());
                self.bootstrap(session, frontier, &reason).await
            }
            Plan::Rebuild(reason) => {
                self.force_rebuild = Some(reason.clone());
                self.bootstrap(session, frontier, &reason).await
            }
        };
        match stream {
            Ok(mut stream) => {
                self.backoff.reset();
                let exit = stream.run(&mut self.clock, shutdown).await;
                stream.close().await;
                exit
            }
            Err(e) => e,
        }
    }

    /// Resume or rebuild (section 8.3).
    async fn decide(&mut self, f: &Frontier) -> Result<Plan, Exit> {
        if f.generation == 0 {
            return Ok(Plan::Rebuild("Knotrel has no graph yet".into()));
        }
        if !matches!(f.status.as_str(), "catching_up" | "ready" | "stale") {
            return Ok(Plan::Rebuild(format!("Knotrel status is {}", f.status)));
        }
        let Some(raw) = &f.applied_pos else {
            return Ok(Plan::Rebuild("Knotrel reports no source position".into()));
        };
        let position = match Position::parse(raw) {
            Ok(p) => p,
            Err(e) => {
                return Ok(Plan::Rebuild(format!(
                    "Knotrel's position is not this feeder's: {e}"
                )));
            }
        };
        match self
            .control
            .slot(&position.lineage.slot)
            .await
            .map_err(pg_exit)?
        {
            None => Ok(Plan::GapThenRebuild(format!(
                "slot {} of the current lineage no longer exists",
                position.lineage.slot
            ))),
            Some(slot) if !slot.usable() => Ok(Plan::GapThenRebuild(format!(
                "slot {} was invalidated (its WAL was removed)",
                slot.name
            ))),
            Some(slot) if !self.control.owns(&slot, &self.cfg.slot_prefix) => {
                Ok(Plan::GapThenRebuild(format!(
                    "slot {} does not belong to this feeder (prefix, plugin or database differ)",
                    slot.name
                )))
            }
            Some(_) => Ok(Plan::Resume(position)),
        }
    }

    async fn declare_gap(&self, s: &Session, f: &Frontier, reason: &str) -> Result<(), Exit> {
        declare_gap(&self.knotrel, s, f.generation, f.applied_seq, reason).await
    }

    fn repl_config(&self, slot: &str) -> ReplicationConfig {
        ReplicationConfig::new(
            self.cfg.pg.host.clone(),
            self.cfg.pg.user.clone(),
            self.cfg.pg.password.clone(),
            self.cfg.pg.database.clone(),
            slot.to_string(),
            self.cfg.publication.as_str(),
        )
        .with_port(self.cfg.pg.port)
        .with_tls(TlsConfig::disabled())
        // 0/0: PostgreSQL starts a logical slot at its confirmed position.
        .with_start_lsn(Lsn::ZERO)
        .with_status_interval(Duration::from_millis(500))
        .with_wakeup_interval(Duration::from_secs(1))
        .with_buffer_size(8192)
        .with_application_name("knotrel-pg-feeder")
    }

    /// Bootstrap a new lineage and import it (sections 5 and 8.7).
    async fn bootstrap(
        &mut self,
        session: Session,
        frontier: Frontier,
        reason: &str,
    ) -> Result<Stream, Exit> {
        let slot = format!(
            "{}_{:012x}",
            self.cfg.slot_prefix,
            random_u64() & 0xFFFF_FFFF_FFFF
        );
        tracing::info!(%slot, %reason, "bootstrapping a new lineage");
        let mut repl = ReplicationClient::connect_gated(self.repl_config(&slot))
            .await
            .map_err(|e| reconnect(format!("replication connection: {e}")))?;
        let system = repl
            .identify_system()
            .await
            .map_err(|e| reconnect(format!("IDENTIFY_SYSTEM: {e}")))?;
        let rows = repl
            .simple_query(&format!(
                "CREATE_REPLICATION_SLOT {slot} LOGICAL pgoutput (SNAPSHOT 'export')"
            ))
            .await
            .map_err(|e| reconnect(format!("CREATE_REPLICATION_SLOT: {e}")))?;
        let text = |row: &pgwire_replication::QueryRow, i: usize| {
            row.get(i)
                .and_then(|v| v.as_ref())
                .map(|b| String::from_utf8_lossy(b).into_owned())
        };
        let row = rows
            .first()
            .ok_or_else(|| reconnect("CREATE_REPLICATION_SLOT returned no row"))?;
        let consistent_point = text(row, 1)
            .and_then(|s| Lsn::parse(&s).ok())
            .ok_or_else(|| reconnect("CREATE_REPLICATION_SLOT: bad consistent_point"))?;
        let snapshot =
            text(row, 2).ok_or_else(|| reconnect("CREATE_REPLICATION_SLOT: no snapshot"))?;
        fault::hit("after_slot_created");

        let (nodes, edges) = self
            .control
            .read_image(&self.cfg.tables, &snapshot)
            .await
            .map_err(pg_exit)?;
        fault::hit("after_snapshot_read");

        let lineage = Lineage {
            system_id: system.system_id.clone(),
            timeline: system.timeline,
            slot: slot.clone(),
        };
        let position = Position::snapshot(lineage.clone(), consistent_point);
        let source_id = source_id(&system.system_id, &self.cfg);
        let source_epoch = lineage.source_epoch();
        let etag = if frontier.generation > 0 {
            let stats = self
                .knotrel
                .stats()
                .await
                .map_err(|e| reconnect(e.to_string()))?;
            if !stats.ok() {
                return Err(reconnect(format!("GET /v1/stats: {}", stats.describe())));
            }
            if stats.str("instance_id") != Some(session.instance_id.as_str()) {
                return Err(Exit::Restart(Restart::KnotrelRestarted));
            }
            Some(
                stats
                    .etag
                    .ok_or_else(|| reconnect("GET /v1/stats without ETag"))?,
            )
        } else {
            None
        };
        let r = self
            .knotrel
            .import(
                &session,
                etag.as_deref(),
                &source_id,
                &source_epoch,
                &position.to_string(),
                &nodes,
                &edges,
            )
            .await
            .map_err(|e| reconnect(format!("import: {e}")))?;
        if !r.ok() {
            return Err(import_failure(&r));
        }
        let generation = r.u64("generation").unwrap_or(frontier.generation + 1);
        tracing::info!(
            generation,
            nodes = nodes.len(),
            edges = edges.len(),
            position = %position,
            "image imported"
        );
        fault::hit("after_import");
        self.force_rebuild = None;
        drop((nodes, edges));

        self.drop_old_slots(&slot).await;
        repl.start()
            .await
            .map_err(|e| reconnect(format!("START_REPLICATION: {e}")))?;
        repl.wait_started()
            .await
            .map_err(|e| reconnect(format!("START_REPLICATION: {e}")))?;
        Ok(Stream::new(
            self,
            repl,
            session,
            StreamStart {
                generation,
                lineage,
                source_id,
                applied_seq: 0,
                applied_pos: position.to_string(),
                skip_until: None,
                needs_barrier: true,
            },
        ))
    }

    /// Resume the current lineage after a feeder or connection restart
    /// (sections 8.2 and 8.3).
    async fn resume(
        &mut self,
        session: Session,
        frontier: Frontier,
        position: Position,
    ) -> Result<Stream, Exit> {
        let slot = position.lineage.slot.clone();
        let mut repl = ReplicationClient::connect_gated(self.repl_config(&slot))
            .await
            .map_err(|e| reconnect(format!("replication connection: {e}")))?;
        let system = repl
            .identify_system()
            .await
            .map_err(|e| reconnect(format!("IDENTIFY_SYSTEM: {e}")))?;
        if system.system_id != position.lineage.system_id
            || system.timeline != position.lineage.timeline
        {
            let reason = format!(
                "source lineage changed (system {} timeline {} instead of system {} timeline {})",
                system.system_id,
                system.timeline,
                position.lineage.system_id,
                position.lineage.timeline
            );
            self.declare_gap(&session, &frontier, &reason).await?;
            return Err(rebuild(reason));
        }
        self.drop_old_slots(&slot).await;
        repl.start()
            .await
            .map_err(|e| reconnect(format!("START_REPLICATION: {e}")))?;
        repl.wait_started()
            .await
            .map_err(|e| reconnect(format!("START_REPLICATION on {slot}: {e}")))?;
        tracing::info!(
            %slot,
            applied = frontier.applied_seq,
            position = %position,
            "resuming the current lineage"
        );
        let source_id = source_id(&system.system_id, &self.cfg);
        Ok(Stream::new(
            self,
            repl,
            session,
            StreamStart {
                generation: frontier.generation,
                lineage: position.lineage.clone(),
                source_id,
                applied_seq: frontier.applied_seq,
                applied_pos: frontier.applied_pos.clone().unwrap_or_default(),
                skip_until: Some(position),
                needs_barrier: frontier.status != "ready",
            },
        ))
    }

    /// Drops this feeder's other slots (section 9). A slot still held by a
    /// closing session is retried briefly, then left for the next cleanup.
    async fn drop_old_slots(&self, keep: &str) {
        for attempt in 0..10 {
            match self
                .control
                .drop_own_slots(&self.cfg.slot_prefix, Some(keep))
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "slot cleanup failed");
                    return;
                }
            }
            match self.control.slots(&self.cfg.slot_prefix).await {
                Ok(slots)
                    if slots.iter().all(|s| {
                        s.name == keep || !self.control.owns(s, &self.cfg.slot_prefix)
                    }) =>
                {
                    return;
                }
                Ok(_) if attempt < 9 => tokio::time::sleep(Duration::from_millis(200)).await,
                _ => return,
            }
        }
    }
}

fn source_id(system_id: &str, cfg: &Config) -> String {
    format!(
        "pg;sys={system_id};db={};pub={}",
        cfg.pg.database, cfg.publication
    )
}

fn import_failure(r: &Reply) -> Exit {
    if let Some(e) = common_failure(r) {
        return e;
    }
    if r.status == 413 {
        return Exit::Fatal(Fatal::Capacity(format!(
            "the graph image exceeds Knotrel's KNOTREL_MAX_IMPORT_BODY_BYTES: {}",
            r.describe()
        )));
    }
    if r.is("limit_exceeded") || r.is("generation_exhausted") {
        return Exit::Fatal(Fatal::Capacity(format!("import refused: {}", r.describe())));
    }
    if r.is("fenced_feeder_session") {
        return Exit::Fatal(Fatal::Fenced(format!("import refused: {}", r.describe())));
    }
    if r.status == 412 || r.is("lease_expired") || r.is("precondition_failed") {
        return reconnect(format!("import refused, retrying: {}", r.describe()));
    }
    Exit::Fatal(Fatal::Other(format!("import refused: {}", r.describe())))
}

async fn declare_gap(
    knotrel: &Knotrel,
    s: &Session,
    generation: u64,
    applied: u64,
    reason: &str,
) -> Result<(), Exit> {
    if generation == 0 {
        return Ok(());
    }
    match knotrel.gap(s, generation, applied + 1, reason).await {
        Ok(r) if r.ok() => {
            tracing::warn!(%reason, "declared a source gap: Knotrel is desynchronized until the rebuild");
            Ok(())
        }
        Ok(r) if r.is("stale_instance_id") => Err(Exit::Restart(Restart::KnotrelRestarted)),
        Ok(r) => {
            tracing::warn!(answer = %r.describe(), "gap declaration refused; rebuilding anyway");
            Ok(())
        }
        Err(e) => Err(reconnect(e.to_string())),
    }
}

/// Heartbeat observations (section 8.5).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Beat {
    Status(String),
    InstanceChanged,
    Fenced,
    LeaseExpired,
    Unreachable,
}

fn spawn_heartbeat(
    knotrel: Knotrel,
    session: watch::Receiver<Session>,
    interval: Duration,
) -> (JoinHandle<()>, watch::Receiver<Beat>) {
    let (tx, rx) = watch::channel(Beat::Status(String::new()));
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let s = session.borrow().clone();
            let beat = match knotrel.heartbeat(&s).await {
                Ok(r) if r.ok() => Beat::Status(r.str("status").unwrap_or("").to_string()),
                Ok(r) if r.is("stale_instance_id") => Beat::InstanceChanged,
                Ok(r) if r.is("fenced_feeder_session") => Beat::Fenced,
                Ok(r) if r.is("lease_expired") => Beat::LeaseExpired,
                Ok(r) => {
                    tracing::debug!(answer = %r.describe(), "heartbeat refused");
                    Beat::Unreachable
                }
                Err(e) => {
                    tracing::debug!(error = %e, "heartbeat failed");
                    Beat::Unreachable
                }
            };
            if tx.send(beat).is_err() {
                return;
            }
        }
    });
    (handle, rx)
}

struct Txn {
    lsn: Lsn,
    index: u32,
}

struct StreamStart {
    generation: u64,
    lineage: Lineage,
    source_id: String,
    applied_seq: u64,
    applied_pos: String,
    skip_until: Option<Position>,
    needs_barrier: bool,
}

/// One streaming session on one lineage, one Knotrel instance and one
/// generation.
struct Stream {
    knotrel: Knotrel,
    control_params: crate::postgres::PgParams,
    /// Control connection for WAL positions, opened on demand.
    control: Option<Control>,
    slot_prefix: String,
    max_batch: usize,
    lease: Duration,
    catch_up_idle: Duration,
    slot_check_interval: Duration,
    repl: ReplicationClient,
    session: Session,
    session_tx: watch::Sender<Session>,
    heartbeat: JoinHandle<()>,
    beats: watch::Receiver<Beat>,
    generation: u64,
    lineage: Lineage,
    source_id: String,
    source_epoch: String,
    relations: Relations,
    /// Knotrel's acknowledged frontier.
    applied_seq: u64,
    applied_pos: String,
    /// Last assigned sequence + 1, and the position of the last assigned event.
    next_seq: u64,
    last_pos: String,
    /// Events at or before this position are already in Knotrel (resume).
    skip_until: Option<Position>,
    txn: Option<Txn>,
    /// The latest event of the open transaction, kept back until it is known
    /// whether it is the transaction's last.
    held: Option<Event>,
    /// Events ready to send, contiguous after `applied_seq`.
    pending: Vec<Event>,
    pending_since: Option<Instant>,
    /// Finished transactions not yet confirmed: (commit end LSN, last sequence).
    commits: VecDeque<(Lsn, u64)>,
    needs_barrier: bool,
    /// WAL position to reach before the barrier: sampled from
    /// `pg_current_wal_lsn()` when a barrier becomes necessary.
    barrier_target: Option<Lsn>,
    /// Highest commit end or keepalive position received.
    received_lsn: Lsn,
    lease_lapsed: bool,
    /// A new epoch was registered after a lapse; `stale` is expected until the
    /// barrier.
    recovering: bool,
    last_slot_check: Instant,
}

impl Stream {
    fn new(feeder: &Feeder, repl: ReplicationClient, session: Session, start: StreamStart) -> Self {
        let (session_tx, session_rx) = watch::channel(session.clone());
        let (heartbeat, beats) =
            spawn_heartbeat(feeder.knotrel.clone(), session_rx, feeder.cfg.lease / 3);
        let source_epoch = start.lineage.source_epoch();
        Self {
            knotrel: feeder.knotrel.clone(),
            control_params: feeder.cfg.pg.clone(),
            control: None,
            slot_prefix: feeder.cfg.slot_prefix.clone(),
            max_batch: feeder.cfg.max_batch,
            lease: feeder.cfg.lease,
            catch_up_idle: feeder.cfg.catch_up_idle,
            slot_check_interval: feeder.cfg.slot_check_interval,
            repl,
            session,
            session_tx,
            heartbeat,
            beats,
            generation: start.generation,
            lineage: start.lineage,
            source_id: start.source_id,
            source_epoch,
            relations: Relations::new(feeder.cfg.tables.clone()),
            applied_seq: start.applied_seq,
            next_seq: start.applied_seq + 1,
            last_pos: start.applied_pos.clone(),
            applied_pos: start.applied_pos,
            skip_until: start.skip_until,
            txn: None,
            held: None,
            pending: Vec::new(),
            pending_since: None,
            commits: VecDeque::new(),
            needs_barrier: start.needs_barrier,
            barrier_target: None,
            received_lsn: Lsn::ZERO,
            lease_lapsed: false,
            recovering: false,
            last_slot_check: Instant::now(),
        }
    }

    async fn close(self) {
        self.heartbeat.abort();
        let mut repl = self.repl;
        repl.stop();
        let _ = tokio::time::timeout(Duration::from_secs(3), repl.shutdown()).await;
    }

    fn idle(&self) -> bool {
        self.txn.is_none() && self.held.is_none() && self.pending.is_empty()
    }

    async fn run(&mut self, clock: &mut EpochClock, shutdown: &mut watch::Receiver<bool>) -> Exit {
        loop {
            let wait = if !self.pending.is_empty() {
                FLUSH_IDLE
            } else if self.needs_barrier {
                self.catch_up_idle
            } else {
                self.slot_check_interval
                    .saturating_sub(self.last_slot_check.elapsed())
                    .max(Duration::from_millis(10))
            };
            let step = tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    self.graceful_stop().await;
                    return Exit::Shutdown;
                }
                changed = self.beats.changed() => {
                    if changed.is_err() {
                        return reconnect("heartbeat task ended");
                    }
                    let beat = self.beats.borrow_and_update().clone();
                    self.on_beat(beat, clock).await
                }
                received = tokio::time::timeout(wait, self.repl.recv()) => match received {
                    Err(_) => self.on_idle(clock).await,
                    Ok(Ok(Some(event))) => self.on_event(event).await,
                    Ok(Ok(None)) => Err(reconnect("replication stream ended")),
                    Ok(Err(e)) => Err(self.on_replication_error(e.to_string()).await),
                }
            };
            if let Err(exit) = step {
                return exit;
            }
            let due = self
                .pending_since
                .is_some_and(|t| t.elapsed() >= MAX_BATCH_DELAY);
            if (self.pending.len() >= self.max_batch || due)
                && let Err(exit) = self.flush(clock).await
            {
                return exit;
            }
            if self.last_slot_check.elapsed() >= self.slot_check_interval
                && let Err(exit) = self.check_slot().await
            {
                return exit;
            }
            if let Err(exit) = self.maybe_barrier(clock, false).await {
                return exit;
            }
        }
    }

    async fn graceful_stop(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let mut clock = EpochClock::default();
            let _ = self.flush(&mut clock).await;
        })
        .await;
        tracing::info!(applied = self.applied_seq, "stopping");
    }

    async fn on_event(&mut self, event: ReplicationEvent) -> Result<(), Exit> {
        match event {
            ReplicationEvent::Begin { final_lsn, .. } => {
                if self.txn.is_some() {
                    return Err(reconnect("Begin inside an open transaction"));
                }
                self.txn = Some(Txn {
                    lsn: final_lsn,
                    index: 0,
                });
            }
            ReplicationEvent::XLogData { data, .. } => {
                let message = pgoutput::decode(&data).map_err(|e| reconnect(e.to_string()))?;
                if let Message::Relation(rel) = &message {
                    return self
                        .relations
                        .register(rel)
                        .map_err(|e| Exit::Fatal(Fatal::Schema(e.to_string())));
                }
                match self
                    .relations
                    .translate(&message)
                    .map_err(|e| Exit::Fatal(Fatal::Schema(e.to_string())))?
                {
                    None => {}
                    Some(Translation::Truncate) => {
                        return Err(rebuild("a graph table was truncated"));
                    }
                    Some(Translation::Ops(ops)) => {
                        if self.txn.is_none() {
                            return Err(reconnect("row change outside a transaction"));
                        }
                        for op in ops {
                            self.push(op);
                        }
                    }
                }
            }
            ReplicationEvent::Commit { end_lsn, .. } => {
                if self.txn.take().is_none() {
                    return Err(reconnect("Commit without Begin"));
                }
                if let Some(mut last) = self.held.take() {
                    last.position.push_str(";end");
                    self.last_pos = last.position.clone();
                    self.pending.push(last);
                    self.pending_since.get_or_insert_with(Instant::now);
                }
                self.commits.push_back((end_lsn, self.next_seq - 1));
                self.received_lsn = self.received_lsn.max(end_lsn);
                self.confirm();
            }
            ReplicationEvent::KeepAlive { wal_end, .. } => {
                self.received_lsn = self.received_lsn.max(wal_end);
                if self.idle() && self.commits.is_empty() {
                    // Nothing pending: every graph change up to wal_end is
                    // applied (rule R1).
                    self.repl.update_applied_lsn(wal_end);
                }
            }
            ReplicationEvent::Message { .. } | ReplicationEvent::StoppedAt { .. } => {}
        }
        Ok(())
    }

    /// Adds one operation of the open transaction (sections 6.2 and 8.2).
    fn push(&mut self, op: GraphOp) {
        let Some(txn) = self.txn.as_mut() else {
            return;
        };
        txn.index += 1;
        let (lsn, index) = (txn.lsn, txn.index);
        if let Some(skip) = &self.skip_until {
            if skip.stream_cmp(lsn, index) != Ordering::Less {
                return;
            }
            self.skip_until = None;
        }
        if let Some(previous) = self.held.take() {
            self.pending.push(previous);
            self.pending_since.get_or_insert_with(Instant::now);
        }
        let position = Position {
            lineage: self.lineage.clone(),
            lsn,
            index,
            end: false,
        }
        .to_string();
        let event = Event {
            sequence: self.next_seq,
            event_id: event_id(lsn, index),
            prev_position: std::mem::replace(&mut self.last_pos, position.clone()),
            position,
            op,
        };
        self.next_seq += 1;
        self.held = Some(event);
    }

    /// Confirms finished transactions that Knotrel applied (rule R1).
    fn confirm(&mut self) {
        let mut confirmed = false;
        while let Some(&(end, last)) = self.commits.front() {
            if last > self.applied_seq {
                break;
            }
            self.repl.update_applied_lsn(end);
            self.commits.pop_front();
            confirmed = true;
        }
        if confirmed {
            fault::hit("after_ack");
        }
    }

    /// Sends every pending event (section 8.1).
    async fn flush(&mut self, clock: &mut EpochClock) -> Result<(), Exit> {
        let mut attempts = 0_u32;
        while !self.pending.is_empty() {
            let n = self.pending.len().min(self.max_batch);
            fault::hit("before_events");
            let sent = self
                .knotrel
                .events(
                    &self.session,
                    self.generation,
                    &self.source_id,
                    &self.source_epoch,
                    &self.pending[..n],
                )
                .await;
            let sent = match sent {
                Ok(r) if r.ok() && fault::lose("lose_events_response") => {
                    Err(Uncertain("injected loss of the answer".into()))
                }
                other => other,
            };
            match sent {
                Ok(r) if r.ok() => {
                    let applied = r
                        .u64("applied_delivery_sequence")
                        .or_else(|| r.u64("current_applied_delivery_sequence"))
                        .unwrap_or(self.pending[n - 1].sequence);
                    let position = r
                        .str("applied_source_position")
                        .or_else(|| r.str("current_applied_source_position"))
                        .map(str::to_string);
                    self.advance(applied, position)?;
                    fault::hit("after_events_applied");
                    self.confirm();
                    attempts = 0;
                }
                Ok(r) => {
                    // A partial failure still reports the applied prefix.
                    if let Some(applied) = r.u64("applied_delivery_sequence") {
                        let position = r.str("applied_source_position").map(str::to_string);
                        self.advance(applied, position)?;
                        self.confirm();
                    }
                    self.events_failure(&r, clock).await?;
                    attempts += 1;
                }
                Err(Uncertain(e)) => {
                    tracing::warn!(error = %e, "events request outcome unknown; reconciling");
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50 * u64::from(attempts.min(40))))
                        .await;
                    self.reconcile(false, clock).await?;
                }
            }
            if attempts > 60 {
                return Err(reconnect("Knotrel keeps refusing events"));
            }
        }
        self.pending_since = None;
        Ok(())
    }

    /// Records Knotrel's frontier and drops the events it holds.
    fn advance(&mut self, applied: u64, position: Option<String>) -> Result<(), Exit> {
        let last_assigned = self.next_seq - 1;
        if applied < self.applied_seq || applied > last_assigned {
            return Err(rebuild(format!(
                "Knotrel frontier {applied} outside the expected range {}..={last_assigned}",
                self.applied_seq
            )));
        }
        if let (Some(p), Some(e)) = (
            &position,
            self.pending.iter().find(|e| e.sequence == applied),
        ) && p != &e.position
        {
            return Err(rebuild(format!(
                "Knotrel position at sequence {applied} differs from the feeder's"
            )));
        }
        self.applied_seq = applied;
        if let Some(p) = position {
            self.applied_pos = p;
        }
        self.pending.retain(|e| e.sequence > applied);
        if self.pending.is_empty() {
            self.pending_since = None;
        }
        Ok(())
    }

    async fn events_failure(&mut self, r: &Reply, clock: &mut EpochClock) -> Result<(), Exit> {
        let code = if r.is("event_failed") || r.str("result") == Some("partial_failure") {
            r.body
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        } else {
            r.code.clone().unwrap_or_default()
        };
        tracing::warn!(answer = %r.describe(), %code, "events refused");
        if let Some(e) = common_failure(r) {
            if matches!(e, Exit::Restart(Restart::Reconnect(_))) {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Ok(());
            }
            return Err(e);
        }
        match code.as_str() {
            "lease_expired" => {
                self.lease_lapsed = true;
                self.reconcile(true, clock).await
            }
            "fenced_feeder_session" => self.on_fenced(clock).await,
            "limit_exceeded" => Err(Exit::Fatal(Fatal::Capacity(format!(
                "Knotrel graph limit reached (KNOTREL_MAX_NODES / KNOTREL_MAX_EDGES): {}",
                r.describe()
            )))),
            "unknown_node" | "unknown_edge" | "self_loop" => {
                let reason = format!("Knotrel diverged from the source: {}", r.describe());
                declare_gap(
                    &self.knotrel,
                    &self.session,
                    self.generation,
                    self.applied_seq,
                    &reason,
                )
                .await?;
                Err(rebuild(reason))
            }
            "out_of_order_sequence"
            | "source_predecessor_mismatch"
            | "conflicting_duplicate"
            | "unverifiable_duplicate"
            | "graph_desynchronized"
            | "source_epoch_mismatch"
            | "source_mismatch"
            | "stale_generation" => {
                Err(rebuild(format!("Knotrel refused events: {}", r.describe())))
            }
            _ => Err(Exit::Fatal(Fatal::Other(format!(
                "Knotrel refused events: {}",
                r.describe()
            )))),
        }
    }

    /// Re-reads Knotrel's frontier, with a new epoch if `new_epoch`.
    async fn reconcile(&mut self, new_epoch: bool, clock: &mut EpochClock) -> Result<(), Exit> {
        let mut session = self.session.clone();
        if new_epoch {
            session.epoch = clock.next();
        }
        let mut tries = 0;
        let r = loop {
            match self
                .knotrel
                .session(&session, self.lease.as_millis() as u64)
                .await
            {
                Ok(r) => break r,
                Err(e) if tries < 20 => {
                    tries += 1;
                    tracing::debug!(error = %e, "session registration failed; retrying");
                    tokio::time::sleep(Duration::from_millis(100 * tries)).await;
                }
                Err(e) => return Err(reconnect(e.to_string())),
            }
        };
        if !r.ok() {
            if r.is("fenced_feeder_session") {
                return Err(Exit::Fatal(Fatal::Fenced(
                    "a feeder with a higher epoch owns the Knotrel server".into(),
                )));
            }
            return Err(common_failure(&r)
                .unwrap_or_else(|| reconnect(format!("session registration: {}", r.describe()))));
        }
        let frontier = Frontier::from_reply(&r)?;
        if frontier.generation != self.generation {
            return Err(rebuild(format!(
                "Knotrel generation changed from {} to {}",
                self.generation, frontier.generation
            )));
        }
        if new_epoch {
            self.session = session;
            let _ = self.session_tx.send(self.session.clone());
            self.lease_lapsed = false;
            if frontier.status != "ready" {
                self.needs_barrier = true;
                self.barrier_target = None;
                self.recovering = true;
            }
            tracing::info!(epoch = self.session.epoch, status = %frontier.status, "registered a new epoch");
        }
        self.advance(frontier.applied_seq, frontier.applied_pos)?;
        self.confirm();
        Ok(())
    }

    /// `fenced_feeder_session`: after a lease lapse it is recovery fencing
    /// (register a new epoch); otherwise another feeder took over (stop).
    async fn on_fenced(&mut self, clock: &mut EpochClock) -> Result<(), Exit> {
        if self.lease_lapsed {
            self.reconcile(true, clock).await
        } else {
            Err(Exit::Fatal(Fatal::Fenced(
                "another feeder took over the Knotrel server".into(),
            )))
        }
    }

    async fn on_beat(&mut self, beat: Beat, clock: &mut EpochClock) -> Result<(), Exit> {
        match beat {
            Beat::Status(status) => match status.as_str() {
                // After a new epoch, `stale` lasts until the barrier.
                "stale" if self.recovering => Ok(()),
                "stale" => {
                    tracing::warn!("Knotrel reports the lease lapsed; registering a new epoch");
                    self.lease_lapsed = true;
                    self.reconcile(true, clock).await
                }
                "desynchronized" => Err(rebuild("Knotrel is desynchronized")),
                "uninitialized" | "loading" => Err(rebuild(format!("Knotrel status is {status}"))),
                "catching_up" if !self.needs_barrier => {
                    self.needs_barrier = true;
                    self.barrier_target = None;
                    Ok(())
                }
                _ => Ok(()),
            },
            Beat::InstanceChanged => Err(Exit::Restart(Restart::KnotrelRestarted)),
            Beat::LeaseExpired => {
                self.lease_lapsed = true;
                self.reconcile(true, clock).await
            }
            Beat::Fenced => self.on_fenced(clock).await,
            Beat::Unreachable => Ok(()),
        }
    }

    /// The stream was idle for the chosen wait.
    async fn on_idle(&mut self, clock: &mut EpochClock) -> Result<(), Exit> {
        if !self.pending.is_empty() {
            return self.flush(clock).await;
        }
        self.maybe_barrier(clock, true).await
    }

    /// Declares the barrier when caught up (section 8.9): no open
    /// transaction, every received event applied, and either the stream was
    /// idle for the catch-up interval or the feeder received a WAL position
    /// at or past the target sampled when the barrier became necessary. The
    /// second condition is reached under continuous writes too.
    async fn maybe_barrier(
        &mut self,
        clock: &mut EpochClock,
        idle_elapsed: bool,
    ) -> Result<(), Exit> {
        if !self.needs_barrier || !self.idle() {
            return Ok(());
        }
        if self.barrier_target.is_none() {
            match self.current_wal_lsn().await {
                Some(lsn) => self.barrier_target = Some(lsn),
                None if !idle_elapsed => return Ok(()),
                None => {}
            }
        }
        let reached = self.barrier_target.is_some_and(|t| self.received_lsn >= t);
        if idle_elapsed || reached {
            return self.barrier(clock).await;
        }
        Ok(())
    }

    async fn current_wal_lsn(&mut self) -> Option<Lsn> {
        if self.control.as_ref().is_none_or(Control::is_closed) {
            self.control = Control::connect(&self.control_params).await.ok();
        }
        self.control.as_ref()?.current_wal_lsn().await.ok()
    }

    /// Declares a barrier at Knotrel's frontier once caught up (section 8.9).
    async fn barrier(&mut self, clock: &mut EpochClock) -> Result<(), Exit> {
        let r = self
            .knotrel
            .barrier(
                &self.session,
                self.generation,
                self.applied_seq,
                &self.applied_pos,
            )
            .await
            .map_err(|e| reconnect(e.to_string()))?;
        if r.ok() {
            self.needs_barrier = false;
            self.barrier_target = None;
            self.recovering = false;
            tracing::info!(
                applied = self.applied_seq,
                position = %self.applied_pos,
                "caught up: barrier accepted, Knotrel is ready"
            );
            return Ok(());
        }
        if let Some(e) = common_failure(&r) {
            return Err(e);
        }
        match r.code.as_deref() {
            Some("fenced_feeder_session") => self.on_fenced(clock).await,
            Some("lease_expired") => {
                self.lease_lapsed = true;
                self.reconcile(true, clock).await
            }
            Some("barrier_not_reached" | "superseded_barrier" | "barrier_position_mismatch") => {
                self.reconcile(false, clock).await
            }
            Some("graph_desynchronized") => Err(rebuild("Knotrel is desynchronized")),
            _ => Err(Exit::Fatal(Fatal::Other(format!(
                "barrier refused: {}",
                r.describe()
            )))),
        }
    }

    /// Periodic slot check (section 9).
    async fn check_slot(&mut self) -> Result<(), Exit> {
        self.last_slot_check = Instant::now();
        let control = match Control::connect(&self.control_params).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "slot check skipped");
                return Ok(());
            }
        };
        match control.slot(&self.lineage.slot).await {
            Ok(Some(slot)) => {
                if !slot.usable() {
                    return Err(self
                        .history_lost(format!("slot {} was invalidated", slot.name))
                        .await);
                }
                if slot.wal_status.as_deref() == Some("unreserved") {
                    tracing::warn!(slot = %slot.name, retained_bytes = ?slot.retained_bytes, "slot WAL is unreserved: it will be invalidated if the feeder falls further behind");
                } else {
                    tracing::info!(slot = %slot.name, retained_bytes = ?slot.retained_bytes, wal_status = ?slot.wal_status, "slot status");
                }
            }
            Ok(None) => {
                return Err(self
                    .history_lost(format!("slot {} no longer exists", self.lineage.slot))
                    .await);
            }
            Err(e) => tracing::warn!(error = %e, "slot check failed"),
        }
        if let Err(e) = control
            .drop_own_slots(&self.slot_prefix, Some(&self.lineage.slot))
            .await
        {
            tracing::warn!(error = %e, "slot cleanup failed");
        }
        Ok(())
    }

    async fn on_replication_error(&mut self, error: String) -> Exit {
        tracing::warn!(%error, "replication stream failed");
        let Ok(control) = Control::connect(&self.control_params).await else {
            return reconnect(error);
        };
        match control.slot(&self.lineage.slot).await {
            Ok(None) => {
                self.history_lost(format!("slot {} was dropped", self.lineage.slot))
                    .await
            }
            Ok(Some(slot)) if !slot.usable() => {
                self.history_lost(format!("slot {} was invalidated: {error}", slot.name))
                    .await
            }
            _ => reconnect(error),
        }
    }

    /// Source history after Knotrel's frontier is gone (section 8.8).
    async fn history_lost(&mut self, reason: String) -> Exit {
        match declare_gap(
            &self.knotrel,
            &self.session,
            self.generation,
            self.applied_seq,
            &reason,
        )
        .await
        {
            Ok(()) => rebuild(reason),
            Err(e) => e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_strictly_increase() {
        let mut c = EpochClock::default();
        let a = c.next();
        let b = c.next();
        let d = c.next();
        assert!(a < b && b < d);
    }

    #[test]
    fn exit_codes_are_distinct() {
        let codes: Vec<i32> = [
            Fatal::Other(String::new()),
            Fatal::Config(String::new()),
            Fatal::Schema(String::new()),
            Fatal::Capacity(String::new()),
            Fatal::Fenced(String::new()),
        ]
        .iter()
        .map(Fatal::exit_code)
        .collect();
        assert_eq!(codes, vec![1, 2, 3, 4, 5]);
    }
}
