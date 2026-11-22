//! Randomized feeder/restart soak against a reference model (v1.0 readiness
//! criterion 3, see `docs/releases/v1.0-readiness.md` and `docs/releases/v1.0-criterion-3-evidence.md`).
//!
//! The soak drives real `knotrel-server` child processes through random
//! interleavings of feeder sessions, imports (create-only and replacement),
//! ordered feeder events (including duplicates, gaps, evicted and conflicting
//! duplicates, predecessor mismatches, stale generations, fenced sessions,
//! source lineage changes and domain failures), barriers, gap declarations,
//! lease expiry, legacy `/v1/operations` and `/v1/batch` requests, lifecycle
//! operations (`KNOTREL_TOPOLOGY_LIFECYCLE`), queries, process kills
//! (`SIGKILL` on Unix) and rebuilds from an authoritative source. After
//! **every** step it compares the server with a small, independent reference
//! model: the response of the step, `GET /v1/stats`, `GET /ready`, the full
//! registered topology through the inspection routes, and the connectivity of
//! every pair of a fixed node universe in one query batch.
//!
//! Every decision comes from one seeded pseudo-random generator, and requests
//! are strictly sequential, so a run is reproducible from its printed seed.
//! Wall-clock time only bounds the run length and the deliberate lease
//! expiry; it never changes which requests are sent.
//!
//! Two variants:
//!
//! - **Short** (`soak_short_*`): fixed seeds and a bounded number of steps;
//!   runs in the normal `cargo test` and in CI.
//! - **Long** (`soak_long`, ignored by default), configured by environment:
//!
//!   ```text
//!   KNOTREL_SOAK_SEED=0x5eed       # default: derived from the clock, printed
//!   KNOTREL_SOAK_STEPS=200000      # default 50000
//!   KNOTREL_SOAK_SECONDS=1800      # default 600; whichever bound comes first
//!   KNOTREL_SOAK_LIFECYCLE=on|off  # default: chosen by the seed
//!   KNOTREL_SOAK_LEASE_EXPIRIES=8  # default 4; each costs about 1.2 s
//!   KNOTREL_SOAK_TRACE=1           # print every step
//!   cargo test -p knotrel-server --test soak -- --ignored --nocapture
//!   ```
//!
//! A failure prints the seed, the step number, the server configuration, the
//! last steps, the expected and actual responses and the server's stderr.
//! Re-running the same seed with `KNOTREL_SOAK_STEPS` set to the failing step
//! reproduces it.
//!
//! What the soak deliberately does not cover (covered by deterministic tests
//! instead): concurrent requests, timeouts and lost responses (`events.rs`
//! `test_11`, `test_25`, `test_27`; `e2e_rebuild.rs`), worker panics, admission
//! limits and malformed envelopes.

mod common;

use common::feeder::{ExternalFeederClient, HttpResponse};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// Fixed universe and limits
// ---------------------------------------------------------------------------

/// Node IDs used by the source. They include `0`, IDs above 2^53 (not exact as
/// a JavaScript number) and the end of the u64 range.
const UNIVERSE: [u64; 12] = [
    0,
    1,
    2,
    3,
    5,
    8,
    13,
    21,
    1 << 53,
    (1 << 53) + 1,
    u64::MAX - 1,
    u64::MAX,
];

/// A node ID outside [`UNIVERSE`], used for unknown-node probes and for
/// node-limit faults.
const OUTSIDE: u64 = 4;

/// `KNOTREL_MAX_NODES`: equal to the universe, so the source never exceeds it
/// and an extra node is a deliberate fault.
const MAX_NODES: usize = UNIVERSE.len();

/// `KNOTREL_MAX_EDGES`: small enough that the source reaches it regularly.
const MAX_EDGES: usize = 10;

/// Lease used for normal registrations (the server's maximum).
const LONG_LEASE_MS: u64 = 60_000;

/// Lease used for a deliberate expiry (the server's minimum).
const SHORT_LEASE_MS: u64 = 1_000;

/// A renewal is forced after this many steps without one, so that the long
/// lease cannot expire by accident while steps take milliseconds.
const RENEW_EVERY_STEPS: u32 = 100;

/// Wall-clock guard: if this much time passes without a renewal the
/// environment is too slow for the lease model and the run stops.
const RENEW_GUARD: Duration = Duration::from_secs(45);

/// Number of recent steps printed in a failure report.
const RECENT_STEPS: usize = 40;

/// Identifier of the authoritative source.
const SOURCE_ID: &str = "soak-source";

// ---------------------------------------------------------------------------
// Deterministic pseudo-random generator
// ---------------------------------------------------------------------------

/// SplitMix64 (G. L. Steele, D. Lea, C. H. Flood, "Fast splittable
/// pseudorandom number generators", OOPSLA 2014). Platform independent, so a
/// seed reproduces the same run everywhere. `below` uses a modulo reduction;
/// its bias is irrelevant for test generation.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish index in `0..n`; `n` must be positive.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn node(&mut self) -> u64 {
        self.pick(&UNIVERSE)
    }

    /// Two distinct universe nodes.
    fn pair(&mut self) -> (u64, u64) {
        let u = self.node();
        loop {
            let v = self.node();
            if v != u {
                return (u, v);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reference topology
// ---------------------------------------------------------------------------

/// Graph-domain errors of the reference model, with their wire codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DomainError {
    SelfLoop,
    UnknownNode,
    UnknownEdge,
    NodeLimit,
    EdgeLimit,
}

impl DomainError {
    fn code(self) -> &'static str {
        match self {
            Self::SelfLoop => "self_loop",
            Self::UnknownNode => "unknown_node",
            Self::UnknownEdge => "unknown_edge",
            Self::NodeLimit => "node_limit_exceeded",
            Self::EdgeLimit => "edge_limit_exceeded",
        }
    }

    /// HTTP status of a direct operation failing with this error.
    fn direct_status(self) -> u16 {
        match self {
            Self::UnknownNode | Self::UnknownEdge => 404,
            Self::SelfLoop | Self::NodeLimit | Self::EdgeLimit => 422,
        }
    }
}

/// Normalized undirected edge key.
fn key(u: u64, v: u64) -> (u64, u64) {
    (u.min(v), u.max(v))
}

/// Registered topology with lifecycle flags, written from `docs/http-api.md`
/// ("Single operations", "Node and edge lifecycle") and the limit rules of
/// `knotrel-core`, not from the server code. Without lifecycle support every
/// flag stays `true`.
///
/// Every operation is `O(V + E)` at worst (`connected` is a breadth-first
/// search over the effective graph); the universe is tiny, so this is cheap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Topology {
    /// Registered nodes and their `active` flag.
    nodes: BTreeMap<u64, bool>,
    /// Registered edges (normalized) and their `enabled` flag.
    edges: BTreeMap<(u64, u64), bool>,
}

impl Topology {
    fn add_node(&mut self, n: u64) -> Result<bool, DomainError> {
        if self.nodes.contains_key(&n) {
            return Ok(false);
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(DomainError::NodeLimit);
        }
        self.nodes.insert(n, true);
        Ok(true)
    }

    /// Self-loop first; an existing edge is a no-op even at capacity; node
    /// limits take precedence over edge limits; nothing changes on error.
    fn link(&mut self, u: u64, v: u64) -> Result<bool, DomainError> {
        if u == v {
            return Err(DomainError::SelfLoop);
        }
        if self.edges.contains_key(&key(u, v)) {
            return Ok(false);
        }
        let new_nodes = [u, v]
            .iter()
            .filter(|n| !self.nodes.contains_key(n))
            .count();
        if self.nodes.len() + new_nodes > MAX_NODES {
            return Err(DomainError::NodeLimit);
        }
        if self.edges.len() >= MAX_EDGES {
            return Err(DomainError::EdgeLimit);
        }
        self.nodes.entry(u).or_insert(true);
        self.nodes.entry(v).or_insert(true);
        self.edges.insert(key(u, v), true);
        Ok(true)
    }

    fn cut(&mut self, u: u64, v: u64) -> Result<bool, DomainError> {
        if u == v {
            return Err(DomainError::SelfLoop);
        }
        Ok(self.edges.remove(&key(u, v)).is_some())
    }

    fn set_node_active(&mut self, n: u64, active: bool) -> Result<bool, DomainError> {
        let flag = self.nodes.get_mut(&n).ok_or(DomainError::UnknownNode)?;
        let changed = *flag != active;
        *flag = active;
        Ok(changed)
    }

    fn set_edge_enabled(&mut self, u: u64, v: u64, enabled: bool) -> Result<bool, DomainError> {
        if u == v {
            return Err(DomainError::SelfLoop);
        }
        if !self.nodes.contains_key(&u) || !self.nodes.contains_key(&v) {
            return Err(DomainError::UnknownNode);
        }
        let flag = self
            .edges
            .get_mut(&key(u, v))
            .ok_or(DomainError::UnknownEdge)?;
        let changed = *flag != enabled;
        *flag = enabled;
        Ok(changed)
    }

    fn remove_node(&mut self, n: u64) -> bool {
        if self.nodes.remove(&n).is_none() {
            return false;
        }
        self.edges.retain(|&(a, b), _| a != n && b != n);
        true
    }

    fn active(&self, n: u64) -> bool {
        self.nodes.get(&n).copied().unwrap_or(false)
    }

    fn effective(&self, (a, b): (u64, u64)) -> bool {
        self.edges.get(&(a, b)).copied().unwrap_or(false) && self.active(a) && self.active(b)
    }

    /// Breadth-first search over the effective graph.
    fn connected(&self, u: u64, v: u64) -> Result<bool, DomainError> {
        if !self.nodes.contains_key(&u) || !self.nodes.contains_key(&v) {
            return Err(DomainError::UnknownNode);
        }
        if !self.active(u) || !self.active(v) {
            return Ok(false);
        }
        let mut seen = BTreeSet::from([u]);
        let mut queue = VecDeque::from([u]);
        while let Some(x) = queue.pop_front() {
            if x == v {
                return Ok(true);
            }
            for &(a, b) in self.edges.keys() {
                if !self.effective((a, b)) {
                    continue;
                }
                let next = if a == x {
                    b
                } else if b == x {
                    a
                } else {
                    continue;
                };
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        Ok(false)
    }

    fn degree(&self, n: u64) -> usize {
        self.edges
            .keys()
            .filter(|&&(a, b)| a == n || b == n)
            .count()
    }

    fn apply(&mut self, op: &Op) -> Result<bool, DomainError> {
        match *op {
            Op::AddNode(n) => self.add_node(n),
            Op::Link(u, v) => self.link(u, v),
            Op::Cut(u, v) => self.cut(u, v),
            Op::Progress => Ok(false),
            Op::SetNodeActive(n, a) => self.set_node_active(n, a),
            Op::SetEdgeEnabled(u, v, e) => self.set_edge_enabled(u, v, e),
            Op::RemoveNode(n) => Ok(self.remove_node(n)),
        }
    }
}

// ---------------------------------------------------------------------------
// Operations and wire events
// ---------------------------------------------------------------------------

/// A graph mutation or progress marker, as an event or a direct operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    AddNode(u64),
    Link(u64, u64),
    Cut(u64, u64),
    Progress,
    SetNodeActive(u64, bool),
    SetEdgeEnabled(u64, u64, bool),
    RemoveNode(u64),
}

impl Op {
    fn is_lifecycle(self) -> bool {
        matches!(
            self,
            Self::SetNodeActive(..) | Self::SetEdgeEnabled(..) | Self::RemoveNode(_)
        )
    }

