//! Common external source adapter, oracle, and client for Feature 0.2.6.
//!
//! Provides:
//! - Deterministic authoritative source fixture representing an external database log.
//! - Independent graph oracle maintaining connectivity state without `knotrel_core` or Axum.
//! - Protocol-compliant external feeder client managing discovery, sessions, heartbeats, imports,
//!   event streaming, barriers, gaps, and error recovery.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener as TokioTcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

/// A coherent baseline graph snapshot at source position $S$.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Opaque source position at which this snapshot was taken.
    pub position: String,
    /// List of initial node IDs.
    pub nodes: Vec<u64>,
    /// List of initial undirected edges as `(u, v)` pairs.
    pub edges: Vec<(u64, u64)>,
}

/// An incremental source mutation or progress record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceEvent {
    /// Unique event identifier for deduplication tracking.
    pub event_id: String,
    /// Strictly increasing delivery sequence ($D$).
    pub delivery_sequence: u64,
    /// Delivery sequence of immediate predecessor ($D_{prev}$).
    pub prev_delivery_sequence: u64,
    /// Opaque source position cursor ($P$).
    pub source_position: String,
    /// Opaque predecessor source position cursor ($P_{prev}$).
    pub prev_source_position: String,
    /// Canonical operation: "link", "cut", "add_node", or "progress".
    pub op: String,
    /// Source endpoint for link or cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<u64>,
    /// Target endpoint for link or cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u64>,
    /// Node identifier for add_node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<u64>,
}

/// Deterministic external source fixture modeling an authoritative external database.
#[derive(Debug, Clone)]
pub struct ExternalSourceFixture {
    /// External authoritative store identifier.
    pub source_id: String,
    /// External timeline or failover epoch identifier.
    pub source_epoch: String,
    /// Baseline snapshot at position $S$.
    pub snapshot: SourceSnapshot,
    /// Ordered event stream strictly after position $S$.
    pub events: Vec<SourceEvent>,
    /// Primary high-water mark barrier ($D_{barrier}, P_{barrier}$).
    pub high_water_mark: (u64, String),
}

impl ExternalSourceFixture {
    /// Constructs the standard curated fixture exercising all required protocol features:
    /// - Explicit isolated nodes (e.g. 99, 100).
    /// - Explicit links and cuts.
    /// - An operation producing no topological change (no-op duplicate link).
    /// - An explicit progress-only source record (`op: "progress"`).
    /// - Explicit barrier high-water mark.
    pub fn standard() -> Self {
        let snapshot = SourceSnapshot {
            position: "pos-0100-snapshot".to_string(),
            nodes: vec![1, 2, 3, 4, 5, 10, 20, 99, 100],
            edges: vec![(1, 2), (2, 3), (3, 4), (4, 5), (10, 20)],
        };

        let events = vec![
            // 1. Link connecting component {1..5} to component {10, 20}
            SourceEvent {
                event_id: "evt-0101".to_string(),
                delivery_sequence: 1,
                prev_delivery_sequence: 0,
                source_position: "pos-0101".to_string(),
                prev_source_position: "pos-0100-snapshot".to_string(),
                op: "link".to_string(),
                source: Some(1),
                target: Some(10),
                node: None,
            },
            // 2. Add explicit isolated node 101
            SourceEvent {
                event_id: "evt-0102".to_string(),
                delivery_sequence: 2,
                prev_delivery_sequence: 1,
                source_position: "pos-0102".to_string(),
                prev_source_position: "pos-0101".to_string(),
                op: "add_node".to_string(),
                source: None,
                target: None,
                node: Some(101),
            },
            // 3. Link previously isolated node 100 to newly added node 101
            SourceEvent {
                event_id: "evt-0103".to_string(),
                delivery_sequence: 3,
                prev_delivery_sequence: 2,
                source_position: "pos-0103".to_string(),
                prev_source_position: "pos-0102".to_string(),
                op: "link".to_string(),
                source: Some(100),
                target: Some(101),
                node: None,
            },
            // 4. Cut bridge between 2 and 3
            SourceEvent {
                event_id: "evt-0104".to_string(),
                delivery_sequence: 4,
                prev_delivery_sequence: 3,
                source_position: "pos-0104".to_string(),
                prev_source_position: "pos-0103".to_string(),
                op: "cut".to_string(),
                source: Some(2),
                target: Some(3),
                node: None,
            },
            // 5. Operation that produces NO topological change (linking (1, 2) which already exists)
            SourceEvent {
                event_id: "evt-0105-noop".to_string(),
                delivery_sequence: 5,
                prev_delivery_sequence: 4,
                source_position: "pos-0105".to_string(),
                prev_source_position: "pos-0104".to_string(),
                op: "link".to_string(),
                source: Some(1),
                target: Some(2),
                node: None,
            },
            // 6. Explicit progress-only source record (non-mutating transaction at source)
            SourceEvent {
                event_id: "evt-0106-prog".to_string(),
                delivery_sequence: 6,
                prev_delivery_sequence: 5,
                source_position: "pos-0106".to_string(),
                prev_source_position: "pos-0105".to_string(),
                op: "progress".to_string(),
                source: None,
                target: None,
                node: None,
            },
            // 7. Link node 3 to isolated node 99
            SourceEvent {
                event_id: "evt-0107".to_string(),
                delivery_sequence: 7,
                prev_delivery_sequence: 6,
                source_position: "pos-0107-b1".to_string(),
                prev_source_position: "pos-0106".to_string(),
                op: "link".to_string(),
                source: Some(3),
                target: Some(99),
                node: None,
            },
        ];

        Self {
            source_id: "sql-primary-authoritative".to_string(),
            source_epoch: "epoch-1".to_string(),
            snapshot,
            events,
            high_water_mark: (7, "pos-0107-b1".to_string()),
        }
    }

    /// Generates a deterministic synthetic workload of specified size for reproducible measurements.
    pub fn synthetic(node_count: usize, edge_count: usize, event_count: usize, seed: u64) -> Self {
        // Simple linear congruential pseudo-random generator for 100% deterministic reproducibility
        let mut rng = seed;
        let mut next_rand = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };

        let mut nodes = Vec::with_capacity(node_count);
        for i in 1..=node_count {
            nodes.push(i as u64);
        }

        let mut edges = Vec::with_capacity(edge_count);
        let mut edge_set = BTreeSet::new();
        while edges.len() < edge_count && node_count > 1 {
            let u = (next_rand() % (node_count as u64)) + 1;
            let v = (next_rand() % (node_count as u64)) + 1;
            if u != v {
                let pair = if u < v { (u, v) } else { (v, u) };
                if edge_set.insert(pair) {
                    edges.push(pair);
                }
            }
        }

        let snapshot = SourceSnapshot {
            position: "synth-pos-0000".to_string(),
            nodes,
            edges: edges.clone(),
        };

        let mut events = Vec::with_capacity(event_count);
        let mut prev_pos = "synth-pos-0000".to_string();

