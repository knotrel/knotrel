//! Exact, in-memory dynamic connectivity for undirected simple graphs.
//!
//! Knotrel maintains exact reachability queries ("is vertex `u` connected to vertex `v`
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
//! Add the latest published `knotrel-core` release to your project:
//!
//! ```sh
//! cargo add knotrel-core
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
//! # Node and Edge Lifecycle
//!
//! [`LifecycleGraph`] adds reversible node activation, reversible edge
//! enabling and permanent node removal on the `compact-bfs` engine (`ett` and
//! `hdt` lifecycle support is deferred and rejected with
//! [`LifecycleError::UnsupportedEngine`]). It keeps the registered
//! topology with flags and answers connectivity over the effective graph:
//! enabled edges whose endpoints are both active. [`ConnectivityGraph`] is
//! unchanged and carries no lifecycle bookkeeping.
//!
//! ```rust
//! use knotrel_core::{EngineConfig, LifecycleGraph};
//!
//! let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
//! graph.link(1, 2)?;
//! graph.set_edge_enabled(1, 2, false)?;
//! assert!(!graph.connected(1, 2)?);
//! assert_eq!(graph.edge_count(), 1);
//! assert_eq!(graph.effective_edge_count(), 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Listing Neighbors (since 0.8)
//!
//! Neighbors can be listed page by page with a cursor, for browsing tools.
//! On `compact-bfs`, [`Graph::neighbors_after`] needs a one-time opt-in
//! ([`ConnectivityGraph::enable_neighbor_listing`], O(V)) because it keeps an
//! index-to-ID vector (8 bytes per node) that ordinary use does not need.
//! [`LifecycleGraph::incident_after`] lists neighbors with their edge state,
//! in ascending neighbor ID. A cursor is the last neighbor returned; pages are
//! not a snapshot of a graph that changes between them.
//!
//! ```rust
//! use knotrel_core::{ConnectivityGraph, EngineConfig};
//!
//! let mut graph = ConnectivityGraph::new(EngineConfig::CompactBfs);
//! graph.link(1, 2)?;
//! graph.link(1, 3)?;
//! assert!(graph.enable_neighbor_listing());
//! let compact = graph.as_compact().expect("compact-bfs engine");
//! assert_eq!(compact.degree(1), Some(2));
//! let first: Vec<_> = compact.neighbors_after(1, None)?.take(1).collect();
//! let rest: Vec<_> = compact.neighbors_after(1, first.last().copied())?.collect();
//! assert_eq!(first.len() + rest.len(), 2);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Choosing a Backend Engine
//!
//! Knotrel provides three underlying dynamic connectivity engines through [`EngineConfig`].
//!
//! ### Algorithmic Complexity and API Assumptions
//!
//! The asymptotic costs in the table below describe the core graph and forest algorithms
//! operating on internal indices. The public API maps external 64-bit [`NodeId`] keys to internal
//! indices using hash maps (`IntMap` / `FxHashMap`), which rely on expected `O(1)` lookups and do
//! not provide deterministic worst-case constant-time bounds for arbitrary ID distributions.
//! Furthermore, collection and arena vector growth is amortized; a single vertex insertion or
//! capacity reallocation may take `O(V)` in the worst case.
//!
//! | Engine | Configuration | Query Cost | Link Cost | Cut Cost | Logical Memory |
//! | --- | --- | --- | --- | --- | --- |
//! | **Compact BFS** (Default) | [`EngineConfig::CompactBfs`] | `O(V + E)` worst-case | `O(deg(u) + deg(v))` amortized | `O(deg(u) + deg(v))` worst-case | `O(V + E)` |
//! | **Euler Tour Trees** (ETT) | [`EngineConfig::EulerTour`] | `O(log V)` algorithmically | `O(log V + log E)` amortized | `O(log V + log E + s + c log V)` amortized | `O(V + E)` |
//! | **HDT** | [`EngineConfig::Hdt`] | `O(log V)` algorithmically | `O(log V)` amortized | `O(log² V)` amortized | `O(E + V log V)` |
//!
//! *Note on memory:* `O(V + E)` and `O(E + V log V)` represent *logical* graph entity storage.
//! Allocated memory capacities are retained after cuts and do not bound process heap memory or RSS.
//!
//! ### 1. Compact BFS ([`EngineConfig::CompactBfs`])
//! - **Algorithmic properties**: Represents adjacency as compact, sorted vectors of stable internal indices.
//!   Queries perform a fresh breadth-first traversal in `O(V + E)` worst-case time using `O(V)` temporary scratch space.
//!   Link and cut shift sorted vectors in `O(deg(u) + deg(v))` time. Adding a vertex can trigger an `O(V)`
//!   outer-vector reallocation.
//! - **Workload expectations**: Contiguous memory layout and low allocation overhead. Suitable as a general-purpose
//!   engine when mutations are frequent or graph components are small-to-moderate in size.
//!
//! ### 2. Euler Tour Trees ([`EngineConfig::EulerTour`])
//! - **Algorithmic properties**: Maintains a spanning forest using AVL-balanced Euler tour sequences.
//!   Reachability queries compare tour roots in `O(log V)` algorithmic steps without graph traversals. Non-tree edge
//!   cuts take `O(log V + log E)` amortized time. Tree cuts partition the tree and scan non-tree edges of the smaller
//!   component (`s` vertices, `c` candidate edges) using candidate-pruned subtrees; this search costs
//!   `O(log V + log E + s + c log V)` amortized and allocates `O(s)` scratch space.
//! - **Workload expectations**: Effective for query-dominated workloads and tree-like topologies where bridge cuts
//!   rarely split large components. Because it lacks edge-level hierarchies, repeated bridge cuts in dense cyclic
//!   components can trigger repeated scans over the same non-tree edges.
//!
//! ### 3. Holm–de Lichtenberg–Thorup ([`EngineConfig::Hdt`])
//! - **Algorithmic properties**: Experimental deterministic implementation based on Holm, de Lichtenberg, and
//!   Thorup (2001), extended with an append-only growth policy for expanding vertex universes.
//!   Maintains a hierarchy of `O(log V)` levels, materializing upper levels lazily as edge promotions require them.
//!   Queries run on the base level in `O(log V)` algorithmic steps. Tree cuts search for replacement edges starting
//!   at the cut edge's level downwards; examined tree edges and non-tree edges on the smaller component are promoted
//!   to upper levels, bounding the amortized update cost to `O(log² V)` across operation sequences.
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
//! - **In-memory volatile state**: Graph data lives solely in memory. There is no write-ahead log (WAL),
//!   disk persistence, or crash recovery.
//! - **No generic graph properties**: Knotrel is not an attributed property graph; it does not store node labels,
//!   edge weights, directed edges, or multi-edges.
//! - **Stability classes**: every public item is classified **stable**, **experimental** or **internal** in the
//!   [stability classification](https://github.com/knotrel/knotrel/blob/main/docs/stability.md). Stable items change
//!   only as the [compatibility policy](https://github.com/knotrel/knotrel/blob/main/docs/compatibility.md) allows,
//!   after a deprecation. The `EulerTour` and `Hdt` engines with their types, and [`LifecycleStorage`], are
//!   experimental; [`ReferenceGraph`] is an internal test oracle.

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
mod topology;
pub use topology::{
    EdgeSpec, EdgeState, LifecycleError, LifecycleGraph, LifecycleStorage, NodeSpec,
};