    /// The deduplication fingerprint: operation, normalized endpoints, flag.
    fn canonical(self) -> Self {
        match self {
            Self::Link(u, v) => Self::Link(u.min(v), u.max(v)),
            Self::Cut(u, v) => Self::Cut(u.min(v), u.max(v)),
            Self::SetEdgeEnabled(u, v, e) => Self::SetEdgeEnabled(u.min(v), u.max(v), e),
            other => other,
        }
    }

    fn json(self) -> Value {
        let s = |n: u64| Value::String(n.to_string());
        match self {
            Self::AddNode(n) => json!({"op": "add_node", "node": s(n)}),
            Self::Link(u, v) => json!({"op": "link", "source": s(u), "target": s(v)}),
            Self::Cut(u, v) => json!({"op": "cut", "source": s(u), "target": s(v)}),
            Self::Progress => json!({"op": "progress"}),
            Self::SetNodeActive(n, a) => {
                json!({"op": "set_node_active", "node": s(n), "active": a})
            }
            Self::SetEdgeEnabled(u, v, e) => {
                json!({"op": "set_edge_enabled", "source": s(u), "target": s(v), "enabled": e})
            }
            Self::RemoveNode(n) => json!({"op": "remove_node", "node": s(n)}),
        }
    }
}

/// One feeder event as sent on the wire.
#[derive(Clone, Debug)]
struct WireEvent {
    event_id: String,
    seq: u64,
    prev_seq: u64,
    pos: String,
    prev_pos: String,
    op: Op,
}

impl WireEvent {
    fn json(&self) -> Value {
        let mut value = self.op.json();
        let obj = value.as_object_mut().expect("operation object");
        obj.insert("event_id".into(), json!(self.event_id));
        obj.insert("delivery_sequence".into(), json!(self.seq));
        obj.insert("prev_delivery_sequence".into(), json!(self.prev_seq));
        obj.insert("source_position".into(), json!(self.pos));
        obj.insert("prev_source_position".into(), json!(self.prev_pos));
        value
    }

    fn short(&self) -> String {
        format!("{}@{}:{:?}", self.event_id, self.seq, self.op)
    }
}

/// Source position of source change `k` (`p0` is the empty source).
fn position(k: u64) -> String {
    format!("p{k}")
}

// ---------------------------------------------------------------------------
// Authoritative source
// ---------------------------------------------------------------------------

/// The authoritative store: an append-only log of valid changes. Change `k`
/// (1-based) has position `p{k}` and event ID `ev{k}`. The source only emits
/// changes that are valid for it, so replaying its log never fails.
struct Source {
    log: Vec<Op>,
    head_topology: Topology,
    /// Snapshots of the most recent positions, for imports behind the head.
    recent: VecDeque<(u64, Topology)>,
    /// Source epoch counter; a reset changes the epoch string.
    epoch: u64,
}

impl Source {
    fn new() -> Self {
        Self {
            log: Vec::new(),
            head_topology: Topology::default(),
            recent: VecDeque::from([(0, Topology::default())]),
            epoch: 1,
        }
    }

    fn head(&self) -> u64 {
        self.log.len() as u64
    }

    fn epoch_string(&self) -> String {
        format!("epoch-{}", self.epoch)
    }

    fn snapshot(&self, k: u64) -> Option<&Topology> {
        self.recent.iter().find(|(p, _)| *p == k).map(|(_, t)| t)
    }

    /// Wire event for change `k` delivered in a generation whose baseline is
    /// change `base`.
    fn event(&self, k: u64, base: u64) -> WireEvent {
        WireEvent {
            event_id: format!("ev{k}"),
            seq: k - base,
            prev_seq: k - base - 1,
            pos: position(k),
            prev_pos: position(k - 1),
            op: self.log[(k - 1) as usize],
        }
    }