        for i in 1..=event_count {
            let seq = i as u64;
            let cur_pos = format!("synth-pos-{seq:04}");
            let op_type = next_rand() % 10;

            let event = if op_type < 5 {
                // Link
                let u = (next_rand() % (node_count as u64)) + 1;
                let v = (next_rand() % (node_count as u64)) + 1;
                let (u, v) = if u == v {
                    (u, if u < node_count as u64 { u + 1 } else { 1 })
                } else {
                    (u, v)
                };
                SourceEvent {
                    event_id: format!("synth-evt-{seq:04}"),
                    delivery_sequence: seq,
                    prev_delivery_sequence: seq - 1,
                    source_position: cur_pos.clone(),
                    prev_source_position: prev_pos.clone(),
                    op: "link".to_string(),
                    source: Some(u),
                    target: Some(v),
                    node: None,
                }
            } else if op_type < 7 && !edges.is_empty() {
                // Cut
                let idx = (next_rand() as usize) % edges.len();
                let (u, v) = edges[idx];
                SourceEvent {
                    event_id: format!("synth-evt-{seq:04}"),
                    delivery_sequence: seq,
                    prev_delivery_sequence: seq - 1,
                    source_position: cur_pos.clone(),
                    prev_source_position: prev_pos.clone(),
                    op: "cut".to_string(),
                    source: Some(u),
                    target: Some(v),
                    node: None,
                }
            } else if op_type == 7 {
                // Add isolated node
                let new_node = (node_count as u64) + seq;
                SourceEvent {
                    event_id: format!("synth-evt-{seq:04}"),
                    delivery_sequence: seq,
                    prev_delivery_sequence: seq - 1,
                    source_position: cur_pos.clone(),
                    prev_source_position: prev_pos.clone(),
                    op: "add_node".to_string(),
                    source: None,
                    target: None,
                    node: Some(new_node),
                }
            } else if op_type == 8 {
                // No-op link (pick existing edge)
                let (u, v) = if !edges.is_empty() { edges[0] } else { (1, 2) };
                SourceEvent {
                    event_id: format!("synth-evt-{seq:04}"),
                    delivery_sequence: seq,
                    prev_delivery_sequence: seq - 1,
                    source_position: cur_pos.clone(),
                    prev_source_position: prev_pos.clone(),
                    op: "link".to_string(),
                    source: Some(u),
                    target: Some(v),
                    node: None,
                }
            } else {
                // Progress
                SourceEvent {
                    event_id: format!("synth-evt-{seq:04}"),
                    delivery_sequence: seq,
                    prev_delivery_sequence: seq - 1,
                    source_position: cur_pos.clone(),
                    prev_source_position: prev_pos.clone(),
                    op: "progress".to_string(),
                    source: None,
                    target: None,
                    node: None,
                }
            };

            prev_pos = cur_pos;
            events.push(event);
        }

        let hwm = (event_count as u64, prev_pos);

        Self {
            source_id: "synthetic-source".to_string(),
            source_epoch: format!("epoch-{seed}"),
            snapshot,
            events,
            high_water_mark: hwm,
        }
    }
}

/// Independent ground-truth oracle computing undirected connectivity directly from external records.
///
/// Implements standard Breadth-First Search (BFS) using standard library collections.
/// Contains zero dependencies on `knotrel-core` or `knotrel-server`.
#[derive(Debug, Clone)]
pub struct ExternalSourceOracle {
    nodes: BTreeSet<u64>,
    edges: BTreeSet<(u64, u64)>,
    adjacency: HashMap<u64, BTreeSet<u64>>,
    applied_sequence: u64,
    applied_position: String,
}

impl ExternalSourceOracle {
    /// Creates an empty oracle.
    pub fn new() -> Self {
        Self {
            nodes: BTreeSet::new(),
            edges: BTreeSet::new(),
            adjacency: HashMap::new(),
            applied_sequence: 0,
            applied_position: String::new(),
        }
    }

    /// Rebuilds oracle state from an authoritative snapshot.
    pub fn load_snapshot(&mut self, snapshot: &SourceSnapshot) {
        self.nodes.clear();
        self.edges.clear();
        self.adjacency.clear();

        for &n in &snapshot.nodes {
            self.nodes.insert(n);
            self.adjacency.entry(n).or_default();
        }

        for &(u, v) in &snapshot.edges {
            self.nodes.insert(u);
            self.nodes.insert(v);
            let pair = if u < v { (u, v) } else { (v, u) };
            self.edges.insert(pair);
            self.adjacency.entry(u).or_default().insert(v);
            self.adjacency.entry(v).or_default().insert(u);
        }

        self.applied_sequence = 0;
        self.applied_position = snapshot.position.clone();
    }

    /// Applies an ordered source event to the oracle state.
    pub fn apply_event(&mut self, event: &SourceEvent) {
        match event.op.as_str() {
            "add_node" => {
                if let Some(n) = event.node {
                    self.nodes.insert(n);
                    self.adjacency.entry(n).or_default();
                }
            }
            "link" => {
                if let (Some(u), Some(v)) = (event.source, event.target) {
                    self.nodes.insert(u);
                    self.nodes.insert(v);
                    let pair = if u < v { (u, v) } else { (v, u) };
                    self.edges.insert(pair);
                    self.adjacency.entry(u).or_default().insert(v);
                    self.adjacency.entry(v).or_default().insert(u);
                }
            }
            "cut" => {
                if let (Some(u), Some(v)) = (event.source, event.target) {
                    let pair = if u < v { (u, v) } else { (v, u) };
                    self.edges.remove(&pair);
                    if let Some(neighbors) = self.adjacency.get_mut(&u) {
                        neighbors.remove(&v);
                    }
                    if let Some(neighbors) = self.adjacency.get_mut(&v) {
                        neighbors.remove(&u);
                    }
                }
            }
            "progress" => {
                // Non-mutating progress: topology remains completely unchanged.
            }
            _ => panic!("unknown oracle event operation: {}", event.op),
        }

        self.applied_sequence = event.delivery_sequence;
        self.applied_position = event.source_position.clone();
    }

    /// Evaluates exact undirected connectivity between `source` and `target` using BFS.
    pub fn is_connected(&self, source: u64, target: u64) -> bool {
        if source == target {
            return self.nodes.contains(&source);
        }
        if !self.nodes.contains(&source) || !self.nodes.contains(&target) {
            return false;
        }

        let mut visited = BTreeSet::new();
        let mut queue = std::collections::VecDeque::new();

        visited.insert(source);
        queue.push_back(source);

        while let Some(current) = queue.pop_front() {
            if current == target {
                return true;
            }
            if let Some(neighbors) = self.adjacency.get(&current) {
                for &neighbor in neighbors {
                    if visited.insert(neighbor) {
                        queue.push_back(neighbor);
                    }
                }
            }
        }

        false
    }

    /// Number of distinct nodes in the oracle.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of distinct undirected edges in the oracle.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Current applied delivery sequence.
    pub fn applied_delivery_sequence(&self) -> u64 {
        self.applied_sequence
    }

    /// Current applied source position cursor.
    pub fn applied_source_position(&self) -> &str {
        &self.applied_position
    }
}

impl Default for ExternalSourceOracle {
    fn default() -> Self {
        Self::new()
    }
}

/// Raw HTTP response representation.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: String,
    /// Response body as UTF-8 string.
    pub body: String,
}

impl HttpResponse {
    /// Parses the body as a JSON Value.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| {
            panic!(
                "expected JSON body, got status {} with body: {}",
                self.status, self.body
            )
        })
    }

    /// Searches for a response header by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim())
            } else {
                None
            }
        })
    }
}

/// Errors occurring during external feeder execution.
#[derive(Debug)]
pub enum FeederClientError {
    /// IO or network failure.
    Io(std::io::Error),
    /// HTTP status error from server.
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
    },
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// Protocol violation or precondition failure.
    Protocol(String),
    /// Feeder was fenced by the server or a competing session.
    Fenced(String),
    /// Server instance restarted, invalidating ephemeral session.
    StaleInstance(String),
    /// Feeder lease has expired on the server.
    LeaseExpired(String),
    /// Persistent connection or transport failure over repeated attempts.
    PersistentTransport(String),
    /// Explicit recovery is required (e.g. uncertain outcome across restart, takeover, or eviction).
    RecoveryRequired(String),
    /// Uncertain outcome where evidence is ambiguous and cannot be verified without authoritative rebuild.
    UncertainOutcome(String),
}