/// A vertex identifier.
pub type NodeId = u64;

/// An invalid graph operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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

/// Why an incidence listing could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IncidenceError {
    /// The node whose incident edges were requested is not registered.
    UnknownNode,
    /// The pagination cursor names a node that is not registered.
    UnknownCursor,
    /// Neighbor listing was not enabled with [`Graph::enable_neighbor_listing`].
    NotEnabled,
}

impl fmt::Display for IncidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownNode => "unknown node",
            Self::UnknownCursor => "unknown incidence cursor",
            Self::NotEnabled => "neighbor listing is not enabled",
        })
    }
}

impl Error for IncidenceError {}

/// An undirected simple graph with compact adjacency and exact BFS queries.
///
/// External IDs map to stable, append-only indices. Neighbor listing
/// ([`neighbors_after`](Self::neighbors_after)) is opt-in: after
/// [`enable_neighbor_listing`](Self::enable_neighbor_listing) the graph also keeps
/// the ID of every index (8 bytes per vertex); otherwise it keeps nothing extra.
/// Each sorted adjacency vector contains unique indices, and every edge appears at
/// both endpoints. Cuts retain vertices. Logical storage is O(V + E). Allocated storage is O(V + H), where H is
/// the sum of per-vertex adjacency capacities: each is at least the vertex's
/// historical maximum degree (capacity is retained) and, for a graph built in
/// bulk by the lifecycle facade, starts at the vertex's enabled registered
/// degree. No query answers are cached.
#[derive(Debug, Default)]
pub struct Graph {
    ids: IntMap<NodeId, usize>,
    /// Inverse of `ids` (the ID registered at each index), kept only after
    /// [`Graph::enable_neighbor_listing`]. Indices are never reused or removed,
    /// so it only grows, in step with `adjacency`.
    nodes: Option<Vec<NodeId>>,
    adjacency: Vec<Vec<usize>>,
    edges: usize,
}

