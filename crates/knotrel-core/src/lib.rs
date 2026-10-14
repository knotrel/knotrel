//! Exact, in-memory dynamic connectivity for undirected simple graphs.
//!
//! Knotrel maintains exact reachability queries ("is vertex $u$ connected to vertex $v$
//! via some path?") while edges are dynamically inserted and deleted over time.
//! It is a specialized dynamic connectivity engine, not a generic graph database.
//!
//! # Graph Model
//!
//! - **Undirected simple graphs**: Edges have no orientation and connect two distinct endpoints.
//! - **64-bit integer identifiers**: Vertices are identified by [`NodeId`] (`u64`).
//! - **No self-loops**: Attempting to link a vertex to itself returns [`GraphError::SelfLoop`].
//! - **No multi-edges**: Linking an already existing edge is an idempotent no-op returning `Ok(false)`.
//! - **Dynamic vertex registration**: Calling [`ConnectivityGraph::add_node`] explicitly registers an isolated vertex;
//!   calling [`ConnectivityGraph::link`] implicitly creates missing endpoints.
//! - **Cuts retain vertices**: Calling [`ConnectivityGraph::cut`] removes the edge, but both endpoints
//!   remain registered in the graph universe.
//!
//! # Quick Start
//!
//! Because `knotrel-core` is currently unpublished (`publish = false`), integrate it
//! into your `Cargo.toml` as a path or git dependency:
//!
//! ```toml
//! [dependencies]
//! # Local path dependency:
//! knotrel-core = { path = "path/to/knotrel/crates/knotrel-core" }
//! # Or via git repository:
//! # knotrel-core = { git = "https://github.com/knotrel/knotrel.git" }
//! # Future registry dependency (once published):
//! # knotrel-core = "0.1.0"
//! ```
//!
//! Basic usage with [`ConnectivityGraph`]:
//!
//! ```rust
//! use knotrel_core::{ConnectivityGraph, EngineConfig};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a graph using the default compact BFS engine.
//!     let mut graph = ConnectivityGraph::new(EngineConfig::CompactBfs);
//!
//!     // Add nodes and link edges.
//!     graph.add_node(1)?;
//!     graph.add_node(2)?;
//!     graph.link(1, 2)?;
//!     graph.link(2, 3)?;
//!
//!     // Query connectivity.
//!     assert!(graph.connected(1, 3)?);
//!
//!     // Cut an edge and observe the split.
//!     assert!(graph.cut(2, 3)?);
//!     assert!(!graph.connected(1, 3)?);
//!
//!     // Existing endpoints are preserved after cuts.
//!     assert_eq!(graph.node_count(), 3);
//!
//!     Ok(())
//! }
//! ```
//!
//! # Bulk Initialization
//!
//! If you have existing vertex and edge collections, construct a graph using
//! [`ConnectivityGraph::try_from_edges`]:
//!
//! ```rust
//! use knotrel_core::{ConnectivityGraph, EngineConfig, GraphLimits};
//!
//! let isolated_nodes = [99];
//! let edges = [(1, 2), (2, 3), (3, 4)];
//!
//! let mut graph = ConnectivityGraph::try_from_edges(
//!     EngineConfig::CompactBfs,
//!     GraphLimits::default(),
//!     &isolated_nodes,
//!     &edges,
//! )?;
//!
//! assert!(graph.connected(1, 4)?);
//! assert!(!graph.connected(1, 99)?);
//!
//! // The resulting graph supports continued dynamic mutations:
//! graph.link(4, 99)?;
//! assert!(graph.connected(1, 99)?);
//! # Ok::<(), knotrel_core::GraphError>(())
//! ```
//!
//! Construction processes all explicit `nodes` first, then inserts `edges` in input order.
//! It is atomic with respect to publication: on the first validation error, the partially
//! constructed graph is dropped and the error is returned.
//!
//! # Choosing a Backend Engine
//!
//! Knotrel provides three underlying dynamic connectivity engines through [`EngineConfig`].
//!
//! ### Algorithmic Complexity and API Assumptions
//!
//! The asymptotic costs in the table below describe the core graph and forest algorithms
//! operating on internal indices. The public API maps external 64-bit [`NodeId`] keys to internal
//! indices using hash maps (`IntMap` / `FxHashMap`), which rely on expected $O(1)$ lookups and do
//! not provide deterministic worst-case constant-time bounds for arbitrary ID distributions.
//! Furthermore, collection and arena vector growth is amortized; a single vertex insertion or
//! capacity reallocation may take $O(V)$ in the worst case.
//!
//! | Engine | Configuration | Query Cost | Link Cost | Cut Cost | Logical Memory |
//! | --- | --- | --- | --- | --- | --- |
//! | **Compact BFS** (Default) | [`EngineConfig::CompactBfs`] | $O(V + E)$ worst-case | $O(\text{deg}(u) + \text{deg}(v))$ amortized | $O(\text{deg}(u) + \text{deg}(v))$ worst-case | $O(V + E)$ |
//! | **Euler Tour Trees** (ETT) | [`EngineConfig::EulerTour`] | $O(\log V)$ algorithmically | $O(\log V + \log E)$ amortized | $O(\log V + \log E + s + c \log V)$ amortized | $O(V + E)$ |
//! | **HDT** | [`EngineConfig::Hdt`] | $O(\log V)$ algorithmically | $O(\log V)$ amortized | $O(\log^2 V)$ amortized | $O(E + V \log V)$ |
//!
//! *Note on memory:* $O(V + E)$ and $O(E + V \log V)$ represent *logical* graph entity storage.
//! Allocated memory capacities are retained after cuts and do not bound process heap memory or RSS.
//!
//! ### 1. Compact BFS ([`EngineConfig::CompactBfs`])
//! - **Algorithmic properties**: Represents adjacency as compact, sorted vectors of stable internal indices.
//!   Queries perform a fresh breadth-first traversal in $O(V + E)$ worst-case time using $O(V)$ temporary scratch space.
//!   Link and cut shift sorted vectors in $O(\text{deg}(u) + \text{deg}(v))$ time. Adding a vertex can trigger an $O(V)$
//!   outer-vector reallocation.
//! - **Workload expectations**: Contiguous memory layout and low allocation overhead. Suitable as a general-purpose
//!   engine when mutations are frequent or graph components are small-to-moderate in size.
//!
//! ### 2. Euler Tour Trees ([`EngineConfig::EulerTour`])
//! - **Algorithmic properties**: Maintains a spanning forest using AVL-balanced Euler tour sequences.
//!   Reachability queries compare tour roots in $O(\log V)$ algorithmic steps without graph traversals. Non-tree edge
//!   cuts take $O(\log V + \log E)$ amortized time. Tree cuts partition the tree and scan non-tree edges of the smaller
//!   component ($s$ vertices, $c$ candidate edges) using candidate-pruned subtrees; this search costs
//!   $O(\log V + \log E + s + c \log V)$ amortized and allocates $O(s)$ scratch space.
//! - **Workload expectations**: Effective for query-dominated workloads and tree-like topologies where bridge cuts
//!   rarely split large components. Because it lacks edge-level hierarchies, repeated bridge cuts in dense cyclic
//!   components can trigger repeated scans over the same non-tree edges.
//!
//! ### 3. Holm–de Lichtenberg–Thorup ([`EngineConfig::Hdt`])
//! - **Algorithmic properties**: Experimental deterministic implementation based on Holm, de Lichtenberg, and
//!   Thorup (2001), extended with an append-only growth policy for expanding vertex universes.
//!   Maintains a hierarchy of $O(\log V)$ levels, materializing upper levels lazily as edge promotions require them.
//!   Queries run on the base level in $O(\log V)$ algorithmic steps. Tree cuts search for replacement edges starting
//!   at the cut edge's level downwards; examined tree edges and non-tree edges on the smaller component are promoted
//!   to upper levels, bounding the amortized update cost to $O(\log^2 V)$ across operation sequences.
//! - **Workload expectations**: Designed to prevent repeated scan storms in dense graphs undergoing frequent bridge cuts.
//!   However, its amortized bounds represent sequence averages rather than worst-case per-request latency guarantees;
//!   large individual promotion passes or vector reallocations can cause latency spikes, and it may have higher constant
//!   factors on small or sparse graphs; verify against your actual workload.
//!
//! ### Pragmatic Guidance
//! - **No universal winner**: Micro-benchmarks demonstrate that no single engine dominates across all graph topologies
//!   and operation mixes.
//! - **Empirical evaluation**: If your workload characteristics (topology, scale, mutation mix) are uncertain, profile
//!   empirically rather than assuming polylogarithmic engines will outperform Compact BFS.
//! - **Starting point**: Start with Compact BFS ([`EngineConfig::CompactBfs`]); evaluate ETT ([`EngineConfig::EulerTour`])
//!   for query-dominated workloads or HDT ([`EngineConfig::Hdt`]) for workloads with repeated bridge replacement searches,
//!   measuring actual topology, scale, and mutation mix.
//!
//! # Error and Resource Limit Handling
//!
//! All operations that may fail return [`Result<T, GraphError>`]. The possible errors in [`GraphError`] are:
//! - [`GraphError::SelfLoop`]: The source and target node identifiers were identical.
//! - [`GraphError::UnknownNode`]: A reachability query referenced a vertex that was never added or linked.
//! - [`GraphError::NodeLimitExceeded`]: A mutation would exceed the configured [`GraphLimits::max_nodes`].
//! - [`GraphError::EdgeLimitExceeded`]: A mutation would exceed the configured [`GraphLimits::max_edges`].
//!
//! ### Cardinality Limits vs. Memory
//! [`GraphLimits`] enforce strict ceilings on registered vertex and live edge counts.
//! These limits bound logical graph cardinality, not allocator heap memory or process RSS.
//! Rejected mutations leave the graph completely unmodified.
//!
//! # Guarantees and Limitations
//!
//! - **Exact answers**: Knotrel computes exact connectivity; answers are never heuristic or approximate.
//! - **Synchronous execution**: All graph algorithms execute synchronously on the caller's thread.
//! - **Zero internal locking**: Graphs do not contain internal mutexes. For concurrent multi-threaded access,
//!   wrap [`ConnectivityGraph`] in standard Rust synchronization primitives (such as `std::sync::Mutex` or `std::sync::RwLock`).
//! - **In-memory volatile state**: Graph data lives solely in memory. In `v0.1.0`, there is no write-ahead log (WAL),
//!   disk persistence, or crash recovery.
//! - **No generic graph properties**: Knotrel is not an attributed property graph; it does not store node labels,
//!   edge weights, directed edges, or multi-edges.
//! - **Pre-1.0 stability**: As a `0.1.0` release, APIs are subject to refinement before 1.0 based on user adoption feedback.