    /// Appends one random change that is valid for the source.
    fn advance(&mut self, rng: &mut Rng, lifecycle: bool) {
        let t = &self.head_topology;
        let roll = rng.below(if lifecycle { 100 } else { 76 });
        let op = match roll {
            0..=14 => Op::AddNode(rng.node()),
            15..=49 => {
                let (u, v) = rng.pair();
                if !t.edges.contains_key(&key(u, v)) && t.edges.len() >= MAX_EDGES {
                    let edges: Vec<_> = t.edges.keys().copied().collect();
                    let (a, b) = rng.pick(&edges);
                    Op::Cut(b, a)
                } else {
                    Op::Link(u, v)
                }
            }
            50..=67 => {
                if !t.edges.is_empty() && rng.chance(70) {
                    let edges: Vec<_> = t.edges.keys().copied().collect();
                    let (a, b) = rng.pick(&edges);
                    if rng.chance(50) {
                        Op::Cut(a, b)
                    } else {
                        Op::Cut(b, a)
                    }
                } else {
                    let (u, v) = rng.pair();
                    Op::Cut(u, v)
                }
            }
            68..=75 => Op::Progress,
            76..=86 => {
                let nodes: Vec<_> = t.nodes.keys().copied().collect();
                if nodes.is_empty() {
                    Op::Progress
                } else {
                    Op::SetNodeActive(rng.pick(&nodes), rng.chance(50))
                }
            }
            87..=96 => {
                let edges: Vec<_> = t.edges.keys().copied().collect();
                if edges.is_empty() {
                    Op::Progress
                } else {
                    let (a, b) = rng.pick(&edges);
                    let (u, v) = if rng.chance(50) { (a, b) } else { (b, a) };
                    Op::SetEdgeEnabled(u, v, rng.chance(50))
                }
            }
            _ => Op::RemoveNode(rng.node()),
        };
        self.head_topology
            .apply(&op)
            .expect("the source only emits changes that are valid for it");
        self.log.push(op);
        self.recent
            .push_back((self.head(), self.head_topology.clone()));
        if self.recent.len() > 32 {
            self.recent.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// Expected responses
// ---------------------------------------------------------------------------

/// Expected HTTP status and JSON fields (by JSON pointer) of one response.
#[derive(Debug, Clone)]
struct Expect {
    status: u16,
    fields: Vec<(&'static str, Value)>,
}

impl Expect {
    fn new(status: u16) -> Self {
        Self {
            status,
            fields: Vec::new(),
        }
    }

    fn with(mut self, pointer: &'static str, value: Value) -> Self {
        self.fields.push((pointer, value));
        self
    }

    /// A plain error envelope.
    fn error(status: u16, code: &str) -> Self {
        Self::new(status)
            .with("/result", json!("error"))
            .with("/code", json!(code))
    }

    /// Returns a description of every difference from `actual`.
    fn mismatches(&self, actual: &HttpResponse) -> Vec<String> {
        let mut out = Vec::new();
        if actual.status != self.status {
            out.push(format!(
                "status: expected {}, got {}",
                self.status, actual.status
            ));
        }
        if self.fields.is_empty() {
            return out;
        }
        let Ok(body) = serde_json::from_str::<Value>(&actual.body) else {
            out.push("body is not JSON".into());
            return out;
        };
        for (pointer, value) in &self.fields {
            let got = body.pointer(pointer);
            if got != Some(value) {
                out.push(format!("{pointer}: expected {value}, got {got:?}"));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Reference server model
// ---------------------------------------------------------------------------

/// Synchronization status of a source-managed server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sync {
    Uninitialized,
    CatchingUp,
    Ready,
    Stale,
    Desynchronized,
}

impl Sync {
    fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::CatchingUp => "catching_up",
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Desynchronized => "desynchronized",
        }
    }
}

/// A retained deduplication-window entry.
#[derive(Clone, Debug)]
struct Entry {
    seq: u64,
    event_id: String,
    pos: String,
    canonical: Op,
    changed: bool,
}

/// One feeder request's identities.
#[derive(Clone, Debug)]
struct Envelope {
    generation: u64,
    session: String,
    epoch: u64,
    source_id: String,
    source_epoch: String,
}

/// Model of a source-managed server, written from `docs/integration-contract.md`
/// §§3–9 and `docs/http-api.md` ("Ordered incremental event stream",
/// "Synchronization barrier verification"). Lease time is modelled by an
/// explicit `lease_expired` flag set only after a deliberate expiry.
#[derive(Clone, Debug)]
struct Feed {
    generation: u64,
    version: u64,
    topology: Topology,
    status: Sync,
    applied: u64,
    position: Option<String>,
    source_id: Option<String>,
    source_epoch: Option<String>,
    window: VecDeque<Entry>,
    window_capacity: usize,
    session: Option<String>,
    feeder_epoch: u64,
    lease_expired: bool,
    /// The registered lease is the short one (a heartbeat would renew it for
    /// only one second, which the model cannot follow deterministically).
    short_lease: bool,
    last_expired_epoch: Option<u64>,
}

impl Feed {
    fn new(window_capacity: usize) -> Self {
        Self {
            generation: 0,
            version: 0,
            topology: Topology::default(),
            status: Sync::Uninitialized,
            applied: 0,
            position: None,
            source_id: None,
            source_epoch: None,
            window: VecDeque::new(),
            window_capacity,
            session: None,
            feeder_epoch: 0,
            lease_expired: false,
            short_lease: false,
            last_expired_epoch: None,
        }
    }

    /// Lease state as the server sees it: no lease before the first
    /// registration counts as expired.
    fn lease_gone(&self) -> bool {
        self.session.is_none() || self.lease_expired
    }

    /// The status reported by `GET /v1/stats` (an expired lease shows as
    /// `stale` before any request records it).
    fn effective_status(&self) -> Sync {
        if matches!(self.status, Sync::Ready | Sync::CatchingUp) && self.lease_gone() {
            Sync::Stale
        } else {
            self.status
        }
    }

    /// `GET /ready` body status and HTTP status, in the documented precedence.
    fn readiness(&self) -> (u16, &'static str) {
        if self.generation == 0 {
            return (503, "uninitialized");
        }
        match self.effective_status() {
            Sync::Ready => (200, "ready"),
            other => (503, other.as_str()),
        }
    }

    /// Records an observed lease expiry (`check_lease_expiry` in the contract's
    /// lease rules): `ready`/`catching_up` become `stale` and the epoch is fenced
    /// for synchronization proof.
    fn note_expiry(&mut self) {
        if matches!(self.status, Sync::Ready | Sync::CatchingUp) {
            self.status = Sync::Stale;
        }
        self.last_expired_epoch = Some(self.feeder_epoch);
    }

    fn desynchronize(&mut self) {
        self.status = Sync::Desynchronized;
    }

    /// Session and lease validation shared by events, barrier, gap and import.
    fn validate_session(&mut self, session: &str, epoch: u64) -> Result<(), Expect> {
        match &self.session {
            None => return Err(Expect::error(409, "session_required")),
            Some(active) if active != session || epoch != self.feeder_epoch => {
                return Err(Expect::error(409, "fenced_feeder_session"));
            }
            Some(_) => {}
        }
        if self.lease_expired {
            self.note_expiry();
            return Err(Expect::error(409, "lease_expired"));
        }
        Ok(())
    }

    fn register(&mut self, session: &str, epoch: u64, lease_ms: u64) -> Expect {
        let was_expired = self.lease_gone();
        if was_expired {
            self.note_expiry();
        }
        match self.session.clone() {
            Some(active) if active == session => {
                if epoch < self.feeder_epoch {
                    return Expect::error(409, "fenced_feeder_session");
                }
                if (epoch > self.feeder_epoch || was_expired) && self.status == Sync::Ready {
                    self.status = Sync::Stale;
                }
            }
            Some(_) => {
                if epoch <= self.feeder_epoch {
                    return Expect::error(409, "fenced_feeder_session");
                }
                if self.status == Sync::Ready {
                    self.status = Sync::Stale;
                }
            }
            None => {}
        }
        self.session = Some(session.to_string());
        self.feeder_epoch = epoch;
        self.lease_expired = false;
        self.short_lease = lease_ms < LONG_LEASE_MS;
        Expect::new(200)
            .with("/result", json!("session_acquired"))
            .with("/generation", json!(self.generation))
            .with("/applied_delivery_sequence", json!(self.applied))
            .with("/status", json!(self.status.as_str()))
    }

    fn heartbeat(&mut self, session: &str, epoch: u64) -> Expect {
        match &self.session {
            None => return Expect::error(409, "session_required"),
            Some(active) if active != session || epoch != self.feeder_epoch => {
                return Expect::error(409, "fenced_feeder_session");
            }
            Some(_) => {}
        }
        if self.lease_expired {
            self.note_expiry();
        }
        self.lease_expired = false;
        Expect::new(200)
            .with("/result", json!("heartbeat_acknowledged"))
            .with("/status", json!(self.status.as_str()))
    }

    fn import(
        &mut self,
        create_only: bool,
        if_match: Option<&str>,
        envelope: &Envelope,
        position: &str,
        snapshot: &Topology,
    ) -> Expect {
        if let Err(e) = self.validate_session(&envelope.session, envelope.epoch) {
            return e;
        }
        if create_only {
            if self.generation != 0 {
                return Expect::error(409, "graph_already_initialized");
            }
        } else {
            if self.generation == 0 {
                return Expect::error(412, "precondition_failed");
            }
            if if_match != Some(self.etag().as_str()) {
                return Expect::error(412, "precondition_failed");
            }
        }
        self.generation += 1;
        self.version = 0;
        self.topology = snapshot.clone();
        self.source_id = Some(envelope.source_id.clone());
        self.source_epoch = Some(envelope.source_epoch.clone());
        self.position = Some(position.to_string());
        self.applied = 0;
        self.status = Sync::CatchingUp;
        self.window.clear();
        self.last_expired_epoch = None;
        Expect::new(201)
            .with("/generation", json!(self.generation))
            .with("/state_version", json!("0"))
    }

    /// `"<generation>.<state_version>.<position>"`; positions here never need
    /// percent-encoding.
    fn etag(&self) -> String {
        format!(
            "\"{}.{}.{}\"",
            self.generation,
            self.version,
            self.position.as_deref().unwrap_or("")
        )
    }

    fn barrier(&mut self, envelope: &Envelope, seq: u64, pos: &str) -> Expect {
        if let Err(e) = self.validate_session(&envelope.session, envelope.epoch) {
            return e;
        }
        if envelope.generation != self.generation {
            return Expect::error(409, "stale_generation");
        }
        if self.generation == 0 || self.status == Sync::Uninitialized {
            return Expect::error(409, "graph_uninitialized");
        }
        if self.status == Sync::Desynchronized {
            return Expect::error(409, "graph_desynchronized");
        }
        if seq > self.applied {
            return Expect::error(409, "barrier_not_reached");
        }
        if seq < self.applied {
            return Expect::error(409, "superseded_barrier");
        }
        if Some(pos) != self.position.as_deref() {
            return Expect::error(409, "barrier_position_mismatch");
        }
        if self
            .last_expired_epoch
            .is_some_and(|expired| envelope.epoch <= expired)
        {
            return Expect::error(409, "fenced_feeder_session");
        }
        let result = if self.status == Sync::Ready {
            "already_synchronized"
        } else {
            self.status = Sync::Ready;
            "synchronized"
        };
        Expect::new(200)
            .with("/result", json!(result))
            .with("/status", json!("ready"))
    }

    fn gap(&mut self, envelope: &Envelope) -> Expect {
        if let Err(e) = self.validate_session(&envelope.session, envelope.epoch) {
            return e;
        }
        if self.generation == 0 {
            return Expect::error(409, "graph_uninitialized");
        }
        if envelope.generation != self.generation {
            return Expect::error(409, "stale_generation");
        }
        self.desynchronize();
        Expect::new(200)
            .with("/result", json!("gap_acknowledged"))
            .with("/status", json!("desynchronized"))
    }

    /// `POST /v1/feeder/events`, following the failure matrix of
    /// `docs/integration-contract.md` §9.3 and the invariants of
    /// `docs/http-api.md` ("Invariants and validation").
    fn events(&mut self, envelope: &Envelope, events: &[WireEvent], lifecycle: bool) -> Expect {
        if let Err(e) = self.validate_session(&envelope.session, envelope.epoch) {
            return e;
        }
        if self.generation == 0 {
            return Expect::error(409, "graph_uninitialized");
        }
        if envelope.generation != self.generation {
            return Expect::error(409, "stale_generation");
        }
        if self.status == Sync::Desynchronized {
            return Expect::error(409, "graph_desynchronized");
        }
        if self.source_id.as_deref() != Some(envelope.source_id.as_str()) {
            return Expect::error(409, "source_mismatch");
        }
        if self.source_epoch.as_deref() != Some(envelope.source_epoch.as_str()) {
            self.desynchronize();
            return Expect::error(409, "source_epoch_mismatch");
        }
        if !lifecycle && events.iter().any(|e| e.op.is_lifecycle()) {
            return Expect::error(409, "topology_lifecycle_disabled");
        }

        let total = events.len();
        let mut newly_applied = 0usize;
        let mut last_duplicate: Option<Entry> = None;
        for (index, event) in events.iter().enumerate() {
            let is_batch = newly_applied > 0 || total > 1;
            let canonical = event.op.canonical();
            let fail = |feed: &Self, status: u16, outer: &str, inner: &str| {
                if is_batch {
                    Expect::new(status)
                        .with("/result", json!("partial_failure"))
                        .with("/code", json!(outer))
                        .with("/failed_batch_index", json!(index))
                        .with("/failed_delivery_sequence", json!(event.seq))
                        .with("/failed_event_id", json!(event.event_id))
                        .with("/applied_delivery_sequence", json!(feed.applied))
                        .with("/state_version", json!(feed.version.to_string()))
                        .with("/error/code", json!(inner))
                } else {
                    Expect::error(status, inner)
                }
            };
            if event.seq <= self.applied {
                let min = if self.applied >= self.window_capacity as u64 {
                    self.applied - self.window_capacity as u64 + 1
                } else {
                    1
                };
                // Sequence 0 is never a committed sequence: it is outside
                // every window ("wrapped sequences (D = 0) ... are rejected
                // with HTTP 409 unverifiable_duplicate").
                if event.seq < min {
                    return fail(
                        self,
                        409,
                        "unverifiable_duplicate",
                        "unverifiable_duplicate",
                    );
                }
                let retained = self
                    .window
                    .iter()
                    .find(|e| e.seq == event.seq)
                    .cloned()
                    .expect("every sequence in the window range is retained");
                if retained.event_id != event.event_id
                    || retained.pos != event.pos
                    || retained.canonical != canonical
                {
                    self.desynchronize();
                    return fail(self, 409, "conflicting_duplicate", "conflicting_duplicate");
                }
                last_duplicate = Some(retained);
                continue;
            }
            let expected = self.applied + 1;
            if event.seq != expected {
                return fail(self, 409, "out_of_order_sequence", "out_of_order_sequence");
            }
            if event.prev_seq != self.applied
                || event.prev_pos != self.position.as_deref().unwrap_or("")
            {
                self.desynchronize();
                return fail(
                    self,
                    409,
                    "source_predecessor_mismatch",
                    "source_predecessor_mismatch",
                );
            }
            if self.window.iter().any(|e| e.event_id == event.event_id) {
                self.desynchronize();
                return fail(self, 409, "conflicting_duplicate", "conflicting_duplicate");
            }
            let changed = match self.topology.apply(&event.op) {
                Ok(changed) => changed,
                Err(error) => return fail(self, 422, "event_failed", error.code()),
            };
            if changed {
                self.version += 1;
            }
            self.applied = event.seq;
            self.position = Some(event.pos.clone());
            if self.window.len() == self.window_capacity {
                self.window.pop_front();
            }
            self.window.push_back(Entry {
                seq: event.seq,
                event_id: event.event_id.clone(),
                pos: event.pos.clone(),
                canonical,
                changed,
            });
            newly_applied += 1;
        }
        if total == 1 && newly_applied == 0 {
            let dup = last_duplicate.expect("a single event that applied nothing was a duplicate");
            return Expect::new(200)
                .with("/result", json!("duplicate"))
                .with("/status", json!("already_applied"))
                .with("/delivery_sequence", json!(dup.seq))
                .with("/historical_outcome/changed", json!(dup.changed))
                .with("/current_applied_delivery_sequence", json!(self.applied))
                .with("/current_state_version", json!(self.version.to_string()));
        }
        Expect::new(200)
            .with("/result", json!("applied"))
            .with("/generation", json!(self.generation))
            .with("/applied_delivery_sequence", json!(self.applied))
            .with(
                "/applied_source_position",
                json!(self.position.clone().unwrap_or_default()),
            )
            .with("/state_version", json!(self.version.to_string()))
            .with("/events_applied", json!(newly_applied))
    }

    /// Query gate of source-managed mode: `None` when queries are served.
    fn query_refusal(&self) -> Option<&'static str> {
        if self.generation == 0 || self.status == Sync::Uninitialized {
            Some("uninitialized")
        } else if self.status == Sync::Desynchronized {
            Some("desynchronized")
        } else {
            None
        }
    }
}

/// A query of `/v1/operations` or `/v1/batch`.
#[derive(Clone, Copy, Debug)]
enum Query {
    Connected(u64, u64),
    NodeState(u64),
    EdgeState(u64, u64),
}

impl Query {
    fn json(self) -> Value {
        let s = |n: u64| Value::String(n.to_string());
        match self {
            Self::Connected(u, v) => json!({"op": "connected", "source": s(u), "target": s(v)}),
            Self::NodeState(n) => json!({"op": "node_state", "node": s(n)}),
            Self::EdgeState(u, v) => json!({"op": "edge_state", "source": s(u), "target": s(v)}),
        }
    }

    /// Expected outcome object and HTTP status of a single query.
    fn answer(self, t: &Topology) -> (u16, Value) {
        match self {
            Self::Connected(u, v) => match t.connected(u, v) {
                Ok(c) => (200, json!({"result": "connected", "connected": c})),
                Err(e) => (
                    e.direct_status(),
                    json!({"result": "error", "code": e.code()}),
                ),
            },
            Self::NodeState(n) => (
                200,
                json!({"result": "node_state", "registered": t.nodes.contains_key(&n), "active": t.active(n)}),
            ),
            Self::EdgeState(u, v) => {
                if u == v {
                    return (422, json!({"result": "error", "code": "self_loop"}));
                }
                let k = key(u, v);
                (
                    200,
                    json!({
                        "result": "edge_state",
                        "registered": t.edges.contains_key(&k),
                        "enabled": t.edges.get(&k).copied().unwrap_or(false),
                        "effective": t.effective(k),
                    }),
                )
            }
        }
    }
}

/// A direct operation item of `/v1/operations` or `/v1/batch`.
#[derive(Clone, Copy, Debug)]
enum Item {
    Mutation(Op),
    Query(Query),
}

impl Item {
    fn json(self) -> Value {
        match self {
            Self::Mutation(op) => op.json(),
            Self::Query(q) => q.json(),
        }
    }
}

/// Expected outcome (status, object) of a direct mutation in standalone mode.
fn direct_mutation(t: &mut Topology, version: &mut u64, op: Op) -> (u16, Value) {
    match t.apply(&op) {
        Ok(changed) => {
            if changed {
                *version += 1;
            }
            (200, json!({"result": "changed", "changed": changed}))
        }
        Err(e) => (
            e.direct_status(),
            json!({"result": "error", "code": e.code()}),
        ),
    }
}

/// Compares only the fields present in `expected` (messages are not part of
/// the contract).
fn outcome_matches(expected: &Value, actual: &Value) -> bool {
    expected
        .as_object()
        .expect("expected outcome object")
        .iter()
        .all(|(k, v)| actual.get(k) == Some(v))
}

/// The server-side state the model predicts.
#[derive(Clone, Debug)]
enum Model {
    /// Standalone volatile mode.
    Plain { version: u64, topology: Topology },
    /// Source-managed mode.
    Managed(Feed),
}

impl Model {
    fn topology(&self) -> &Topology {
        match self {
            Self::Plain { topology, .. } => topology,
            Self::Managed(f) => &f.topology,
        }
    }

    fn version(&self) -> u64 {
        match self {
            Self::Plain { version, .. } => *version,
            Self::Managed(f) => f.version,
        }
    }
}

// ---------------------------------------------------------------------------
// Child server process
// ---------------------------------------------------------------------------

/// Configuration of one server incarnation.
#[derive(Clone, Copy, Debug)]
struct ServerSpec {
    managed: bool,
    engine: &'static str,
    window: usize,
    lifecycle: bool,
}

/// A running `knotrel-server` child process, killed on drop.
struct Server {
    child: Child,
    client: ExternalFeederClient,
    stderr_path: std::path::PathBuf,
}

impl Server {
    async fn spawn(spec: ServerSpec) -> Self {
        // Reserve a free loopback port (the listener is closed before spawn).
        let address: SocketAddr = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("reserve a loopback port");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_knotrel-server"));
        for (k, _) in std::env::vars() {
            if k.starts_with("KNOTREL_") {
                cmd.env_remove(k);
            }
        }
        cmd.env("KNOTREL_ADDR", address.to_string())
            .env(
                "KNOTREL_MODE",
                if spec.managed {
                    "source_managed"
                } else {
                    "standalone"
                },
            )
            .env("KNOTREL_ENGINE", spec.engine)
            .env("KNOTREL_MAX_NODES", MAX_NODES.to_string())
            .env("KNOTREL_MAX_EDGES", MAX_EDGES.to_string())
            .env("KNOTREL_DEDUP_WINDOW_SIZE", spec.window.to_string())
            .env("KNOTREL_INSPECTION", "true")
            .env(
                "KNOTREL_TOPOLOGY_LIFECYCLE",
                if spec.lifecycle { "true" } else { "false" },
            );
        let stderr_path = std::env::temp_dir().join(format!(
            "knotrel_soak_{}_{}.stderr",
            std::process::id(),
            address.port()
        ));
        let stderr = std::fs::File::create(&stderr_path).expect("create stderr capture");
        cmd.stdout(Stdio::null()).stderr(Stdio::from(stderr));
        let child = cmd.spawn().expect("spawn knotrel-server");
        let mut server = Self {
            child,
            client: ExternalFeederClient::new(address, "soak"),
            stderr_path,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(Some(status)) = server.child.try_wait() {
                panic!(
                    "knotrel-server exited during startup ({status}); stderr:\n{}",
                    server.stderr_tail()
                );
            }
            if let Ok(r) = server.client.raw_request("GET", "/health", &[], "").await
                && r.status == 200
            {
                return server;
            }
            assert!(
                Instant::now() < deadline,
                "knotrel-server did not answer /health; stderr:\n{}",
                server.stderr_tail()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn stderr_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    }

    /// Kills the process abruptly (`SIGKILL` on Unix) and waits for it.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_file(&self.stderr_path);
    }
}

// ---------------------------------------------------------------------------
// Run specification
// ---------------------------------------------------------------------------

/// Parameters of one soak run.
#[derive(Clone, Debug)]
struct RunSpec {
    seed: u64,
    steps: u64,
    max_duration: Duration,
    lifecycle: bool,
    lease_expiries: u32,
    trace: bool,
}

/// Counters printed at the end of a run, to show what was exercised.
#[derive(Default)]
struct Coverage(BTreeMap<String, u64>);

impl Coverage {
    fn bump(&mut self, key: impl Into<String>) {
        *self.0.entry(key.into()).or_default() += 1;
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Session names used by the driver; switching names is a takeover.
const SESSIONS: [&str; 2] = ["feeder-a", "feeder-b"];

/// A request the driver is about to send, with its model prediction.
struct Step {
    label: String,
    method: &'static str,
    path: &'static str,
    instance: Option<String>,
    if_match: Option<String>,
    body: Option<Value>,
    expect: Expect,
    /// For `/v1/batch`: expected per-item outcome objects.
    items: Option<Vec<Value>>,
}

struct Driver {
    spec: RunSpec,
    rng: Rng,
    source: Source,
    server: Server,
    server_spec: ServerSpec,
    model: Model,
    instance_id: String,
    old_instances: Vec<String>,
    /// The driver's own current session identity.
    session: (&'static str, u64),
    /// Identities the driver used before (for stale-session faults).
    old_sessions: Vec<(&'static str, u64)>,
    /// Source change index of the current generation's baseline.
    base: u64,
    /// Source epoch the current generation was imported with.
    imported_epoch: String,
    steps_since_renew: u32,
    last_renew: Instant,
    lease_expiries: u32,
    bad_counter: u64,
    /// The last request rejected for a domain error, with its generation, for
    /// exact resends ("evaluated fresh ... fails with the same domain error").
    last_failed: Option<(u64, Vec<WireEvent>)>,
    step: u64,
    recent: VecDeque<String>,
    coverage: Coverage,
}

impl Driver {
    async fn new(spec: RunSpec) -> Self {
        let mut rng = Rng(spec.seed);
        let server_spec = Self::random_server_spec(&mut rng, spec.lifecycle);
        let server = Server::spawn(server_spec).await;
        let mut driver = Self {
            rng,
            source: Source::new(),
            server,
            server_spec,
            model: Self::fresh_model(server_spec),
            instance_id: String::new(),
            old_instances: Vec::new(),
            session: (SESSIONS[0], 1),
            old_sessions: Vec::new(),
            base: 0,
            imported_epoch: String::new(),
            steps_since_renew: 0,
            last_renew: Instant::now(),
            lease_expiries: 0,
            bad_counter: 0,
            last_failed: None,
            step: 0,
            recent: VecDeque::new(),
            coverage: Coverage::default(),
            spec,
        };
        driver.discover().await;
        driver
    }

    fn random_server_spec(rng: &mut Rng, lifecycle: bool) -> ServerSpec {
        let managed = rng.chance(80);
        let engine = if lifecycle {
            "compact-bfs"
        } else {
            rng.pick(&["compact-bfs", "ett", "hdt"])
        };
        let window = rng.pick(&[1, 2, 3, 5, 8, 64]);
        ServerSpec {
            managed,
            engine,
            window,
            lifecycle,
        }
    }

    fn fresh_model(spec: ServerSpec) -> Model {
        if spec.managed {
            Model::Managed(Feed::new(spec.window))
        } else {
            Model::Plain {
                version: 0,
                topology: Topology::default(),
            }
        }
    }

    async fn discover(&mut self) {
        let r = self.request("GET", "/v1/info", None, None, None).await;
        assert_eq!(r.status, 200, "GET /v1/info: {}", r.body);
        let info = r.json();
        self.instance_id = info["instance_id"].as_str().unwrap_or_default().to_string();
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        instance: Option<&str>,
        if_match: Option<&str>,
        body: Option<&Value>,
    ) -> HttpResponse {
        let mut headers = Vec::new();
        if let Some(i) = instance {
            headers.push(("X-Knotrel-Instance-Id", i));
        }
        if let Some(m) = if_match {
            headers.push(("If-Match", m));
        }
        let body = body.map(Value::to_string).unwrap_or_default();
        match self
            .server
            .client
            .raw_request(method, path, &headers, &body)
            .await
        {
            Ok(r) => r,
            Err(e) => self.fail(&format!("transport error on {method} {path}: {e}")),
        }
    }

    /// Stops the run with a full, reproducible report.
    fn fail(&self, what: &str) -> ! {
        let mut report = String::new();
        let _ = writeln!(report, "\n=== knotrel soak divergence ===");
        let _ = writeln!(
            report,
            "seed=0x{:x} step={} lifecycle={} server={:?}",
            self.spec.seed, self.step, self.spec.lifecycle, self.server_spec
        );
        let _ = writeln!(
            report,
            "reproduce: KNOTREL_SOAK_SEED=0x{:x} KNOTREL_SOAK_STEPS={} KNOTREL_SOAK_LIFECYCLE={} \
             KNOTREL_SOAK_LEASE_EXPIRIES={} \
             cargo test -p knotrel-server --test soak soak_long -- --ignored --nocapture",
            self.spec.seed,
            self.step,
            if self.spec.lifecycle { "on" } else { "off" },
            self.spec.lease_expiries,
        );
        let _ = writeln!(report, "\n{what}\n\nlast steps:");
        for line in &self.recent {
            let _ = writeln!(report, "  {line}");
        }
        let _ = writeln!(report, "\nmodel: {:#?}", self.model);
        let _ = writeln!(
            report,
            "\nserver stderr (tail):\n{}",
            self.server.stderr_tail()
        );
        panic!("{report}");
    }

    fn note(&mut self, label: String) {
        if self.spec.trace {
            eprintln!("[soak {}] {label}", self.step);
        }
        self.recent.push_back(format!("{}: {label}", self.step));
        if self.recent.len() > RECENT_STEPS {
            self.recent.pop_front();
        }
    }

    async fn execute(&mut self, step: Step) {
        self.note(step.label.clone());
        let r = self
            .request(
                step.method,
                step.path,
                step.instance.as_deref(),
                step.if_match.as_deref(),
                step.body.as_ref(),
            )
            .await;
        let mut problems = step.expect.mismatches(&r);
        if let (Some(items), true) = (&step.items, problems.is_empty() && r.status == 200) {
            let actual = r.json()["results"].as_array().cloned().unwrap_or_default();
            if actual.len() != items.len() {
                problems.push(format!(
                    "batch: expected {} results, got {}",
                    items.len(),
                    actual.len()
                ));
            }
            for (i, (e, a)) in items.iter().zip(actual.iter()).enumerate() {
                if !outcome_matches(e, a) {
                    problems.push(format!("batch item {i}: expected {e}, got {a}"));
                }
            }
        }
        if !problems.is_empty() {
            self.fail(&format!(
                "step `{}` ({} {}) diverged:\n  {}\nrequest: {}\nresponse: {} {}",
                step.label,
                step.method,
                step.path,
                problems.join("\n  "),
                step.body.map(|b| b.to_string()).unwrap_or_default(),
                r.status,
                r.body
            ));
        }
        let code = serde_json::from_str::<Value>(&r.body)
            .ok()
            .and_then(|b| {
                b["code"]
                    .as_str()
                    .or(b["result"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| r.status.to_string());
        self.coverage
            .bump(format!("{} {} -> {code}", step.method, step.path));
    }

    // -- identities ---------------------------------------------------------

    fn feed(&mut self) -> Option<&mut Feed> {
        match &mut self.model {
            Model::Managed(f) => Some(f),
            Model::Plain { .. } => None,
        }
    }

    fn envelope(&self) -> Envelope {
        let generation = match &self.model {
            Model::Managed(f) => f.generation,
            Model::Plain { .. } => 0,
        };
        Envelope {
            generation,
            session: self.session.0.to_string(),
            epoch: self.session.1,
            source_id: SOURCE_ID.to_string(),
            source_epoch: self.imported_epoch.clone(),
        }
    }

    fn feeder_body(&self, envelope: &Envelope, extra: Value) -> Value {
        let mut body = json!({
            "instance_id": self.instance_id,
            "graph_id": "default",
            "generation": envelope.generation,
            "feeder_session_id": envelope.session,
            "feeder_epoch": envelope.epoch,
        });
        let obj = body.as_object_mut().expect("object");
        for (k, v) in extra.as_object().expect("extra fields").iter() {
            obj.insert(k.clone(), v.clone());
        }
        body
    }

    // -- the run --------------------------------------------------------------

    async fn run(mut self) {
        let started = Instant::now();
        eprintln!(
            "soak: seed=0x{:x} steps={} max={}s lifecycle={} first server={:?}",
            self.spec.seed,
            self.spec.steps,
            self.spec.max_duration.as_secs(),
            self.spec.lifecycle,
            self.server_spec
        );
        self.verify().await;
        while self.step < self.spec.steps && started.elapsed() < self.spec.max_duration {
            self.step += 1;
            self.steps_since_renew += 1;
            let lease_running = matches!(
                &self.model,
                Model::Managed(f) if f.session.is_some() && !f.lease_expired && !f.short_lease
            );
            if lease_running && self.last_renew.elapsed() > RENEW_GUARD {
                self.fail("environment too slow: no lease renewal within the guard interval");
            }
            if self.server_spec.managed {
                self.managed_step().await;
            } else {
                self.standalone_step().await;
            }
            self.verify().await;
        }
        let mut summary = String::new();
        for (k, v) in &self.coverage.0 {
            let _ = writeln!(summary, "  {v:>7}  {k}");
        }
        eprintln!(
            "soak: seed=0x{:x} passed {} steps in {:.1}s (source head {}):\n{summary}",
            self.spec.seed,
            self.step,
            started.elapsed().as_secs_f64(),
            self.source.head()
        );
    }

    async fn restart(&mut self) {
        let kind = if self.rng.chance(50) {
            "same config"
        } else {
            "new config"
        };
        if kind == "new config" {
            self.server_spec = Self::random_server_spec(&mut self.rng, self.spec.lifecycle);
        }
        self.note(format!("restart (kill, {kind}) -> {:?}", self.server_spec));
        self.server.kill();
        self.server = Server::spawn(self.server_spec).await;
        if !self.instance_id.is_empty() {
            self.old_instances
                .push(std::mem::take(&mut self.instance_id));
        }
        self.model = Self::fresh_model(self.server_spec);
        self.discover().await;
        self.coverage.bump(format!(
            "restart {} {}",
            if self.server_spec.managed {
                "source_managed"
            } else {
                "standalone"
            },
            self.server_spec.engine
        ));
    }

    // -- standalone mode ------------------------------------------------------

    fn random_mutation(&mut self) -> Op {
        let lifecycle = self.spec.lifecycle;
        let roll = self.rng.below(if lifecycle { 100 } else { 70 });
        match roll {
            0..=14 => Op::AddNode(if self.rng.chance(10) {
                OUTSIDE
            } else {
                self.rng.node()
            }),
            15..=44 => {
                if self.rng.chance(5) {
                    let n = self.rng.node();
                    Op::Link(n, n)
                } else {
                    let (u, v) = self.rng.pair();
                    Op::Link(u, v)
                }
            }
            45..=69 => {
                let (u, v) = self.rng.pair();
                Op::Cut(u, v)
            }
            70..=81 => Op::SetNodeActive(self.rng.node(), self.rng.chance(50)),
            82..=93 => {
                let (u, v) = self.rng.pair();
                Op::SetEdgeEnabled(u, v, self.rng.chance(50))
            }
            _ => Op::RemoveNode(self.rng.node()),
        }
    }

    fn random_query(&mut self) -> Query {
        let lifecycle = self.spec.lifecycle;
        let u = if self.rng.chance(10) {
            OUTSIDE
        } else {
            self.rng.node()
        };
        let v = self.rng.node();
        match self.rng.below(if lifecycle { 3 } else { 1 }) {
            0 => Query::Connected(u, v),
            1 => Query::NodeState(u),
            _ => Query::EdgeState(u, v),
        }
    }

    async fn standalone_step(&mut self) {
        let roll = self.rng.below(100);
        let Model::Plain {
            mut version,
            mut topology,
        } = self.model.clone()
        else {
            unreachable!("standalone step on a managed model")
        };
        let step = match roll {
            0..=1 => return self.restart().await,
            2..=31 => {
                let op = self.random_mutation();
                let (status, outcome) = direct_mutation(&mut topology, &mut version, op);
                let mut expect = Expect::new(status);
                for (k, v) in outcome.as_object().expect("object") {
                    expect = expect.with(pointer_for(k), v.clone());
                }
                Step {
                    label: format!("standalone op {op:?}"),
                    method: "POST",
                    path: "/v1/operations",
                    instance: None,
                    if_match: None,
                    body: Some(op.json()),
                    expect,
                    items: None,
                }
            }
            32..=79 => {
                let n = 1 + self.rng.below(16);
                let mut items = Vec::new();
                let mut outcomes = Vec::new();
                for _ in 0..n {
                    if self.rng.chance(65) {
                        let op = self.random_mutation();
                        outcomes.push(direct_mutation(&mut topology, &mut version, op).1);
                        items.push(Item::Mutation(op));
                    } else {
                        let q = self.random_query();
                        outcomes.push(q.answer(&topology).1);
                        items.push(Item::Query(q));
                    }
                }
                Step {
                    label: format!("standalone batch {items:?}"),
                    method: "POST",
                    path: "/v1/batch",
                    instance: None,
                    if_match: None,
                    body: Some(
                        json!({"operations": items.iter().map(|i| i.json()).collect::<Vec<_>>()}),
                    ),
                    expect: Expect::new(200),
                    items: Some(outcomes),
                }
            }
            80..=94 => {
                let q = self.random_query();
                let (status, outcome) = q.answer(&topology);
                let mut expect = Expect::new(status);
                for (k, v) in outcome.as_object().expect("object") {
                    expect = expect.with(pointer_for(k), v.clone());
                }
                Step {
                    label: format!("standalone query {q:?}"),
                    method: "POST",
                    path: "/v1/operations",
                    instance: None,
                    if_match: None,
                    body: Some(q.json()),
                    expect,
                    items: None,
                }
            }
            _ => {
                let path = self.rng.pick(&[
                    "/v1/feeder/session",
                    "/v1/feeder/events",
                    "/v1/feeder/import",
                    "/v1/feeder/barrier",
                    "/v1/feeder/gap",
                    "/v1/feeder/heartbeat",
                ]);
                Step {
                    label: format!("standalone feeder request {path}"),
                    method: "POST",
                    path,
                    instance: Some("srv-00000000-0000-0000-0000-000000000000".into()),
                    if_match: None,
                    body: Some(json!({})),
                    expect: Expect::error(409, "mode_conflict"),
                    items: None,
                }
            }
        };
        self.model = Model::Plain { version, topology };
        self.execute(step).await;
    }

    // -- source-managed mode --------------------------------------------------

    /// Picks and executes one source-managed action. Weights favour the
    /// recovery path the state calls for, but every action stays possible so
    /// that off-policy requests are exercised too.
    async fn managed_step(&mut self) {
        let Model::Managed(feed) = self.model.clone() else {
            unreachable!("managed step on a standalone model")
        };
        let owner =
            feed.session.as_deref() == Some(self.session.0) && feed.feeder_epoch == self.session.1;
        let live = feed.generation > 0 && feed.status != Sync::Desynchronized;
        let caught_up = live && self.base + feed.applied == self.source.head();

        // Renewals are forced deterministically (by step count, never by
        // time). After a deliberate expiry the driver keeps acting on the
        // expired lease for a few steps before it re-registers.
        if owner && feed.short_lease && self.rng.chance(25) {
            // Re-registering the same epoch renews the lease but leaves the
            // epoch fenced for synchronization proof; a higher epoch clears it.
            if self.rng.chance(50) {
                self.old_sessions.push(self.session);
                self.session.1 += 1;
            }
            return self
                .register(self.session.0, self.session.1, LONG_LEASE_MS)
                .await;
        }
        if owner && !feed.short_lease && self.steps_since_renew >= RENEW_EVERY_STEPS {
            return self.heartbeat(self.session).await;
        }

        let lifecycle_fault = !self.spec.lifecycle;
        let can_expire = self.lease_expiries < self.spec.lease_expiries && owner;
        let table: [(u32, Action); 30] = [
            (if owner { 1 } else { 60 }, Action::Register),
            (4, Action::Heartbeat),
            (2, Action::BumpEpoch),
            (2, Action::Takeover),
            (1, Action::StaleRegister),
            (if can_expire { 1 } else { 0 }, Action::LeaseExpiry),
            (
                if feed.generation == 0 { 50 } else { 1 },
                Action::CreateOnly,
            ),
            (
                if feed.status == Sync::Desynchronized {
                    50
                } else {
                    2
                },
                Action::Replace,
            ),
            (1, Action::ReplaceStale),
            (if live { 40 } else { 2 }, Action::Deliver),
            (10, Action::SourceAdvance),
            (if live { 6 } else { 1 }, Action::Duplicates),
            (2, Action::EvictedDuplicate),
            (3, Action::Gap),
            (6, Action::DomainFault),
            (1, Action::ConflictingDuplicate),
            (1, Action::PredecessorMismatch),
            (2, Action::StaleGeneration),
            (2, Action::StaleSession),
            (1, Action::SourceMismatch),
            (1, Action::SourceEpochReset),
            (2, Action::StaleInstance),
            (
                if lifecycle_fault { 2 } else { 0 },
                Action::LifecycleDisabled,
            ),
            (
                if caught_up && feed.status != Sync::Ready {
                    40
                } else {
                    3
                },
                Action::Barrier,
            ),
            (3, Action::BadBarrier),
            (1, Action::DeclareGap),
            (6, Action::Query),
            (5, Action::QueryBatch),
            (2, Action::DirectMutation),
            (1, Action::Restart),
        ];
        let total: u32 = table.iter().map(|(w, _)| w).sum();
        let mut roll = self.rng.below(total as usize) as u32;
        let mut action = Action::Query;
        for (w, a) in table {
            if roll < w {
                action = a;
                break;
            }
            roll -= w;
        }
        self.coverage.bump(format!("action {action:?}"));
        match action {
            Action::Register => {
                self.register(self.session.0, self.session.1, LONG_LEASE_MS)
                    .await;
            }
            Action::Heartbeat => {
                if feed.short_lease {
                    // A one-second renewal cannot be modelled; register instead.
                    self.register(self.session.0, self.session.1, LONG_LEASE_MS)
                        .await;
                } else {
                    self.heartbeat(self.session).await;
                }
            }
            Action::BumpEpoch => {
                self.old_sessions.push(self.session);
                self.session.1 = self.session.1.max(feed.feeder_epoch) + 1;
                self.register(self.session.0, self.session.1, LONG_LEASE_MS)
                    .await;
            }
            Action::Takeover => {
                self.old_sessions.push(self.session);
                let other = if self.session.0 == SESSIONS[0] {
                    SESSIONS[1]
                } else {
                    SESSIONS[0]
                };
                self.session = (other, self.session.1.max(feed.feeder_epoch) + 1);
                self.register(self.session.0, self.session.1, LONG_LEASE_MS)
                    .await;
            }
            Action::StaleRegister => {
                let stale = (self.session.0, self.session.1.saturating_sub(1));
                self.register(stale.0, stale.1, LONG_LEASE_MS).await;
            }
            Action::LeaseExpiry => self.lease_expiry().await,
            Action::CreateOnly => self.import(true, ImportFault::None).await,
            Action::Replace => self.import(false, ImportFault::None).await,
            Action::ReplaceStale => self.import(false, ImportFault::StaleIfMatch).await,
            Action::Deliver => self.deliver(Fault::None).await,
            Action::SourceAdvance => {
                let n = 1 + self.rng.below(8);
                for _ in 0..n {
                    self.source.advance(&mut self.rng, self.spec.lifecycle);
                }
                self.note(format!("source advanced to {}", self.source.head()));
            }
            Action::Duplicates => self.deliver(Fault::DuplicatesOnly).await,
            Action::EvictedDuplicate => self.deliver(Fault::Evicted).await,
            Action::Gap => self.deliver(Fault::Gap).await,
            Action::DomainFault => self.deliver(Fault::Domain).await,
            Action::ConflictingDuplicate => self.deliver(Fault::Conflicting).await,
            Action::PredecessorMismatch => self.deliver(Fault::Predecessor).await,
            Action::StaleGeneration => self.deliver(Fault::StaleGeneration).await,
            Action::StaleSession => self.deliver(Fault::StaleSession).await,
            Action::SourceMismatch => self.deliver(Fault::SourceId).await,
            Action::SourceEpochReset => {
                // The source is restored and changes its epoch; the feeder
                // keeps streaming with the new epoch until it rebuilds.
                self.source.epoch += 1;
                self.note(format!("source epoch reset to {}", self.source.epoch));
                self.deliver(Fault::SourceEpoch).await;
            }
            Action::StaleInstance => self.deliver(Fault::StaleInstance).await,
            Action::LifecycleDisabled => self.deliver(Fault::LifecycleDisabled).await,
            Action::Barrier => self.barrier(BarrierKind::Exact).await,
            Action::BadBarrier => {
                let kind = self.rng.pick(&[
                    BarrierKind::Premature,
                    BarrierKind::Superseded,
                    BarrierKind::Position,
                    BarrierKind::StaleEpoch,
                ]);
                self.barrier(kind).await;
            }
            Action::DeclareGap => self.declare_gap().await,
            Action::Query => self.managed_query().await,
            Action::QueryBatch => self.managed_query_batch().await,
            Action::DirectMutation => self.managed_direct_mutation().await,
            Action::Restart => self.restart().await,
        }
    }

    async fn register(&mut self, session: &'static str, epoch: u64, lease_ms: u64) {
        let instance = self.instance_id.clone();
        let feed = self.feed().expect("managed");
        let expect = feed.register(session, epoch, lease_ms);
        let accepted = expect.status == 200;
        let step = Step {
            label: format!("register {session}/{epoch} lease={lease_ms}ms"),
            method: "POST",
            path: "/v1/feeder/session",
            body: Some(json!({
                "instance_id": instance,
                "feeder_session_id": session,
                "feeder_epoch": epoch,
                "lease_duration_ms": lease_ms,
            })),
            instance: Some(instance),
            if_match: None,
            expect,
            items: None,
        };
        self.execute(step).await;
        if accepted {
            self.session = (session, epoch);
            self.steps_since_renew = 0;
            self.last_renew = Instant::now();
        }
    }

    async fn heartbeat(&mut self, identity: (&'static str, u64)) {
        let instance = self.instance_id.clone();
        let feed = self.feed().expect("managed");
        let expect = feed.heartbeat(identity.0, identity.1);
        let accepted = expect.status == 200;
        let step = Step {
            label: format!("heartbeat {}/{}", identity.0, identity.1),
            method: "POST",
            path: "/v1/feeder/heartbeat",
            body: Some(json!({
                "instance_id": instance,
                "feeder_session_id": identity.0,
                "feeder_epoch": identity.1,
            })),
            instance: Some(instance),
            if_match: None,
            expect,
            items: None,
        };
        self.execute(step).await;
        if accepted {
            self.steps_since_renew = 0;
            self.last_renew = Instant::now();
        }
    }

    /// Registers a one-second lease, waits until it has certainly expired and
    /// records the expiry in the model.
    async fn lease_expiry(&mut self) {
        self.lease_expiries += 1;
        self.register(self.session.0, self.session.1, SHORT_LEASE_MS)
            .await;
        tokio::time::sleep(Duration::from_millis(SHORT_LEASE_MS + 250)).await;
        if let Some(feed) = self.feed()
            && feed.session.is_some()
        {
            feed.lease_expired = true;
        }
        self.note("lease expired (waited past the one-second lease)".into());
    }

    async fn import(&mut self, create_only: bool, fault: ImportFault) {
        // Snapshot at the head, sometimes a little behind it (a backlog to
        // catch up).
        let behind = if self.rng.chance(30) {
            self.rng.below(8) as u64
        } else {
            0
        };
        let at = self.source.head().saturating_sub(behind);
        let snapshot = self
            .source
            .snapshot(at)
            .cloned()
            .expect("recent snapshot retained");
        let mut envelope = self.envelope();
        envelope.source_epoch = self.source.epoch_string();
        let instance = self.instance_id.clone();
        let lifecycle = self.spec.lifecycle;
        let feed = self.feed().expect("managed");
        let if_match = if create_only {
            None
        } else {
            match fault {
                ImportFault::None => Some(feed.etag()),
                ImportFault::StaleIfMatch => Some(format!(
                    "\"{}.{}.{}\"",
                    feed.generation,
                    feed.version + 1,
                    feed.position.as_deref().unwrap_or("")
                )),
            }
        };
        let expect = feed.import(
            create_only,
            if_match.as_deref(),
            &envelope,
            &position(at),
            &snapshot,
        );
        let published = expect.status == 201;
        let nodes: Vec<Value> = snapshot
            .nodes
            .iter()
            .map(|(&n, &active)| {
                if lifecycle && !active {
                    json!({"node": n.to_string(), "active": false})
                } else {
                    json!(n.to_string())
                }
            })
            .collect();
        let edges: Vec<Value> = snapshot
            .edges
            .iter()
            .map(|(&(a, b), &enabled)| {
                if lifecycle && !enabled {
                    json!({"source": b.to_string(), "target": a.to_string(), "enabled": false})
                } else {
                    json!({"source": a.to_string(), "target": b.to_string()})
                }
            })
            .collect();
        let mut body = self.feeder_body(
            &envelope,
            json!({
                "mode": if create_only { "create_only" } else { "replace" },
                "source_id": envelope.source_id,
                "source_epoch": envelope.source_epoch,
                "source_position": position(at),
                "nodes": nodes,
                "edges": edges,
            }),
        );
        // Imports carry no `generation` field.
        body.as_object_mut().expect("object").remove("generation");
        let step = Step {
            label: format!(
                "import {} at p{at} (head {}) {fault:?} if_match={if_match:?}",
                if create_only {
                    "create_only"
                } else {
                    "replace"
                },
                self.source.head()
            ),
            method: "POST",
            path: "/v1/feeder/import",
            instance: Some(instance),
            if_match,
            body: Some(body),
            expect,
            items: None,
        };
        self.execute(step).await;
        if published {
            self.base = at;
            self.imported_epoch = envelope.source_epoch;
        }
    }

    /// Builds and sends one `/v1/feeder/events` request.
    async fn deliver(&mut self, fault: Fault) {
        let Model::Managed(feed) = self.model.clone() else {
            unreachable!()
        };
        let mut envelope = self.envelope();
        let mut instance = self.instance_id.clone();
        let applied_k = self.base + feed.applied;
        // Ensure the source has changes to deliver.
        if applied_k + 8 > self.source.head() {
            let n = 1 + self.rng.below(8);
            for _ in 0..n {
                self.source.advance(&mut self.rng, self.spec.lifecycle);
            }
        }
        let head = self.source.head();
        // `next` source changes, valid in the current generation.
        let next = |s: &Self, count: usize| -> Vec<WireEvent> {
            (applied_k + 1..=head.min(applied_k + count as u64))
                .filter(|&k| k > s.base)
                .map(|k| s.source.event(k, s.base))
                .collect()
        };
        let retained: Vec<WireEvent> = feed
            .window
            .iter()
            .map(|e| self.source.event(self.base + e.seq, self.base))
            .collect();
        let count = 1 + self.rng.below(6);
        let mut events: Vec<WireEvent> = Vec::new();
        match fault {
            Fault::None => {
                if !retained.is_empty() && self.rng.chance(20) {
                    let r = 1 + self.rng.below(retained.len().min(3));
                    events.extend(retained[retained.len() - r..].iter().cloned());
                }
                events.extend(next(self, count));
            }
            Fault::DuplicatesOnly => {
                if retained.is_empty() {
                    events.extend(next(self, 1));
                } else {
                    let r = 1 + self.rng.below(retained.len().min(4));
                    let start = self.rng.below(retained.len() - r + 1);
                    events.extend(retained[start..start + r].iter().cloned());
                }
            }
            Fault::Evicted => {
                let min_retained = feed.window.front().map(|e| e.seq).unwrap_or(1);
                if min_retained > 1 {
                    let seq = 1 + self.rng.below((min_retained - 1) as usize) as u64;
                    events.push(self.source.event(self.base + seq, self.base));
                } else {
                    // Sequence 0 is outside every window, also while nothing
                    // is applied in the generation (defect D1, fixed).
                    let mut e = next(self, 1)
                        .pop()
                        .unwrap_or_else(|| self.source.event(head.max(1), 0));
                    e.seq = 0;
                    e.prev_seq = 0;
                    e.event_id = format!("zero{}", self.bump_bad());
                    events.push(e);
                }
                events.extend(next(self, count.min(2)).into_iter().take(self.rng.below(2)));
            }
            Fault::Gap => {
                let mut batch = next(self, count + 1);
                if batch.len() >= 2 {
                    let skip = self.rng.below(batch.len() - 1) + 1;
                    batch.remove(skip.min(batch.len() - 1));
                    if self.rng.chance(50) {
                        batch.remove(0);
                    }
                }
                events.extend(batch);
            }
            Fault::Domain
                if self.rng.chance(30)
                    && self
                        .last_failed
                        .as_ref()
                        .is_some_and(|(g, _)| *g == feed.generation) =>
            {
                // Exact resend of a request that failed with a domain error.
                self.coverage
                    .bump("events: exact resend of a failed request");
                events.extend(self.last_failed.clone().expect("checked").1);
            }
            Fault::Domain => {
                let prefix = self.rng.below(count);
                let mut batch = next(self, prefix);
                let mut topology = feed.topology.clone();
                for e in &batch {
                    let _ = topology.apply(&e.op);
                }
                let seq = feed.applied + batch.len() as u64 + 1;
                let bad = self.failing_op(&topology);
                let id = format!("bad{}", self.bump_bad());
                let k = self.base + seq;
                batch.push(WireEvent {
                    event_id: id,
                    seq,
                    prev_seq: seq - 1,
                    pos: format!("bad-p{k}"),
                    prev_pos: position(k - 1),
                    op: bad,
                });
                if self.rng.chance(40) {
                    batch.extend(next(self, prefix + 2).into_iter().skip(prefix + 1));
                }
                events.extend(batch);
            }
            Fault::Conflicting => {
                if let Some(r) = retained.last().cloned() {
                    let mut e = if self.rng.chance(50) {
                        retained[self.rng.below(retained.len())].clone()
                    } else {
                        r
                    };
                    match self.rng.below(5) {
                        0 => e.event_id = format!("other{}", self.bump_bad()),
                        1 => e.op = alter(e.op),
                        4 => e.op = alter_endpoints(e.op),
                        2 => e.pos = format!("{}x", e.pos),
                        3 => {
                            // Reuse a retained event ID at the next sequence.
                            if let Some(mut n) = next(self, 1).pop() {
                                n.event_id = e.event_id.clone();
                                e = n;
                            } else {
                                e.event_id = format!("other{}", self.bump_bad());
                            }
                        }
                        _ => unreachable!("five variants"),
                    }
                    events.push(e);
                } else {
                    events.extend(next(self, 1));
                }
            }
            Fault::Predecessor => {
                let mut batch = next(self, count);
                if let Some(i) = (!batch.is_empty()).then(|| self.rng.below(batch.len())) {
                    if self.rng.chance(50) {
                        batch[i].prev_pos = format!("{}?", batch[i].prev_pos);
                    } else {
                        batch[i].prev_seq += 1;
                    }
                }
                events.extend(batch);
            }
            Fault::StaleGeneration => {
                envelope.generation = if envelope.generation > 0 && self.rng.chance(70) {
                    envelope.generation - 1
                } else {
                    envelope.generation + 1
                };
                events.extend(next(self, count));
            }
            Fault::StaleSession => {
                if let Some(&(s, e)) = self.old_sessions.last() {
                    envelope.session = s.to_string();
                    envelope.epoch = e;
                } else {
                    envelope.epoch += 1;
                }
                events.extend(next(self, count));
            }
            Fault::SourceId => {
                envelope.source_id = "another-source".into();
                events.extend(next(self, count));
            }
            Fault::SourceEpoch => {
                envelope.source_epoch = self.source.epoch_string();
                events.extend(next(self, count));
            }
            Fault::StaleInstance => {
                if let Some(old) = self.old_instances.last() {
                    instance = old.clone();
                } else {
                    instance = "srv-00000000-0000-0000-0000-000000000000".into();
                }
                events.extend(next(self, count));
            }
            Fault::LifecycleDisabled => {
                let mut batch = next(self, count);
                let seq = feed.applied + batch.len() as u64 + 1;
                let k = self.base + seq;
                let at = self.rng.below(batch.len() + 1);
                batch.insert(
                    at,
                    WireEvent {
                        event_id: format!("lc{}", self.bump_bad()),
                        seq,
                        prev_seq: seq - 1,
                        pos: position(k),
                        prev_pos: position(k - 1),
                        op: Op::SetNodeActive(self.rng.node(), false),
                    },
                );
                events.extend(batch);
            }
        }
        if events.is_empty() {
            // Nothing to deliver in this state (for example before the first
            // import): send the next source change at sequence 1.
            let k = applied_k.max(self.base) + 1;
            if k > self.source.head() {
                self.source.advance(&mut self.rng, self.spec.lifecycle);
            }
            events.push(self.source.event(k, k - 1));
        }
        // Wrong instance IDs are rejected before the model is consulted.
        let expect = if instance != self.instance_id {
            Expect::error(409, "stale_instance_id")
        } else {
            let lifecycle = self.spec.lifecycle;
            self.feed()
                .expect("managed")
                .events(&envelope, &events, lifecycle)
        };
        let domain_failure = expect.status == 422;
        let mut extra = self.feeder_body(
            &envelope,
            json!({
                "source_id": envelope.source_id,
                "source_epoch": envelope.source_epoch,
                "events": events.iter().map(WireEvent::json).collect::<Vec<_>>(),
            }),
        );
        extra["instance_id"] = json!(instance);
        let step = Step {
            label: format!(
                "events {fault:?} gen={} {}/{} [{}]",
                envelope.generation,
                envelope.session,
                envelope.epoch,
                events
                    .iter()
                    .map(WireEvent::short)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            method: "POST",
            path: "/v1/feeder/events",
            instance: Some(instance),
            if_match: None,
            body: Some(extra),
            expect,
            items: None,
        };
        self.execute(step).await;
        if domain_failure {
            self.last_failed = Some((envelope.generation, events));
        }
    }

    fn bump_bad(&mut self) -> u64 {
        self.bad_counter += 1;
        self.bad_counter
    }

    /// An operation the model predicts to fail on `t`.
    fn failing_op(&mut self, t: &Topology) -> Op {
        let mut candidates = Vec::new();
        let n = self.rng.node();
        candidates.push(Op::Link(n, n));
        candidates.push(Op::Cut(n, n));
        candidates.push(Op::AddNode(OUTSIDE));
        let (u, v) = self.rng.pair();
        candidates.push(Op::Link(u, OUTSIDE));
        candidates.push(Op::Link(u, v));
        if self.spec.lifecycle {
            candidates.push(Op::SetNodeActive(OUTSIDE, self.rng.chance(50)));
            candidates.push(Op::SetEdgeEnabled(u, v, self.rng.chance(50)));
            candidates.push(Op::SetEdgeEnabled(n, n, false));
            candidates.push(Op::SetEdgeEnabled(OUTSIDE, u, true));
        }
        let failing: Vec<Op> = candidates
            .into_iter()
            .filter(|op| t.clone().apply(op).is_err())
            .collect();
        self.rng.pick(&failing)
    }

    async fn barrier(&mut self, kind: BarrierKind) {
        let Model::Managed(feed) = self.model.clone() else {
            unreachable!()
        };
        let mut envelope = self.envelope();
        let mut seq = feed.applied;
        let mut pos = feed.position.clone().unwrap_or_default();
        match kind {
            BarrierKind::Exact => {}
            BarrierKind::Premature => seq += 1,
            BarrierKind::Superseded => seq = seq.saturating_sub(1),
            BarrierKind::Position => pos.push('!'),
            BarrierKind::StaleEpoch => {
                if let Some(&(s, e)) = self.old_sessions.last() {
                    envelope.session = s.to_string();
                    envelope.epoch = e;
                }
            }
        }
        let instance = self.instance_id.clone();
        let expect = self.feed().expect("managed").barrier(&envelope, seq, &pos);
        let body = self.feeder_body(
            &envelope,
            json!({"barrier_delivery_sequence": seq, "barrier_source_position": pos}),
        );
        let step = Step {
            label: format!("barrier {kind:?} D={seq} P={pos}"),
            method: "POST",
            path: "/v1/feeder/barrier",
            instance: Some(instance),
            if_match: None,
            body: Some(body),
            expect,
            items: None,
        };
        self.execute(step).await;
    }

    async fn declare_gap(&mut self) {
        let envelope = self.envelope();
        let instance = self.instance_id.clone();
        let failed = match &self.model {
            Model::Managed(f) => f.applied + 1,
            Model::Plain { .. } => 1,
        };
        let expect = self.feed().expect("managed").gap(&envelope);
        let body = self.feeder_body(
            &envelope,
            json!({"failed_delivery_sequence": failed, "reason": "soak: simulated source history loss"}),
        );
        let step = Step {
            label: format!("declare gap at {failed}"),
            method: "POST",
            path: "/v1/feeder/gap",
            instance: Some(instance),
            if_match: None,
            body: Some(body),
            expect,
            items: None,
        };
        self.execute(step).await;
    }

    async fn managed_query(&mut self) {
        let Model::Managed(feed) = self.model.clone() else {
            unreachable!()
        };
        let q = self.random_query();
        let expect = match feed.query_refusal() {
            Some(code) => Expect::new(503).with("/code", json!(code)),
            None => {
                let (status, outcome) = q.answer(&feed.topology);
                let mut expect = Expect::new(status);
                for (k, v) in outcome.as_object().expect("object") {
                    expect = expect.with(pointer_for(k), v.clone());
                }
                expect
            }
        };
        let step = Step {
            label: format!("query {q:?}"),
            method: "POST",
            path: "/v1/operations",
            instance: None,
            if_match: None,
            body: Some(q.json()),
            expect,
            items: None,
        };
        self.execute(step).await;
    }

    async fn managed_query_batch(&mut self) {
        let Model::Managed(feed) = self.model.clone() else {
            unreachable!()
        };
        let n = 1 + self.rng.below(12);
        let queries: Vec<Query> = (0..n).map(|_| self.random_query()).collect();
        let outcomes = queries
            .iter()
            .map(|q| match feed.query_refusal() {
                Some(code) => json!({"result": "error", "code": code}),
                None => q.answer(&feed.topology).1,
            })
            .collect();
        let step = Step {
            label: format!("query batch {queries:?}"),
            method: "POST",
            path: "/v1/batch",
            instance: None,
            if_match: None,
            body: Some(json!({"operations": queries.iter().map(|q| q.json()).collect::<Vec<_>>()})),
            expect: Expect::new(200),
            items: Some(outcomes),
        };
        self.execute(step).await;
    }

    async fn managed_direct_mutation(&mut self) {
        let lifecycle_probe = !self.spec.lifecycle && self.rng.chance(25);
        let op = if lifecycle_probe {
            Op::SetNodeActive(self.rng.node(), false)
        } else {
            self.random_mutation()
        };
        // Step 5a (lifecycle support off) precedes step 6 (mode).
        let code = if lifecycle_probe {
            "topology_lifecycle_disabled"
        } else {
            "direct_mutation_disabled"
        };
        let (path, body) = if self.rng.chance(50) {
            ("/v1/operations", op.json())
        } else {
            let q = self.random_query();
            ("/v1/batch", json!({"operations": [q.json(), op.json()]}))
        };
        let step = Step {
            label: format!("direct mutation {op:?} via {path}"),
            method: "POST",
            path,
            instance: None,
            if_match: None,
            body: Some(body),
            expect: Expect::new(409).with("/code", json!(code)),
            items: None,
        };
        self.execute(step).await;
    }

    // -- verification ---------------------------------------------------------

    /// Compares the complete observable state with the model.
    async fn verify(&mut self) {
        let model = self.model.clone();
        let topology = model.topology();
        let lifecycle = self.spec.lifecycle;
        let mut problems = Vec::new();

        // GET /v1/stats
        let stats = self.request("GET", "/v1/stats", None, None, None).await;
        if stats.status != 200 {
            problems.push(format!("stats: status {} {}", stats.status, stats.body));
        } else {
            let s = stats.json();
            let mut want = vec![
                ("node_count", json!(topology.nodes.len())),
                ("edge_count", json!(topology.edges.len())),
                ("state_version", json!(model.version().to_string())),
            ];
            if lifecycle {
                want.push((
                    "active_node_count",
                    json!(topology.nodes.values().filter(|a| **a).count()),
                ));
                want.push((
                    "enabled_edge_count",
                    json!(topology.edges.values().filter(|e| **e).count()),
                ));
                want.push((
                    "effective_edge_count",
                    json!(
                        topology
                            .edges
                            .keys()
                            .filter(|k| topology.effective(**k))
                            .count()
                    ),
                ));
            }
            if let Model::Managed(f) = &model {
                want.push(("generation", json!(f.generation)));
                want.push(("sync_status", json!(f.effective_status().as_str())));
                want.push((
                    "source_position",
                    f.position.clone().map_or(Value::Null, Value::String),
                ));
                if stats.header("etag") != Some(f.etag().as_str()) {
                    problems.push(format!(
                        "stats ETag: expected {}, got {:?}",
                        f.etag(),
                        stats.header("etag")
                    ));
                }
            }
            for (k, v) in want {
                // An absent `source_position` means "none" (before the first import).
                if s.get(k).unwrap_or(&Value::Null) != &v {
                    problems.push(format!("stats.{k}: expected {v}, got {:?}", s.get(k)));
                }
            }
        }

        // GET /ready
        let ready = self.request("GET", "/ready", None, None, None).await;
        let (want_status, want_body) = match &model {
            Model::Managed(f) => f.readiness(),
            Model::Plain { .. } => (200, "ready"),
        };
        let got_body = serde_json::from_str::<Value>(&ready.body)
            .ok()
            .and_then(|v| v["status"].as_str().map(str::to_string));
        if ready.status != want_status || got_body.as_deref() != Some(want_body) {
            problems.push(format!(
                "ready: expected {want_status} {want_body}, got {} {}",
                ready.status, ready.body
            ));
        }

        // Inspection: every registered node with flag and degree.
        let nodes = self
            .request("GET", "/v1/nodes?limit=1000", None, None, None)
            .await;
        let edge_listing = self.server_spec.engine == "compact-bfs";
        if nodes.status != 200 {
            problems.push(format!("nodes: status {} {}", nodes.status, nodes.body));
        } else {
            let got: Vec<Value> = nodes.json()["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let want: Vec<Value> = topology
                .nodes
                .iter()
                .map(|(&n, &active)| {
                    let degree = if edge_listing {
                        json!(topology.degree(n))
                    } else {
                        Value::Null
                    };
                    json!({"node": n.to_string(), "active": active, "degree": degree})
                })
                .collect();
            if got != want {
                problems.push(format!(
                    "nodes: expected {}, got {}",
                    Value::from(want),
                    Value::from(got)
                ));
            }
        }
        if edge_listing {
            let edges = self
                .request("GET", "/v1/edges?limit=1000", None, None, None)
                .await;
            if edges.status != 200 {
                problems.push(format!("edges: status {} {}", edges.status, edges.body));
            } else {
                let mut got: Vec<(u64, u64, bool, bool)> = edges.json()["edges"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|e| {
                        let id = |f: &str| e[f].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                        (
                            id("source"),
                            id("target"),
                            e["enabled"].as_bool().unwrap_or(false),
                            e["effective"].as_bool().unwrap_or(false),
                        )
                    })
                    .collect();
                got.sort_unstable();
                let want: Vec<(u64, u64, bool, bool)> = topology
                    .edges
                    .iter()
                    .map(|(&(a, b), &en)| (a, b, en, topology.effective((a, b))))
                    .collect();
                if got != want {
                    problems.push(format!("edges: expected {want:?}, got {got:?}"));
                }
            }
        }

        // Connectivity of every pair (and node state) in one query batch.
        let mut probes: Vec<u64> = UNIVERSE.to_vec();
        probes.push(OUTSIDE);
        let mut queries = Vec::new();
        for (i, &u) in probes.iter().enumerate() {
            for &v in &probes[i..] {
                queries.push(Query::Connected(u, v));
            }
            if lifecycle {
                queries.push(Query::NodeState(u));
            }
        }
        let refusal = match &model {
            Model::Managed(f) => f.query_refusal(),
            Model::Plain { .. } => None,
        };
        let body = json!({"operations": queries.iter().map(|q| q.json()).collect::<Vec<_>>()});
        let batch = self
            .request("POST", "/v1/batch", None, None, Some(&body))
            .await;
        if batch.status != 200 {
            problems.push(format!(
                "probe batch: status {} {}",
                batch.status, batch.body
            ));
        } else {
            let results = batch.json()["results"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if results.len() != queries.len() {
                problems.push(format!(
                    "probe batch: expected {} results, got {}",
                    queries.len(),
                    results.len()
                ));
            }
            for (q, a) in queries.iter().zip(results.iter()) {
                let want = match refusal {
                    Some(code) => json!({"result": "error", "code": code}),
                    None => q.answer(topology).1,
                };
                if !outcome_matches(&want, a) {
                    problems.push(format!("probe {q:?}: expected {want}, got {a}"));
                }
            }
        }

        // Convergence with the authoritative source.
        if let Model::Managed(f) = &model
            && f.generation > 0
            && let Some(expected) = self.source.snapshot(self.base + f.applied)
            && expected != &f.topology
        {
            problems.push(format!(
                "model diverged from the source snapshot at p{}",
                self.base + f.applied
            ));
        }

        if !problems.is_empty() {
            problems.truncate(12);
            self.fail(&format!(
                "verification after step {} failed:\n  {}",
                self.step,
                problems.join("\n  ")
            ));
        }
    }
}

/// JSON pointer for a top-level outcome field.
fn pointer_for(field: &str) -> &'static str {
    match field {
        "result" => "/result",
        "changed" => "/changed",
        "connected" => "/connected",
        "code" => "/code",
        "registered" => "/registered",
        "active" => "/active",
        "enabled" => "/enabled",
        "effective" => "/effective",
        other => panic!("no pointer for outcome field {other}"),
    }
}

/// A different operation with the same endpoints (for altered duplicates).
fn alter(op: Op) -> Op {
    match op {
        Op::Link(u, v) => Op::Cut(u, v),
        Op::Cut(u, v) => Op::Link(u, v),
        Op::AddNode(n) => Op::RemoveNode(n),
        Op::RemoveNode(n) => Op::AddNode(n),
        Op::Progress => Op::AddNode(OUTSIDE),
        Op::SetNodeActive(n, a) => Op::SetNodeActive(n, !a),
        Op::SetEdgeEnabled(u, v, e) => Op::SetEdgeEnabled(u, v, !e),
    }
}

/// The same operation type with different endpoints or node.
fn alter_endpoints(op: Op) -> Op {
    let other = |n: u64| if n == OUTSIDE { UNIVERSE[0] } else { OUTSIDE };
    match op {
        Op::Link(u, v) => Op::Link(u, other(v)),
        Op::Cut(u, v) => Op::Cut(other(u), v),
        Op::AddNode(n) => Op::AddNode(other(n)),
        Op::RemoveNode(n) => Op::RemoveNode(other(n)),
        Op::Progress => Op::Progress,
        Op::SetNodeActive(n, a) => Op::SetNodeActive(other(n), a),
        Op::SetEdgeEnabled(u, v, e) => Op::SetEdgeEnabled(u, other(v), e),
    }
}

/// Source-managed actions the driver chooses from.
#[derive(Clone, Copy, Debug)]
enum Action {
    Register,
    Heartbeat,
    BumpEpoch,
    Takeover,
    StaleRegister,
    LeaseExpiry,
    CreateOnly,
    Replace,
    ReplaceStale,
    Deliver,
    SourceAdvance,
    Duplicates,
    EvictedDuplicate,
    Gap,
    DomainFault,
    ConflictingDuplicate,
    PredecessorMismatch,
    StaleGeneration,
    StaleSession,
    SourceMismatch,
    SourceEpochReset,
    StaleInstance,
    LifecycleDisabled,
    Barrier,
    BadBarrier,
    DeclareGap,
    Query,
    QueryBatch,
    DirectMutation,
    Restart,
}

/// Variants of an events request.
#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    DuplicatesOnly,
    Evicted,
    Gap,
    Domain,
    Conflicting,
    Predecessor,
    StaleGeneration,
    StaleSession,
    SourceId,
    SourceEpoch,
    StaleInstance,
    LifecycleDisabled,
}

/// Variants of an import request.
#[derive(Clone, Copy, Debug)]
enum ImportFault {
    None,
    StaleIfMatch,
}

/// Variants of a barrier request.
#[derive(Clone, Copy, Debug)]
enum BarrierKind {
    Exact,
    Premature,
    Superseded,
    Position,
    StaleEpoch,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn env_u64(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let raw = raw.trim();
    let parsed = match raw.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => raw.parse(),
    };
    Some(parsed.unwrap_or_else(|_| panic!("{name} must be an unsigned integer, got {raw:?}")))
}

async fn run(spec: RunSpec) {
    Driver::new(spec).await.run().await;
}

/// Short variant for CI: fixed seeds, lifecycle support off (all engines).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_short_lifecycle_off() {
    for seed in [0x5eed_0001_u64, 0x5eed_0002] {
        run(RunSpec {
            seed,
            steps: 1_500,
            max_duration: Duration::from_secs(120),
            lifecycle: false,
            lease_expiries: 3,
            trace: false,
        })
        .await;
    }
}

/// Short variant for CI: fixed seeds, lifecycle support on (`compact-bfs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_short_lifecycle_on() {
    for seed in [0x5eed_1001_u64, 0x5eed_1002] {
        run(RunSpec {
            seed,
            steps: 1_500,
            max_duration: Duration::from_secs(120),
            lifecycle: true,
            lease_expiries: 3,
            trace: false,
        })
        .await;
    }
}

/// Long variant, configured by `KNOTREL_SOAK_*` (see the crate documentation).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "long-running soak; run manually or on the release commit"]
async fn soak_long() {
    let seed = env_u64("KNOTREL_SOAK_SEED").unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5eed)
    });
    let lifecycle = match std::env::var("KNOTREL_SOAK_LIFECYCLE").ok().as_deref() {
        Some("on" | "true" | "1") => true,
        Some("off" | "false" | "0") => false,
        Some(other) => panic!("KNOTREL_SOAK_LIFECYCLE must be on or off, got {other:?}"),
        None => Rng(seed ^ 0x6c69_6665).next_u64().is_multiple_of(2),
    };
    run(RunSpec {
        seed,
        steps: env_u64("KNOTREL_SOAK_STEPS").unwrap_or(50_000),
        max_duration: Duration::from_secs(env_u64("KNOTREL_SOAK_SECONDS").unwrap_or(600)),
        lifecycle,
        lease_expiries: env_u64("KNOTREL_SOAK_LEASE_EXPIRIES").unwrap_or(4) as u32,
        trace: std::env::var_os("KNOTREL_SOAK_TRACE").is_some(),
    })
    .await;
}

/// Regression test for defect D1, found by the soak (seed `0x2`, step 8): an
/// event with `delivery_sequence` 0 while nothing is applied in the
/// generation must be rejected with 409 `unverifiable_duplicate`
/// (`docs/http-api.md`, invariant 7; `docs/integration-contract.md` §9.2).
/// Before the fix it reached an `unreachable!()` in the graph worker, which
/// marked the server permanently `unavailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regression_d1_sequence_zero_at_generation_start() {
    let server = Server::spawn(ServerSpec {
        managed: true,
        engine: "compact-bfs",
        window: 8,
        lifecycle: false,
    })
    .await;
    let send = |method: &'static str, path: &'static str, body: Value, instance: String| {
        let client = &server.client;
        async move {
            let headers = [("X-Knotrel-Instance-Id", instance.as_str())];
            client
                .raw_request(method, path, &headers, &body.to_string())
                .await
                .expect("transport")
        }
    };
    let info = server
        .client
        .raw_request("GET", "/v1/info", &[], "")
        .await
        .expect("transport")
        .json();
    let instance = info["instance_id"]
        .as_str()
        .expect("instance_id")
        .to_string();
    let session = json!({
        "instance_id": instance, "feeder_session_id": "f", "feeder_epoch": 1,
        "lease_duration_ms": LONG_LEASE_MS,
    });
    assert_eq!(
        send("POST", "/v1/feeder/session", session, instance.clone())
            .await
            .status,
        200
    );
    let import = json!({
        "instance_id": instance, "graph_id": "default", "mode": "create_only",
        "feeder_session_id": "f", "feeder_epoch": 1, "source_id": "s",
        "source_epoch": "e", "source_position": "p0", "nodes": [], "edges": [],
    });
    assert_eq!(
        send("POST", "/v1/feeder/import", import, instance.clone())
            .await
            .status,
        201
    );
    let events = json!({
        "instance_id": instance, "graph_id": "default", "generation": 1,
        "feeder_session_id": "f", "feeder_epoch": 1, "source_id": "s",
        "source_epoch": "e",
        "events": [{
            "event_id": "e0", "delivery_sequence": 0, "prev_delivery_sequence": 0,
            "source_position": "p1", "prev_source_position": "p0", "op": "progress",
        }],
    });
    let reply = send("POST", "/v1/feeder/events", events, instance.clone()).await;
    let ready = server
        .client
        .raw_request("GET", "/ready", &[], "")
        .await
        .expect("transport");
    assert_eq!(
        (reply.status, reply.json()["code"].clone()),
        (409, json!("unverifiable_duplicate")),
        "events reply: {}",
        reply.body
    );
    assert_eq!(
        ready.json()["status"],
        "catching_up",
        "ready: {}",
        ready.body
    );
}