impl Graph {
    /// Reports whether `node` is registered; expected O(1) ID-map lookup.
    pub(crate) fn contains_node(&self, node: NodeId) -> bool {
        self.ids.contains_key(&node)
    }

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

    /// Returns the stable internal index for `node`, registering it if absent.
    ///
    /// Preserves append-only indexing and O(1) amortized time.
    #[inline]
    fn get_or_insert_index(&mut self, node: NodeId) -> usize {
        match self.ids.entry(node) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let index = self.adjacency.len();
                self.adjacency.push(Vec::new());
                if let Some(nodes) = &mut self.nodes {
                    nodes.push(node);
                }
                entry.insert(index);
                index
            }
        }
    }

    /// Adds an isolated vertex, returning true only if it was absent.
    ///
    /// Existing edges are unchanged. O(1) amortized time; growing the outer
    /// vector can take O(V) in a single insertion. Indices never change.
    pub fn add_node(&mut self, node: NodeId) -> bool {
        if let Entry::Vacant(entry) = self.ids.entry(node) {
            let index = self.adjacency.len();
            self.adjacency.push(Vec::new());
            if let Some(nodes) = &mut self.nodes {
                nodes.push(node);
            }
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
        let a = self.get_or_insert_index(source);
        let b = self.get_or_insert_index(target);
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

    /// Builds a graph in one pass from validated adjacency (bulk construction
    /// for the lifecycle facade).
    ///
    /// `ids[i]` receives internal index `i`; `adjacency[i]` lists the indices
    /// of its neighbors. The caller guarantees the [`Graph`] invariants:
    /// distinct IDs, equal lengths, every list strictly sorted, symmetric
    /// (`j` in `adjacency[i]` iff `i` in `adjacency[j]`) and free of
    /// self-loops; debug builds verify them. Vectors are taken as they are,
    /// so their capacities are kept.
    ///
    /// `O(V + E)` time plus the ID-map insertions (expected `O(1)` each);
    /// no adjacency vector is reallocated.
    pub(crate) fn from_sorted_adjacency(ids: Vec<NodeId>, adjacency: Vec<Vec<usize>>) -> Self {
        assert_eq!(ids.len(), adjacency.len(), "one adjacency list per ID");
        let mut map: IntMap<NodeId, usize> =
            IntMap::with_capacity_and_hasher(ids.len(), Default::default());
        for (index, &node) in ids.iter().enumerate() {
            let previous = map.insert(node, index);
            debug_assert!(previous.is_none(), "distinct IDs");
        }
        let incidences: usize = adjacency.iter().map(Vec::len).sum();
        debug_assert!(incidences.is_multiple_of(2), "symmetric adjacency");
        #[cfg(debug_assertions)]
        for (index, list) in adjacency.iter().enumerate() {
            for pair in list.windows(2) {
                debug_assert!(pair[0] < pair[1], "strictly sorted adjacency");
            }
            for &neighbor in list {
                debug_assert_ne!(neighbor, index, "no self-loops");
                debug_assert!(
                    adjacency[neighbor].binary_search(&index).is_ok(),
                    "symmetric adjacency"
                );
            }
        }
        Self {
            ids: map,
            nodes: None,
            adjacency,
            edges: incidences / 2,
        }
    }

    /// Returns the number of edges incident to `node`, or `None` if it is not
    /// registered. Expected O(1).
    ///
    /// ```rust
    /// let mut graph = knotrel_core::Graph::new();
    /// graph.link(1, 2)?;
    /// assert_eq!(graph.degree(1), Some(1));
    /// assert_eq!(graph.degree(9), None);
    /// # Ok::<(), knotrel_core::GraphError>(())
    /// ```
    #[must_use]
    pub fn degree(&self, node: NodeId) -> Option<usize> {
        self.ids
            .get(&node)
            .map(|&index| self.adjacency[index].len())
    }

    /// Starts keeping the ID of every internal index, which
    /// [`neighbors_after`](Self::neighbors_after) needs. Costs 8 bytes per vertex
    /// and one vector push per new vertex from now on; building the vector for
    /// the existing vertices is O(V). Calling it again does nothing.
    ///
    /// ```rust
    /// let mut graph = knotrel_core::Graph::new();
    /// graph.link(1, 2)?;
    /// assert!(graph.neighbors_after(1, None).is_err());
    /// graph.enable_neighbor_listing();
    /// assert_eq!(graph.neighbors_after(1, None)?.collect::<Vec<_>>(), [2]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn enable_neighbor_listing(&mut self) {
        if self.nodes.is_none() {
            let mut nodes = vec![0; self.adjacency.len()];
            for (&node, &index) in &self.ids {
                nodes[index] = node;
            }
            self.nodes = Some(nodes);
        }
    }

    /// Iterates the neighbors of `node` in the graph's stable incidence order,
    /// starting strictly after the neighbor `after` (or from the start).
    ///
    /// The order is the neighbors' registration order. It is stable across
    /// mutations: indices are never reused, so a neighbor present before and
    /// after a sequence of calls keeps its relative position. That makes
    /// `after` usable as a pagination cursor: every neighbor that stays linked
    /// while a caller pages through the list is visited exactly once, and
    /// neighbors linked or cut meanwhile may or may not be visited. `after`
    /// need not still be linked to `node`.
    ///
    /// Positioning costs O(log deg(node)) expected; each step is O(1).
    ///
    /// # Errors
    ///
    /// [`IncidenceError::NotEnabled`] before
    /// [`enable_neighbor_listing`](Self::enable_neighbor_listing),
    /// [`IncidenceError::UnknownNode`] if `node` is not registered, and
    /// [`IncidenceError::UnknownCursor`] if `after` is not a registered node.
    /// Vertices are never removed from a [`Graph`], so a cursor obtained from
    /// this graph never becomes unknown.
    ///
    /// ```rust
    /// let mut graph = knotrel_core::Graph::new();
    /// graph.enable_neighbor_listing();
    /// graph.link(5, 9)?;
    /// graph.link(5, 1)?;
    /// graph.link(5, 7)?;
    /// let all: Vec<_> = graph.neighbors_after(5, None)?.collect();
    /// assert_eq!(all, [9, 1, 7]); // registration order, not ID order
    /// let rest: Vec<_> = graph.neighbors_after(5, Some(9))?.collect();
    /// assert_eq!(rest, [1, 7]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn neighbors_after(
        &self,
        node: NodeId,
        after: Option<NodeId>,
    ) -> Result<impl Iterator<Item = NodeId> + '_, IncidenceError> {
        let &index = self.ids.get(&node).ok_or(IncidenceError::UnknownNode)?;
        let list = &self.adjacency[index];
        let start = match after {
            None => 0,
            Some(cursor) => {
                let &cursor = self.ids.get(&cursor).ok_or(IncidenceError::UnknownCursor)?;
                list.partition_point(|&neighbor| neighbor <= cursor)
            }
        };
        let nodes = self.nodes.as_deref().ok_or(IncidenceError::NotEnabled)?;
        Ok(list[start..].iter().map(move |&neighbor| nodes[neighbor]))
    }

    /// Releases the adjacency allocation of an isolated vertex.
    ///
    /// A no-op for an absent node or one that still has edges. The vertex and
    /// its index stay registered. `O(1)` expected.
    pub(crate) fn release_isolated_storage(&mut self, node: NodeId) {
        if let Some(&index) = self.ids.get(&node) {
            let list = &mut self.adjacency[index];
            if list.is_empty() {
                *list = Vec::new();
            }
        }
    }

    /// Returns the adjacency capacity (elements) held by `node`, if present.
    #[cfg(test)]
    pub(crate) fn adjacency_capacity(&self, node: NodeId) -> Option<usize> {
        self.ids
            .get(&node)
            .map(|&index| self.adjacency[index].capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paging through a hub's neighbors with `neighbors_after` visits every
    /// neighbor that stays linked exactly once, even when other edges are cut
    /// and added between pages; cursors stay valid after their edge is cut.
    #[test]
    fn neighbor_pages_visit_stable_neighbors_exactly_once() {
        let mut graph = Graph::new();
        graph.link(0, 3).unwrap();
        assert_eq!(
            graph.neighbors_after(0, None).err(),
            Some(IncidenceError::NotEnabled)
        );
        // Enabling after vertices exist rebuilds the index-to-ID vector.
        graph.enable_neighbor_listing();
        graph.enable_neighbor_listing();
        graph.cut(0, 3).unwrap();
        for neighbor in 1..=100u64 {
            graph.link(0, neighbor * 7).unwrap();
        }
        let stable: Vec<u64> = (1..=100u64).filter(|n| n % 3 != 0).map(|n| n * 7).collect();
        let mut seen = Vec::new();
        let mut cursor = None;
        let mut round = 0u64;
        loop {
            let page: Vec<u64> = graph.neighbors_after(0, cursor).unwrap().take(9).collect();
            let Some(&last) = page.last() else { break };
            seen.extend(page);
            cursor = Some(last);
            // Between pages: cut neighbors that are not "stable" (including the
            // cursor itself) and link new ones.
            for n in (1..=100u64).filter(|n| n % 3 == 0) {
                let _ = graph.cut(0, n * 7);
            }
            // New neighbors are appended after the cursor; stop adding them after
            // a few rounds so the traversal terminates.
            if round < 5 {
                round += 1;
                graph.link(0, 10_000 + round).unwrap();
            }
        }
        for node in &stable {
            assert_eq!(seen.iter().filter(|&&s| s == *node).count(), 1, "{node}");
        }
        assert_eq!(graph.degree(0), Some(stable.len() + round as usize));
        assert_eq!(graph.degree(424_242), None);
        assert_eq!(
            graph.neighbors_after(424_242, None).err(),
            Some(IncidenceError::UnknownNode)
        );
        assert_eq!(
            graph.neighbors_after(0, Some(424_242)).err(),
            Some(IncidenceError::UnknownCursor)
        );
        assert_eq!(
            IncidenceError::UnknownCursor.to_string(),
            "unknown incidence cursor"
        );
        assert_eq!(IncidenceError::UnknownNode.to_string(), "unknown node");
        assert_eq!(
            IncidenceError::NotEnabled.to_string(),
            "neighbor listing is not enabled"
        );
    }

    /// A bulk-built graph maps indices back to the same IDs as an incremental one.
    #[test]
    fn bulk_built_graph_lists_neighbors_by_id() {
        let ids = vec![u64::MAX, 3, 9_007_199_254_740_993];
        let adjacency = vec![vec![1, 2], vec![0], vec![0]];
        let mut graph = Graph::from_sorted_adjacency(ids, adjacency);
        graph.enable_neighbor_listing();
        let neighbors: Vec<u64> = graph.neighbors_after(u64::MAX, None).unwrap().collect();
        assert_eq!(neighbors, [3, 9_007_199_254_740_993]);
        assert_eq!(graph.degree(3), Some(1));
    }

    /// A bulk-built graph keeps every `Graph` invariant and then behaves like
    /// an incrementally built one under further `add_node`, `link`, `cut` and
    /// `connected` calls.
    #[test]
    fn bulk_built_graph_matches_incremental_graph_under_mutation() {
        let mut seed = 0x2545_f491_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _case in 0..200 {
            let n = 1 + next(30) as usize;
            let ids: Vec<NodeId> = (0..n as u64).map(|i| i * 7 + 3).collect();
            let mut incremental = Graph::new();
            for &id in &ids {
                incremental.add_node(id);
            }
            let mut adjacency = vec![Vec::new(); n];
            for _ in 0..next(3 * n as u64 + 1) {
                let (a, b) = (next(n as u64) as usize, next(n as u64) as usize);
                if a != b && incremental.link(ids[a], ids[b]) == Ok(true) {
                    adjacency[a].push(b);
                    adjacency[b].push(a);
                }
            }
            for list in &mut adjacency {
                list.sort_unstable();
            }
            let mut bulk = Graph::from_sorted_adjacency(ids.clone(), adjacency);
            assert_eq!(bulk.node_count(), incremental.node_count());
            assert_eq!(bulk.edge_count(), incremental.edge_count());
            for _ in 0..100 {
                let a = next(n as u64 + 5) * 7 + 3;
                let b = next(n as u64 + 5) * 7 + 3;
                match next(4) {
                    0 => assert_eq!(bulk.add_node(a), incremental.add_node(a)),
                    1 => assert_eq!(bulk.link(a, b), incremental.link(a, b)),
                    2 => assert_eq!(bulk.cut(a, b), incremental.cut(a, b)),
                    _ => assert_eq!(bulk.connected(a, b), incremental.connected(a, b)),
                }
                assert_eq!(bulk.edge_count(), incremental.edge_count());
                assert_eq!(bulk.node_count(), incremental.node_count());
            }
            for list in &bulk.adjacency {
                assert!(list.windows(2).all(|pair| pair[0] < pair[1]));
            }
            bulk.release_isolated_storage(ids[0]);
            assert_eq!(bulk.connected(ids[0], ids[0]), Ok(true));
        }
    }

    #[test]
    fn link_preserves_internal_source_before_target_index_order() {
        let mut graph = Graph::new();
        // Link two fresh nodes: source 42, target 99.
        assert_eq!(graph.link(42, 99), Ok(true));
        // Directly verify the internal index mapping:
        // source (42) must receive internal index 0; target (99) must receive internal index 1.
        assert_eq!(graph.ids.get(&42), Some(&0));
        assert_eq!(graph.ids.get(&99), Some(&1));
        assert_eq!(graph.adjacency[0], vec![1]);
        assert_eq!(graph.adjacency[1], vec![0]);

        // Link with one existing endpoint: source 42 (existing index 0), target 1000 (fresh).
        assert_eq!(graph.link(42, 1000), Ok(true));
        assert_eq!(graph.ids.get(&1000), Some(&2));
        assert_eq!(graph.adjacency[0], vec![1, 2]);
        assert_eq!(graph.adjacency[2], vec![0]);

        // Link with one existing endpoint: source 2000 (fresh), target 99 (existing index 1).
        assert_eq!(graph.link(2000, 99), Ok(true));
        assert_eq!(graph.ids.get(&2000), Some(&3));
        assert_eq!(graph.adjacency[1], vec![0, 3]);
        assert_eq!(graph.adjacency[3], vec![1]);

        // Link with two existing endpoints: 1000 (index 2) and 2000 (index 3).
        assert_eq!(graph.link(1000, 2000), Ok(true));
        assert_eq!(graph.ids.get(&1000), Some(&2));
        assert_eq!(graph.ids.get(&2000), Some(&3));
        assert_eq!(graph.adjacency[2], vec![0, 3]);
        assert_eq!(graph.adjacency[3], vec![1, 2]);
    }
}