use fixedbitset::FixedBitSet;
use nohash_hasher::IntMap;
use std::{collections::hash_map::Entry, error::Error, fmt};

mod workspace;
pub use workspace::BfsWorkspace;

mod configuration;
mod dynamic;
mod forest;
mod hdt;
mod hdt_forest;
mod hdt_profile;
mod index;
pub use hdt::{HdtGraph, HdtStats};
pub use hdt_profile::{HdtLevelStorage, HdtStorageStats};
mod reference;
pub use configuration::{ConnectivityGraph, EngineConfig, GraphLimits};
pub use dynamic::{ForestGraph, ForestStats};
pub use reference::ReferenceGraph;

/// A vertex identifier.
pub type NodeId = u64;

/// An invalid graph operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphError {
    /// A mutation would exceed the registered vertex ceiling.
    NodeLimitExceeded {
        /// Configured maximum vertex count.
        limit: usize,
    },
    /// A mutation would exceed the live edge ceiling.
    EdgeLimitExceeded {
        /// Configured maximum edge count.
        limit: usize,
    },
    /// A self-loop was requested.
    SelfLoop {
        /// The repeated endpoint.
        node: NodeId,
    },
    /// A query referenced an absent vertex.
    UnknownNode {
        /// The absent vertex.
        node: NodeId,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeLimitExceeded { limit } => write!(formatter, "node limit exceeded: {limit}"),
            Self::EdgeLimitExceeded { limit } => write!(formatter, "edge limit exceeded: {limit}"),
            Self::SelfLoop { node } => write!(formatter, "self-loops are not supported: {node}"),
            Self::UnknownNode { node } => write!(formatter, "unknown node: {node}"),
        }
    }
}