impl std::fmt::Display for FeederClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Http { status, body } => write!(f, "HTTP error {status}: {body}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Protocol(s) => write!(f, "Protocol error: {s}"),
            Self::Fenced(s) => write!(f, "Feeder fenced: {s}"),
            Self::StaleInstance(s) => write!(f, "Stale instance: {s}"),
            Self::LeaseExpired(s) => write!(f, "Lease expired: {s}"),
            Self::PersistentTransport(s) => write!(f, "Persistent transport error: {s}"),
            Self::RecoveryRequired(s) => write!(f, "Recovery required: {s}"),
            Self::UncertainOutcome(s) => write!(f, "Uncertain outcome: {s}"),
        }
    }
}

impl std::error::Error for FeederClientError {}

impl From<std::io::Error> for FeederClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for FeederClientError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Observable status of the feeder's background heartbeat loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatStatus {
    /// Actively running and maintaining valid unexpired lease.
    Healthy,
    /// Feeder was fenced by the server (e.g. 409 fenced_feeder_session).
    Fenced(String),
    /// Server incarnation changed / restarted (e.g. 409 stale_instance_id).
    StaleInstance(String),
    /// Feeder lease has expired on the server (e.g. 409 lease_expired).
    LeaseExpired(String),
    /// Server reported staleness (e.g. status: "stale").
    Stale(String),
    /// Persistent connection or IO failure over multiple consecutive attempts.
    PersistentTransportError(String),
    /// Cleanly stopped by caller.
    Stopped,
}

/// Structured partial failure outcome returned when a domain error stops batch processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchPartialFailure {
    /// 0-indexed position of failing event in the submitted batch.
    pub failed_batch_index: usize,
    /// Delivery sequence of failing event.
    pub failed_delivery_sequence: u64,
    /// Event ID of failing event.
    pub failed_event_id: String,
    /// Delivery sequence of highest successfully applied predecessor.
    pub applied_delivery_sequence: u64,
    /// Opaque source position cursor of highest successfully applied predecessor.
    pub applied_source_position: String,
    /// Canonical error code from server.
    pub error_code: String,
    /// Human-readable error message.
    pub error_message: String,
}

/// Controlled fault injection proxy for simulating network anomalies (dropped responses, dropped requests, and controlled in-flight pause).
type PauseHook = Arc<
    tokio::sync::Mutex<
        Option<(
            &'static str,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        )>,
    >,
>;

type ChainedDropPause = Arc<
    tokio::sync::Mutex<
        Option<(
            &'static str,
            Arc<tokio::sync::Notify>,
            &'static str,
            Arc<tokio::sync::Notify>,
            Arc<tokio::sync::Notify>,
        )>,
    >,
>;

pub struct ControlledFaultProxy {
    listen_addr: SocketAddr,
    target_addr: SocketAddr,
    drop_next_response: Arc<AtomicBool>,
    drop_next_request: Arc<AtomicBool>,
    drop_resp_pattern: Arc<tokio::sync::Mutex<Option<&'static str>>>,
    drop_req_pattern: Arc<tokio::sync::Mutex<Option<&'static str>>>,
    drop_req_notify: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Notify>>>>,
    chained_drop_pause: ChainedDropPause,
    dropped_responses: Arc<AtomicUsize>,
    dropped_requests: Arc<AtomicUsize>,
    pause_hook: PauseHook,
    shutdown_tx: Option<watch::Sender<bool>>,
    task_handle: Option<JoinHandle<()>>,
}

impl ControlledFaultProxy {
    /// Starts a TCP proxy listening on a free loopback port and forwarding to `target_addr`.
    pub async fn start(target_addr: SocketAddr) -> Result<Self, std::io::Error> {
        let listener = TokioTcpListener::bind("127.0.0.1:0").await?;
        let listen_addr = listener.local_addr()?;
        let drop_next_response = Arc::new(AtomicBool::new(false));
        let drop_next_request = Arc::new(AtomicBool::new(false));
        let drop_resp_pattern = Arc::new(tokio::sync::Mutex::new(None));
        let drop_req_pattern = Arc::new(tokio::sync::Mutex::new(None));
        let drop_req_notify: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Notify>>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let chained_drop_pause: ChainedDropPause = Arc::new(tokio::sync::Mutex::new(None));
        let dropped_responses = Arc::new(AtomicUsize::new(0));
        let dropped_requests = Arc::new(AtomicUsize::new(0));
        let pause_hook: PauseHook = Arc::new(tokio::sync::Mutex::new(None));
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let drop_flag = drop_next_response.clone();
        let drop_req_flag = drop_next_request.clone();
        let drop_resp_pat_clone = drop_resp_pattern.clone();
        let drop_req_pat_clone = drop_req_pattern.clone();
        let drop_req_notify_clone = drop_req_notify.clone();
        let chained_drop_pause_clone = chained_drop_pause.clone();
        let drop_count = dropped_responses.clone();
        let drop_req_count = dropped_requests.clone();
        let pause_hook_clone = pause_hook.clone();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accept_res = listener.accept() => {
                        let Ok((mut client_stream, _)) = accept_res else {
                            continue;
                        };
                        let target = target_addr;
                        let drop_flag_clone = drop_flag.clone();
                        let drop_req_clone = drop_req_flag.clone();
                        let drop_resp_pat_task = drop_resp_pat_clone.clone();
                        let drop_req_pat_task = drop_req_pat_clone.clone();
                        let drop_req_notify_task = drop_req_notify_clone.clone();
                        let drop_count_clone = drop_count.clone();
                        let drop_req_count_clone = drop_req_count.clone();
                        let pause_hook_task = pause_hook_clone.clone();
                        let chained_drop_pause_task = chained_drop_pause_clone.clone();

                        tokio::spawn(async move {
                            let mut initial_buf = [0_u8; 1024];
                            let n = match client_stream.read(&mut initial_buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => n,
                            };
                            let initial_data = &initial_buf[..n];
                            let req_text = String::from_utf8_lossy(initial_data);

                            let chained_action = {
                                let mut chained_lock = chained_drop_pause_task.lock().await;
                                if let Some((drop_pat, _, _, _, _)) = &*chained_lock {
                                    if req_text.contains(drop_pat) {
                                        chained_lock.take()
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            };

                            if let Some((_, dropped_notify, pause_pat, pause_started, pause_release)) = chained_action {
                                drop_req_count_clone.fetch_add(1, Ordering::SeqCst);
                                {
                                    let mut hook_lock = pause_hook_task.lock().await;
                                    *hook_lock = Some((pause_pat, pause_started, pause_release));
                                }
                                dropped_notify.notify_one();
                                let _ = client_stream.shutdown().await;
                                return;
                            }

                            let should_drop_req = {
                                if drop_req_clone.load(Ordering::SeqCst) {
                                    let mut req_pat = drop_req_pat_task.lock().await;
                                    if let Some(pat) = *req_pat {
                                        if req_text.contains(pat) {
                                            *req_pat = None;
                                            drop_req_clone.store(false, Ordering::SeqCst);
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        drop_req_clone.store(false, Ordering::SeqCst);
                                        true
                                    }
                                } else {
                                    false
                                }
                            };

                            if should_drop_req {
                                drop_req_count_clone.fetch_add(1, Ordering::SeqCst);
                                {
                                    let mut lock = drop_req_notify_task.lock().await;
                                    if let Some(n) = lock.take() {
                                        n.notify_one();
                                    }
                                }
                                let _ = client_stream.shutdown().await;
                                return;
                            }

                            let hook_opt = {
                                let mut lock = pause_hook_task.lock().await;
                                if let Some((pattern, _, _)) = &*lock {
                                    if req_text.contains(pattern) {
                                        lock.take()
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            };

                            if let Some((_, started, release)) = hook_opt {
                                started.notify_one();
                                release.notified().await;
                            }

                            let Ok(mut server_stream) = TcpStream::connect(target).await else {
                                return;
                            };

                            let should_drop_resp = {
                                if drop_flag_clone.load(Ordering::SeqCst) {
                                    let mut resp_pat = drop_resp_pat_task.lock().await;
                                    if let Some(pat) = *resp_pat {
                                        if req_text.contains(pat) {
                                            *resp_pat = None;
                                            drop_flag_clone.store(false, Ordering::SeqCst);
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        drop_flag_clone.store(false, Ordering::SeqCst);
                                        true
                                    }
                                } else {
                                    false
                                }
                            };

                            if should_drop_resp {
                                let mut req_buf = Vec::new();
                                req_buf.extend_from_slice(initial_data);
                                let _ = server_stream.write_all(initial_data).await;

                                loop {
                                    if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                        let headers_part = String::from_utf8_lossy(&req_buf[..pos]);
                                        let cl = headers_part.lines().find_map(|l| {
                                            let (k, v) = l.split_once(':')?;
                                            if k.trim().eq_ignore_ascii_case("content-length") {
                                                v.trim().parse::<usize>().ok()
                                            } else {
                                                None
                                            }
                                        }).unwrap_or(0);
                                        let body_received = req_buf.len() - (pos + 4);
                                        if body_received >= cl {
                                            break;
                                        }
                                    }

                                    let mut temp = [0_u8; 4096];
                                    match client_stream.read(&mut temp).await {
                                        Ok(0) | Err(_) => break,
                                        Ok(bytes_read) => {
                                             req_buf.extend_from_slice(&temp[..bytes_read]);
                                            let _ = server_stream.write_all(&temp[..bytes_read]).await;
                                        }
                                    }
                                }
                                let _ = server_stream.flush().await;

                                // Wait for server to process and produce HTTP response
                                let mut srv_resp = [0_u8; 4096];
                                let _ = server_stream.read(&mut srv_resp).await;

                                // Drop response and abruptly close client connection!
                                drop_count_clone.fetch_add(1, Ordering::SeqCst);
                                let _ = client_stream.shutdown().await;
                                drop(client_stream);
                            } else {
                                if server_stream.write_all(initial_data).await.is_ok() {
                                    let _ = tokio::io::copy_bidirectional(&mut client_stream, &mut server_stream).await;
                                }
                            }
                        });
                    }
                }
            }
        });

        Ok(Self {
            listen_addr,
            target_addr,
            drop_next_response,
            drop_next_request,
            drop_resp_pattern,
            drop_req_pattern,
            drop_req_notify,
            chained_drop_pause,
            dropped_responses,
            dropped_requests,
            pause_hook,
            shutdown_tx: Some(shutdown_tx),
            task_handle: Some(task),
        })
    }

    /// Returns the listening socket address of this proxy.
    pub fn address(&self) -> SocketAddr {
        self.listen_addr
    }

    /// Returns the listening socket address of this proxy.
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// Arms fault injection: the next HTTP request forwarded by this proxy will be delivered to
    /// the server, but the server's response will be dropped and the client connection abruptly closed.
    pub fn arm_drop_next_response(&self) {
        self.drop_next_response.store(true, Ordering::SeqCst);
    }

    /// Arms fault injection: the next HTTP request matching `pattern` will have its response dropped.
    pub async fn arm_drop_response_matching(&self, pattern: &'static str) {
        let mut lock = self.drop_resp_pattern.lock().await;
        *lock = Some(pattern);
        self.drop_next_response.store(true, Ordering::SeqCst);
    }

    /// Arms fault injection: the next HTTP request will be dropped before delivery to the server.
    pub fn arm_drop_next_request(&self) {
        self.drop_next_request.store(true, Ordering::SeqCst);
    }

    /// Arms fault injection: the next HTTP request matching `pattern` will be dropped before delivery to server.
    pub async fn arm_drop_request_matching(&self, pattern: &'static str) {
        let mut lock = self.drop_req_pattern.lock().await;
        *lock = Some(pattern);
        self.drop_next_request.store(true, Ordering::SeqCst);
    }

    /// Arms fault injection: the next HTTP request matching `pattern` will be dropped before delivery to server,
    /// and `notify` will be notified when the drop occurs.
    pub async fn arm_drop_request_matching_with_notify(
        &self,
        pattern: &'static str,
        notify: Arc<tokio::sync::Notify>,
    ) {
        let mut lock = self.drop_req_pattern.lock().await;
        *lock = Some(pattern);
        let mut nlock = self.drop_req_notify.lock().await;
        *nlock = Some(notify);
        self.drop_next_request.store(true, Ordering::SeqCst);
    }

    /// Atomically arms a fault that drops the next request matching `drop_pattern` before delivery
    /// to the server, and atomically installs an in-flight pause gate for the subsequent request
    /// matching `pause_pattern`.
    ///
    /// This prevents race conditions between dropping an initial request (e.g. an import) and
    /// pausing a subsequent request (e.g. reconciliation stats), while ensuring earlier requests
    /// (e.g. pre-import baseline stats) are not paused or dropped.
    ///
    /// Returns `(dropped_notify, pause_started, pause_release)`:
    /// - `dropped_notify` signals when the dropped request has been intercepted and closed.
    /// - `pause_started` signals when the subsequent request is in flight and held at the proxy gate.
    /// - `pause_release` can be notified to release the paused request and resume forwarding.
    pub async fn arm_drop_request_and_pause_subsequent(
        &self,
        drop_pattern: &'static str,
        pause_pattern: &'static str,
    ) -> (
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let dropped_notify = Arc::new(tokio::sync::Notify::new());
        let pause_started = Arc::new(tokio::sync::Notify::new());
        let pause_release = Arc::new(tokio::sync::Notify::new());

        let mut lock = self.chained_drop_pause.lock().await;
        *lock = Some((
            drop_pattern,
            dropped_notify.clone(),
            pause_pattern,
            pause_started.clone(),
            pause_release.clone(),
        ));

        (dropped_notify, pause_started, pause_release)
    }

    /// Arms a synchronization hook that pauses forwarding when a request matching `pattern` is received.
    /// Returns `(started_notify, release_notify)`:
    /// - `started_notify` triggers when the request is in flight and paused.
    /// - `release_notify.notify_one()` releases the proxy to continue forwarding.
    pub async fn arm_pause_before_forwarding(
        &self,
        pattern: &'static str,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut lock = self.pause_hook.lock().await;
        *lock = Some((pattern, started.clone(), release.clone()));
        (started, release)
    }

    /// Returns the number of responses dropped by this proxy.
    pub fn dropped_response_count(&self) -> usize {
        self.dropped_responses.load(Ordering::SeqCst)
    }

    /// Returns the number of requests dropped before delivery to the server.
    pub fn dropped_request_count(&self) -> usize {
        self.dropped_requests.load(Ordering::SeqCst)
    }
}

impl Drop for ControlledFaultProxy {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(h) = self.task_handle.take() {
            h.abort();
        }
        if let Ok(mut lock) = self.pause_hook.try_lock()
            && let Some((_, _, release)) = lock.take()
        {
            release.notify_waiters();
        }
        if let Ok(mut lock) = self.chained_drop_pause.try_lock()
            && let Some((_, _, _, _, release)) = lock.take()
        {
            release.notify_waiters();
        }
    }
}

/// External feeder client implementing the complete Knotrel source-managed integration lifecycle.
pub struct ExternalFeederClient {
    /// Address of target Knotrel server.
    pub address: SocketAddr,
    /// Discovered server instance identifier.
    pub instance_id: String,
    /// Registered feeder session identifier.
    pub feeder_session_id: String,
    /// Active feeder epoch.
    pub feeder_epoch: u64,
    /// Current graph generation.
    pub generation: u64,
    /// Bound source store identifier.
    pub source_id: String,
    /// Bound source epoch identifier.
    pub source_epoch: String,
    /// Last observed state version.
    pub state_version: String,
    /// Last observed applied source position.
    pub applied_source_position: String,
    /// Observable receiver for background heartbeat health status.
    pub heartbeat_status: watch::Receiver<HeartbeatStatus>,
    /// Background heartbeat status sender.
    heartbeat_status_tx: watch::Sender<HeartbeatStatus>,
    /// Background heartbeat stop sender.
    heartbeat_stop: Option<watch::Sender<bool>>,
    /// Background heartbeat task handle.
    heartbeat_task: Option<JoinHandle<()>>,
}

impl ExternalFeederClient {
    /// Creates a new uninitialized feeder client.
    pub fn new(address: SocketAddr, session_id: &str) -> Self {
        let (status_tx, status_rx) = watch::channel(HeartbeatStatus::Healthy);
        Self {
            address,
            instance_id: String::new(),
            feeder_session_id: session_id.to_string(),
            feeder_epoch: 1,
            generation: 0,
            source_id: String::new(),
            source_epoch: String::new(),
            state_version: "0".to_string(),
            applied_source_position: String::new(),
            heartbeat_status: status_rx,
            heartbeat_status_tx: status_tx,
            heartbeat_stop: None,
            heartbeat_task: None,
        }
    }

    /// Sends a low-level HTTP request to the server.
    pub async fn raw_request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<HttpResponse, FeederClientError> {
        let mut stream = TcpStream::connect(self.address).await?;

        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.address);
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
            return Err(FeederClientError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed without HTTP response",
            )));
        }

        let head_end = response_bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| {
                FeederClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "malformed HTTP response: connection closed before header end",
                ))
            })?;

        let headers_str = String::from_utf8_lossy(&response_bytes[..head_end]).to_string();
        let body_str = String::from_utf8_lossy(&response_bytes[head_end + 4..]).to_string();

        let status = headers_str
            .lines()
            .next()
            .and_then(|status_line| status_line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(500);

        Ok(HttpResponse {
            status,
            headers: headers_str,
            body: body_str,
        })
    }

    /// Discovers server metadata via `GET /v1/info` and validates `source-managed` mode.
    pub async fn discover(&mut self) -> Result<String, FeederClientError> {
        let res = self.raw_request("GET", "/v1/info", &[], "").await?;
        if res.status != 200 {
            return Err(FeederClientError::Http {
                status: res.status,
                body: res.body,
            });
        }
        let json = res.json();
        let mode = json["mode"].as_str().unwrap_or_default();
        if mode != "source-managed" && mode != "source_managed" {
            return Err(FeederClientError::Protocol(format!(
                "server mode is '{mode}', expected 'source-managed'"
            )));
        }
        self.instance_id = json["instance_id"]
            .as_str()
            .ok_or_else(|| FeederClientError::Protocol("missing instance_id in /v1/info".into()))?
            .to_string();
        self.generation = json["generation"].as_u64().unwrap_or(0);
        Ok(self.instance_id.clone())
    }

    /// Registers the feeder session with the server via `POST /v1/feeder/session`.
    pub async fn register_session(
        &mut self,
        lease_duration_ms: u64,
    ) -> Result<serde_json::Value, FeederClientError> {
        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch,
            "lease_duration_ms": lease_duration_ms
        })
        .to_string();

        let headers = [("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        let res = self
            .raw_request("POST", "/v1/feeder/session", &headers, &payload)
            .await?;

        if res.status != 200 {
            return Err(FeederClientError::Http {
                status: res.status,
                body: res.body,
            });
        }
        Ok(res.json())
    }

    /// Starts an observable background periodic heartbeat loop with bounded timeout and response validation.
    pub fn start_heartbeat(&mut self, interval: Duration) {
        self.abort_heartbeat();

        let (stop_tx, mut stop_rx) = watch::channel(false);
        let status_tx = self.heartbeat_status_tx.clone();
        let _ = status_tx.send(HeartbeatStatus::Healthy);

        let address = self.address;
        let instance_id = self.instance_id.clone();
        let session_id = self.feeder_session_id.clone();
        let epoch = self.feeder_epoch;

        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick fires immediately
            let mut consecutive_failures = 0_usize;

            loop {
                tokio::select! {
                    _ = stop_rx.changed() => {
                        if *stop_rx.borrow() {
                            let _ = status_tx.send(HeartbeatStatus::Stopped);
                            break;
                        }
                    }
                    _ = ticker.tick() => {
                        let payload = serde_json::json!({
                            "instance_id": instance_id,
                            "feeder_session_id": session_id,
                            "feeder_epoch": epoch
                        }).to_string();

                        let send_fut = async {
                            let mut stream = TcpStream::connect(address).await?;
                            let req = format!(
                                "POST /v1/feeder/heartbeat HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Knotrel-Instance-Id: {instance_id}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            stream.write_all(req.as_bytes()).await?;

                            let mut resp_bytes = Vec::new();
                            stream.read_to_end(&mut resp_bytes).await?;
                            let resp_str = String::from_utf8_lossy(&resp_bytes);
                            let status = resp_str
                                .lines()
                                .next()
                                .and_then(|line| line.split_whitespace().nth(1))
                                .and_then(|c| c.parse::<u16>().ok())
                                .unwrap_or(500);

                            let body = resp_str
                                .split_once("\r\n\r\n")
                                .map(|(_, b)| b)
                                .unwrap_or("");

                            Ok::<_, std::io::Error>((status, body.to_string()))
                        };

                        // Bounded timeout for heartbeat operation (max 2 seconds)
                        match tokio::time::timeout(Duration::from_secs(2), send_fut).await {
                            Ok(Ok((200, body))) => {
                                consecutive_failures = 0;
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                                    if json["status"] == "stale" {
                                        let _ = status_tx.send(HeartbeatStatus::Stale("server sync_status is stale".into()));
                                    } else {
                                        let _ = status_tx.send(HeartbeatStatus::Healthy);
                                    }
                                } else {
                                    let _ = status_tx.send(HeartbeatStatus::Healthy);
                                }
                            }
                            Ok(Ok((409, body))) => {
                                let code = serde_json::from_str::<serde_json::Value>(&body)
                                    .ok()
                                    .and_then(|j| j["code"].as_str().map(|s| s.to_string()))
                                    .unwrap_or_else(|| "conflict".to_string());

                                match code.as_str() {
                                    "fenced_feeder_session" => {
                                        let _ = status_tx.send(HeartbeatStatus::Fenced(body));
                                        break; // Fenced! Stop stale ownership immediately.
                                    }
                                    "stale_instance_id" => {
                                        let _ = status_tx.send(HeartbeatStatus::StaleInstance(body));
                                        break; // Server restarted! Stop stale ownership.
                                    }
                                    "lease_expired" => {
                                        let _ = status_tx.send(HeartbeatStatus::LeaseExpired(body));
                                        break; // Expired! Stop stale ownership.
                                    }
                                    _ => {
                                        let _ = status_tx.send(HeartbeatStatus::Fenced(body));
                                        break;
                                    }
                                }
                            }
                            Ok(Ok((status, body))) => {
                                consecutive_failures += 1;
                                if consecutive_failures >= 3 {
                                    let _ = status_tx.send(HeartbeatStatus::PersistentTransportError(format!(
                                        "HTTP {status}: {body}"
                                    )));
                                    break;
                                }
                            }
                            Ok(Err(_)) | Err(_) => {
                                consecutive_failures += 1;
                                if consecutive_failures >= 3 {
                                    let _ = status_tx.send(HeartbeatStatus::PersistentTransportError(
                                        "3 consecutive heartbeat transport timeouts or connection failures".into()
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        self.heartbeat_stop = Some(stop_tx);
        self.heartbeat_task = Some(task);
    }

    /// Stops the background periodic heartbeat loop, awaiting clean task termination with bounded timeout.
    pub async fn stop_heartbeat(&mut self) {
        if let Some(stop_tx) = self.heartbeat_stop.take() {
            let _ = stop_tx.send(true);
        }
        if let Some(mut task) = self.heartbeat_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
            task.abort();
        }
        let _ = self.heartbeat_status_tx.send(HeartbeatStatus::Stopped);
    }

    /// Immediately aborts background heartbeat task without awaiting.
    pub fn abort_heartbeat(&mut self) {
        if let Some(stop_tx) = self.heartbeat_stop.take() {
            let _ = stop_tx.send(true);
        }
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        let _ = self.heartbeat_status_tx.send(HeartbeatStatus::Stopped);
    }

    /// Returns the current observable heartbeat status.
    pub fn current_heartbeat_status(&self) -> HeartbeatStatus {
        self.heartbeat_status.borrow().clone()
    }

    /// Returns true if the background heartbeat loop is currently running and healthy.
    pub fn is_heartbeat_healthy(&self) -> bool {
        matches!(*self.heartbeat_status.borrow(), HeartbeatStatus::Healthy)
    }

    /// Checks the background heartbeat status and immediately errors on fencing, stale instance,
    /// expired lease, or persistent transport failure.
    pub fn ensure_heartbeat_healthy(&self) -> Result<(), FeederClientError> {
        match self.current_heartbeat_status() {
            HeartbeatStatus::Healthy | HeartbeatStatus::Stopped => Ok(()),
            HeartbeatStatus::Fenced(msg) => Err(FeederClientError::Fenced(msg)),
            HeartbeatStatus::StaleInstance(msg) => Err(FeederClientError::StaleInstance(msg)),
            HeartbeatStatus::LeaseExpired(msg) => Err(FeederClientError::LeaseExpired(msg)),
            HeartbeatStatus::Stale(msg) => Err(FeederClientError::Protocol(format!(
                "heartbeat reported stale: {msg}"
            ))),
            HeartbeatStatus::PersistentTransportError(msg) => {
                Err(FeederClientError::PersistentTransport(msg))
            }
        }
    }

    /// Sends a single heartbeat request via `POST /v1/feeder/heartbeat`.
    pub async fn send_heartbeat(&self) -> Result<HttpResponse, FeederClientError> {
        self.ensure_heartbeat_healthy()?;
        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch
        })
        .to_string();
        let headers = [("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        self.raw_request("POST", "/v1/feeder/heartbeat", &headers, &payload)
            .await
    }

    /// Imports a snapshot baseline into the server via `POST /v1/feeder/import`.
    pub async fn import(
        &mut self,
        mode: &str,
        source_id: &str,
        source_epoch: &str,
        snapshot: &SourceSnapshot,
        if_match: Option<&str>,
    ) -> Result<HttpResponse, FeederClientError> {
        self.ensure_heartbeat_healthy()?;
        if mode == "replace" && if_match.is_none() {
            return Err(FeederClientError::Protocol(
                "mandatory replacement precondition missing: If-Match is required for replace mode"
                    .into(),
            ));
        }
        self.source_id = source_id.to_string();
        self.source_epoch = source_epoch.to_string();

        let nodes_str: Vec<String> = snapshot.nodes.iter().map(|n| n.to_string()).collect();
        let edges_json: Vec<serde_json::Value> = snapshot
            .edges
            .iter()
            .map(|&(u, v)| serde_json::json!({ "source": u.to_string(), "target": v.to_string() }))
            .collect();

        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "graph_id": "default",
            "mode": mode,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch,
            "source_id": source_id,
            "source_epoch": source_epoch,
            "source_position": snapshot.position,
            "nodes": nodes_str,
            "edges": edges_json
        })
        .to_string();

        let mut headers = vec![("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        if let Some(im) = if_match {
            headers.push(("If-Match", im));
        }

        let res = self
            .raw_request("POST", "/v1/feeder/import", &headers, &payload)
            .await?;
        if res.status == 200 || res.status == 201 {
            let json = res.json();
            self.generation = json["generation"].as_u64().unwrap_or(self.generation + 1);
            self.state_version = json["state_version"].as_str().unwrap_or("0").to_string();
            self.applied_source_position = snapshot.position.clone();
        }
        Ok(res)
    }

    /// Submits a batch of ordered events via `POST /v1/feeder/events`.
    pub async fn apply_events(
        &mut self,
        events: &[SourceEvent],
    ) -> Result<HttpResponse, FeederClientError> {
        self.ensure_heartbeat_healthy()?;
        let events_json: Vec<serde_json::Value> = events
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "event_id": e.event_id,
                    "delivery_sequence": e.delivery_sequence,
                    "prev_delivery_sequence": e.prev_delivery_sequence,
                    "source_position": e.source_position,
                    "prev_source_position": e.prev_source_position,
                    "op": e.op
                });
                if let Some(src) = e.source {
                    obj["source"] = serde_json::json!(src.to_string());
                }
                if let Some(tgt) = e.target {
                    obj["target"] = serde_json::json!(tgt.to_string());
                }
                if let Some(n) = e.node {
                    obj["node"] = serde_json::json!(n.to_string());
                }
                obj
            })
            .collect();

        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "graph_id": "default",
            "generation": self.generation,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch,
            "source_id": self.source_id,
            "source_epoch": self.source_epoch,
            "events": events_json
        })
        .to_string();

        let headers = [("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        let res = self
            .raw_request("POST", "/v1/feeder/events", &headers, &payload)
            .await?;

        if res.status == 200 {
            let json = res.json();
            if let Some(pos) = json["applied_source_position"].as_str() {
                self.applied_source_position = pos.to_string();
            }
            if let Some(ver) = json["state_version"].as_str() {
                self.state_version = ver.to_string();
            }
        }
        Ok(res)
    }

    /// Declares synchronization completion at barrier $B$ via `POST /v1/feeder/barrier`.
    pub async fn declare_barrier(
        &self,
        sequence: u64,
        position: &str,
    ) -> Result<HttpResponse, FeederClientError> {
        self.ensure_heartbeat_healthy()?;
        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "graph_id": "default",
            "generation": self.generation,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch,
            "barrier_delivery_sequence": sequence,
            "barrier_source_position": position
        })
        .to_string();

        let headers = [("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        self.raw_request("POST", "/v1/feeder/barrier", &headers, &payload)
            .await
    }

    /// Declares an unrepairable source gap via `POST /v1/feeder/gap`.
    pub async fn declare_gap(
        &self,
        failed_sequence: u64,
        reason: &str,
    ) -> Result<HttpResponse, FeederClientError> {
        let payload = serde_json::json!({
            "instance_id": self.instance_id,
            "graph_id": "default",
            "generation": self.generation,
            "feeder_session_id": self.feeder_session_id,
            "feeder_epoch": self.feeder_epoch,
            "failed_delivery_sequence": failed_sequence,
            "reason": reason
        })
        .to_string();

        let headers = [("X-Knotrel-Instance-Id", self.instance_id.as_str())];
        self.raw_request("POST", "/v1/feeder/gap", &headers, &payload)
            .await
    }

    /// Probes server readiness via `GET /ready`.
    pub async fn get_ready(&self) -> Result<HttpResponse, FeederClientError> {
        self.raw_request("GET", "/ready", &[], "").await
    }

    /// Reads live graph statistics via `GET /v1/stats`.
    pub async fn get_stats(&self) -> Result<HttpResponse, FeederClientError> {
        self.raw_request("GET", "/v1/stats", &[], "").await
    }

    /// Executes an undirected connectivity query via `POST /v1/operations`.
    pub async fn query_connected(
        &self,
        source: u64,
        target: u64,
    ) -> Result<HttpResponse, FeederClientError> {
        let payload = serde_json::json!({
            "op": "connected",
            "source": source.to_string(),
            "target": target.to_string()
        })
        .to_string();

        self.raw_request("POST", "/v1/operations", &[], &payload)
            .await
    }

    /// Parses a 422 partial batch failure response into structured diagnostics.
    pub fn parse_partial_failure(res: &HttpResponse) -> Option<BatchPartialFailure> {
        if res.status != 422 {
            return None;
        }
        let json = res.json();
        if json["result"] != "partial_failure" {
            return None;
        }
        Some(BatchPartialFailure {
            failed_batch_index: json["failed_batch_index"].as_u64()? as usize,
            failed_delivery_sequence: json["failed_delivery_sequence"].as_u64()?,
            failed_event_id: json["failed_event_id"].as_str()?.to_string(),
            applied_delivery_sequence: json["applied_delivery_sequence"].as_u64()?,
            applied_source_position: json["applied_source_position"].as_str()?.to_string(),
            error_code: json["error"]["code"].as_str()?.to_string(),
            error_message: json["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Submits events with bounded retry and exact identity preservation in the presence
    /// of lost acknowledgments or transport errors.
    ///
    /// Rather than synthesizing success from /stats, this method retries the exact same immutable
    /// event payload under the original instance and generation identities. The server's deduplication
    /// window evaluates exact retries:
    /// - If the mutation committed, server returns HTTP 200 `already_applied` (`result: duplicate`).
    /// - If the mutation never reached the server, server executes it and returns HTTP 200 `applied`.
    /// - If the server restarted, server returns HTTP 409 `stale_instance_id`.
    /// - If the graph was replaced, server returns HTTP 409 `stale_generation`.
    /// - If the feeder lease was taken over, server returns HTTP 409 `fenced_feeder_session`.
    /// - If the sequence was evicted from the window, server returns HTTP 409 `unverifiable_duplicate`.
    ///
    /// Any divergence, takeover, eviction, or restart returns an explicit `RecoveryRequired` or `Fenced`
    /// error so the caller can execute safe authoritative rebuild, never fabricating deduplication.
    pub async fn apply_events_with_recovery(
        &mut self,
        events: &[SourceEvent],
        max_retries: usize,
    ) -> Result<HttpResponse, FeederClientError> {
        let mut attempts = 0;
        let mut backoff = Duration::from_millis(15);

        loop {
            self.ensure_heartbeat_healthy()?;
            attempts += 1;

            match self.apply_events(events).await {
                Ok(res) => {
                    if res.status == 200 {
                        return Ok(res);
                    }
                    if res.status == 409 {
                        let json = res.json();
                        let code = json["code"].as_str().unwrap_or("");
                        match code {
                            "stale_instance_id" => {
                                let _ = self
                                    .heartbeat_status_tx
                                    .send(HeartbeatStatus::StaleInstance(res.body.clone()));
                                return Err(FeederClientError::StaleInstance(format!(
                                    "server instance changed during event submission: {}",
                                    res.body
                                )));
                            }
                            "fenced_feeder_session" => {
                                let _ = self
                                    .heartbeat_status_tx
                                    .send(HeartbeatStatus::Fenced(res.body.clone()));
                                return Err(FeederClientError::Fenced(format!(
                                    "feeder session was fenced by server: {}",
                                    res.body
                                )));
                            }
                            "lease_expired" => {
                                let _ = self
                                    .heartbeat_status_tx
                                    .send(HeartbeatStatus::LeaseExpired(res.body.clone()));
                                return Err(FeederClientError::LeaseExpired(format!(
                                    "feeder lease expired on server: {}",
                                    res.body
                                )));
                            }
                            "stale_generation" => {
                                return Err(FeederClientError::RecoveryRequired(format!(
                                    "server generation diverged / replaced: {}",
                                    res.body
                                )));
                            }
                            "unverifiable_duplicate" => {
                                return Err(FeederClientError::RecoveryRequired(format!(
                                    "event sequence evicted from deduplication window: {}",
                                    res.body
                                )));
                            }
                            "graph_desynchronized" => {
                                return Err(FeederClientError::RecoveryRequired(format!(
                                    "graph is desynchronized: {}",
                                    res.body
                                )));
                            }
                            _ => {
                                return Err(FeederClientError::Http {
                                    status: res.status,
                                    body: res.body,
                                });
                            }
                        }
                    }
                    return Err(FeederClientError::Http {
                        status: res.status,
                        body: res.body,
                    });
                }
                Err(FeederClientError::Io(e)) => {
                    if attempts > max_retries {
                        return Err(FeederClientError::Io(e));
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(200));

                    // Verify heartbeat status didn't fail during the transport error
                    self.ensure_heartbeat_healthy()?;

                    // Loop continues: retries the EXACT request with immutable identities.
                }
                Err(other) => return Err(other),
            }
        }
    }

    /// Imports a snapshot with conservative reconciliation if an acknowledgment is lost.
    ///
    /// Captures pre-attempt identity and preconditions. If an acknowledgment is lost, queries
    /// live statistics to determine status. Because counts and cursor equality do not prove
    /// graph identity or attribution to this import, publication success is never synthesized.
    /// If evidence is ambiguous, or if competing publication, topology mismatch, or restart occurred,
    /// returns an explicit `UncertainOutcome` or `RecoveryRequired` error without inventing proof.
    pub async fn import_with_reconciliation(
        &mut self,
        mode: &str,
        source_id: &str,
        source_epoch: &str,
        snapshot: &SourceSnapshot,
        if_match: Option<&str>,
        max_retries: usize,
    ) -> Result<HttpResponse, FeederClientError> {
        if mode == "replace" && if_match.is_none() {
            return Err(FeederClientError::Protocol(
                "mandatory replacement precondition missing: If-Match is required for replace mode"
                    .into(),
            ));
        }

        let backoff = Duration::from_millis(20);

        // Pre-attempt: verify heartbeat and capture baseline state
        self.ensure_heartbeat_healthy()?;
        let pre_stats = self.get_stats().await?;
        let pre_json = pre_stats.json();
        let pre_instance = self.instance_id.clone();
        let pre_gen = pre_json["generation"].as_u64().unwrap_or(0);

        if mode == "create_only" && pre_gen != 0 {
            return Err(FeederClientError::Protocol(format!(
                "cannot execute create_only import: server generation is already {pre_gen}"
            )));
        }

        self.ensure_heartbeat_healthy()?;

        match self
            .import(mode, source_id, source_epoch, snapshot, if_match)
            .await
        {
            Ok(res) => {
                if res.status == 200 || res.status == 201 {
                    return Ok(res);
                }
                if res.status == 409 {
                    let json = res.json();
                    let code = json["code"].as_str().unwrap_or("");
                    match code {
                        "stale_instance_id" => {
                            let _ = self
                                .heartbeat_status_tx
                                .send(HeartbeatStatus::StaleInstance(res.body.clone()));
                            return Err(FeederClientError::StaleInstance(format!(
                                "server instance changed: {}",
                                res.body
                            )));
                        }
                        "fenced_feeder_session" => {
                            let _ = self
                                .heartbeat_status_tx
                                .send(HeartbeatStatus::Fenced(res.body.clone()));
                            return Err(FeederClientError::Fenced(format!(
                                "feeder session fenced: {}",
                                res.body
                            )));
                        }
                        "lease_expired" => {
                            let _ = self
                                .heartbeat_status_tx
                                .send(HeartbeatStatus::LeaseExpired(res.body.clone()));
                            return Err(FeederClientError::LeaseExpired(format!(
                                "feeder lease expired: {}",
                                res.body
                            )));
                        }
                        _ => {
                            return Err(FeederClientError::Http {
                                status: res.status,
                                body: res.body,
                            });
                        }
                    }
                }
                Err(FeederClientError::Http {
                    status: res.status,
                    body: res.body,
                })
            }
            Err(FeederClientError::Io(e)) => {
                if max_retries == 0 {
                    return Err(FeederClientError::Io(e));
                }
                tokio::time::sleep(backoff).await;

                self.ensure_heartbeat_healthy()?;

                let post_stats = match self.get_stats().await {
                    Ok(s) => s,
                    Err(err) => {
                        return Err(FeederClientError::UncertainOutcome(format!(
                            "failed to query stats after lost import ACK: {err}"
                        )));
                    }
                };

                if post_stats.status != 200 {
                    return Err(FeederClientError::UncertainOutcome(format!(
                        "stats returned HTTP status {} during import reconciliation",
                        post_stats.status
                    )));
                }

                let sj = post_stats.json();
                let post_inst = sj["instance_id"].as_str().unwrap_or("");
                if post_inst != pre_instance {
                    let _ = self
                        .heartbeat_status_tx
                        .send(HeartbeatStatus::StaleInstance(post_stats.body.clone()));
                    return Err(FeederClientError::RecoveryRequired(format!(
                        "server restarted during uncertain import outcome (instance changed from {pre_instance} to {post_inst})"
                    )));
                }

                if let Some(post_session) = sj
                    .get("active_feeder_session_id")
                    .and_then(|v| v.as_str())
                    .filter(|&post_session| post_session != self.feeder_session_id)
                {
                    let _ = self
                        .heartbeat_status_tx
                        .send(HeartbeatStatus::Fenced(post_stats.body.clone()));
                    return Err(FeederClientError::Fenced(format!(
                        "competing session '{post_session}' acquired lease during uncertain import outcome"
                    )));
                }

                let post_gen = sj["generation"].as_u64().unwrap_or(0);
                let post_pos = sj["source_position"].as_str().unwrap_or("");
                let post_nodes = sj["node_count"].as_u64().unwrap_or(0);
                let post_edges = sj["edge_count"].as_u64().unwrap_or(0);

                let expected_gen = if mode == "create_only" {
                    1
                } else {
                    pre_gen + 1
                };

                if post_gen == pre_gen {
                    return Err(FeederClientError::UncertainOutcome(format!(
                        "publication not observed on server (generation remains {pre_gen}); publication may be uncommitted or delayed"
                    )));
                }

                if post_gen == expected_gen {
                    let expected_nodes = snapshot.nodes.len() as u64;
                    let expected_edges = snapshot.edges.len() as u64;

                    if post_pos != snapshot.position
                        || post_nodes != expected_nodes
                        || post_edges != expected_edges
                    {
                        return Err(FeederClientError::UncertainOutcome(format!(
                            "import outcome is uncertain: server generation advanced to {post_gen} but topology/metadata does not match requested snapshot (expected pos='{}', nodes={expected_nodes}, edges={expected_edges}; found pos='{post_pos}', nodes={post_nodes}, edges={post_edges})",
                            snapshot.position
                        )));
                    } else {
                        // Counts and cursor equality do not prove graph identity or attribution to this import.
                        // Missing ownership/source fields from /stats must not be treated as a match.
                        // With the current API, return an explicit UncertainOutcome requiring recovery.
                        return Err(FeederClientError::UncertainOutcome(format!(
                            "import outcome is uncertain: server generation advanced to {post_gen} with matching cursor '{post_pos}' and counts ({post_nodes} nodes, {post_edges} edges), but server API cannot establish attribution to this import; recovery required"
                        )));
                    }
                }

                if post_gen > expected_gen {
                    return Err(FeederClientError::UncertainOutcome(format!(
                        "server generation is {post_gen} (expected {expected_gen}); graph was replaced by another feeder before reconciliation"
                    )));
                }

                Err(FeederClientError::UncertainOutcome(format!(
                    "unexpected server generation {post_gen} (pre-attempt was {pre_gen}, expected {expected_gen})"
                )))
            }
            Err(other) => Err(other),
        }
    }

    /// Executes a safe conditional rebuild from an authoritative snapshot.
    ///
    /// Validates active ownership and heartbeat health, obtains coherent current metadata
    /// from `GET /v1/stats`, extracts the mandatory composite `ETag`, and imports the snapshot
    /// using `mode: "replace"` and `If-Match`. Never performs an unconditional replacement.
    pub async fn conditional_rebuild_snapshot(
        &mut self,
        source_id: &str,
        source_epoch: &str,
        snapshot: &SourceSnapshot,
    ) -> Result<HttpResponse, FeederClientError> {
        self.ensure_heartbeat_healthy()?;
        let stats = self.get_stats().await?;
        if stats.status != 200 {
            return Err(FeederClientError::Http {
                status: stats.status,
                body: stats.body,
            });
        }
        let raw_etag = stats.header("ETag").ok_or_else(|| {
            FeederClientError::Protocol(
                "cannot execute conditional rebuild: missing ETag header in /stats".into(),
            )
        })?;
        let etag = if raw_etag.starts_with('"') && raw_etag.ends_with('"') {
            raw_etag.to_string()
        } else {
            format!("\"{raw_etag}\"")
        };

        self.import(
            "replace",
            source_id,
            source_epoch,
            snapshot,
            Some(etag.as_str()),
        )
        .await
    }

    /// Executes a complete authoritative build / rebuild cycle:
    /// 1. Discovers server instance ID and validates mode.
    /// 2. Registers session and starts background heartbeat.
    /// 3. Imports baseline snapshot ($S$).
    /// 4. Streams incremental events.
    /// 5. Submits synchronization barrier ($B$).
    /// 6. Verifies `/ready` returns 200 `ready`.
    pub async fn sync_from_fixture(
        &mut self,
        fixture: &ExternalSourceFixture,
        is_replacement: bool,
    ) -> Result<(), FeederClientError> {
        self.discover().await?;
        self.register_session(10_000).await?;
        self.start_heartbeat(Duration::from_millis(2_000));

        let import_mode = if is_replacement {
            "replace"
        } else {
            "create_only"
        };
        let if_match = if is_replacement {
            self.ensure_heartbeat_healthy()?;
            let stats = self.get_stats().await?;
            if stats.status != 200 {
                return Err(FeederClientError::Http {
                    status: stats.status,
                    body: stats.body,
                });
            }
            let raw_etag = stats.header("ETag").ok_or_else(|| {
                FeederClientError::Protocol(
                    "cannot execute conditional replacement rebuild: missing ETag header in /stats"
                        .into(),
                )
            })?;
            let etag = if raw_etag.starts_with('"') && raw_etag.ends_with('"') {
                raw_etag.to_string()
            } else {
                format!("\"{raw_etag}\"")
            };
            Some(etag)
        } else {
            None
        };

        let imp_res = self
            .import_with_reconciliation(
                import_mode,
                &fixture.source_id,
                &fixture.source_epoch,
                &fixture.snapshot,
                if_match.as_deref(),
                3,
            )
            .await?;
        if imp_res.status != 200 && imp_res.status != 201 {
            return Err(FeederClientError::Http {
                status: imp_res.status,
                body: imp_res.body,
            });
        }

        if !fixture.events.is_empty() {
            let evt_res = self.apply_events_with_recovery(&fixture.events, 3).await?;
            if evt_res.status != 200 {
                return Err(FeederClientError::Http {
                    status: evt_res.status,
                    body: evt_res.body,
                });
            }
        }

        let (b_seq, b_pos) = &fixture.high_water_mark;
        let b_res = self.declare_barrier(*b_seq, b_pos).await?;
        if b_res.status != 200 {
            return Err(FeederClientError::Http {
                status: b_res.status,
                body: b_res.body,
            });
        }

        let ready_res = self.get_ready().await?;
        if ready_res.status != 200 {
            return Err(FeederClientError::Protocol(format!(
                "server not ready after barrier: status {} body {}",
                ready_res.status, ready_res.body
            )));
        }

        Ok(())
    }
}

impl Drop for ExternalFeederClient {
    fn drop(&mut self) {
        self.abort_heartbeat();
    }
}