impl GraphError {
    #[inline]
    pub(crate) fn check_distinct(source: NodeId, target: NodeId) -> Result<(), Self> {
        if source == target {
            Err(Self::SelfLoop { node: source })
        } else {
            Ok(())
        }
    }
}

impl Error for GraphError {}

/// An undirected simple graph with compact adjacency and exact BFS queries.
///
/// External IDs map to stable, append-only indices. Each sorted adjacency vector
/// contains unique indices, and every edge appears at both endpoints. Cuts retain
/// vertices. Logical storage is O(V + E). Allocated storage is O(V + H), where H is
/// the sum of per-vertex historical maximum degrees (capacity is retained). No query answers are cached.
#[derive(Debug, Default)]
pub struct Graph {
    ids: IntMap<NodeId, usize>,
    adjacency: Vec<Vec<usize>>,
    edges: usize,
}

impl Graph {
    pub(crate) fn contains_edge(&self, source: NodeId, target: NodeId) -> bool {
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return false;
        };
        self.adjacency[a].binary_search(&b).is_ok()
    }

    /// Creates an empty graph in O(1) time.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an isolated vertex, returning true only if it was absent.
    ///
    /// Existing edges are unchanged. O(1) amortized time; growing the outer
    /// vector can take O(V) in a single insertion. Indices never change.
    pub fn add_node(&mut self, node: NodeId) -> bool {
        if let Entry::Vacant(entry) = self.ids.entry(node) {
            let index = self.adjacency.len();
            self.adjacency.push(Vec::new());
            entry.insert(index);
            true
        } else {
            false
        }
    }

    /// Inserts an undirected edge, creating absent endpoints.
    ///
    /// Reversed endpoints identify the same edge; duplicates return false.
    /// O(1 + deg(source) + deg(target)) amortized time, including sorted
    /// vector shifts. A new vertex can trigger an O(V) capacity growth.
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] without creating vertices for equal IDs.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        self.add_node(source);
        self.add_node(target);
        let a = self.ids[&source];
        let b = self.ids[&target];
        let Err(position) = self.adjacency[a].binary_search(&b) else {
            return Ok(false);
        };
        let reverse = self.adjacency[b]
            .binary_search(&a)
            .expect_err("symmetric absence");
        self.adjacency[a].insert(position, b);
        self.adjacency[b].insert(reverse, a);
        self.edges += 1;
        Ok(true)
    }

    /// Removes an edge, preserving vertices; absent edges return false.
    ///
    /// Worst-case time: O(1 + deg(source) + deg(target)), due to vector shifts.
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] for equal IDs without changing the graph.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return Ok(false);
        };
        let Ok(position) = self.adjacency[a].binary_search(&b) else {
            return Ok(false);
        };
        let reverse = self.adjacency[b].binary_search(&a).expect("symmetric edge");
        self.adjacency[a].remove(position);
        self.adjacency[b].remove(reverse);
        self.edges -= 1;
        Ok(true)
    }

    /// Reports whether a path exists between two existing vertices.
    ///
    /// Existing vertices connect to themselves. Worst-case time O(V + E), with
    /// O(1) endpoint lookup and O(V) local scratch initialization/allocation.
    /// Scratch is private to each call, so shared queries need no internal lock.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownNode`] for absent endpoints, source first.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        let a = *self
            .ids
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let b = *self
            .ids
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        if a == b {
            return Ok(true);
        }
        let mut visited = FixedBitSet::with_capacity(self.adjacency.len());
        let mut queue = Vec::new();
        visited.insert(a);
        queue.push(a);
        let mut cursor = 0;
        // Mark on enqueue: each vertex is visited at most once and every queued
        // vertex is reachable. Exhaustion proves the target is disconnected.
        // Integer adjacency avoids ordered-map lookups inside the traversal.
        while cursor < queue.len() {
            for &neighbor in &self.adjacency[queue[cursor]] {
                if neighbor == b {
                    return Ok(true);
                }
                if !visited.contains(neighbor) {
                    visited.insert(neighbor);
                    queue.push(neighbor);
                }
            }
            cursor += 1;
        }
        Ok(false)
    }

    /// Returns the number of vertices, including isolated ones, in O(1) time.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.adjacency.len()
    }

    /// Returns the number of undirected edges in O(1) time.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges
    }
}
