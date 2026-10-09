//! Exact, in-memory connectivity for undirected simple graphs.
//!
//! Compact adjacency with stable internal indices supports a growing vertex
//! universe. Queries traverse the graph without caching answers or acquiring
//! internal locks. The original ordered implementation is [`ReferenceGraph`].
//!
//! ```
//! use knotrel_core::Graph;
//! let mut graph = Graph::new();
//! assert!(graph.link(10, 20)?);
//! assert!(graph.link(20, 30)?);
//! assert!(graph.connected(10, 30)?);
//! assert!(graph.cut(20, 30)?);
//! assert!(!graph.connected(10, 30)?);
//! # Ok::<(), knotrel_core::GraphError>(())
//! ```

use std::{
    collections::{BTreeMap, btree_map::Entry},
    error::Error,
    fmt,
};

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

impl Error for GraphError {}

/// An undirected simple graph with compact adjacency and exact BFS queries.
///
/// External IDs map to stable, append-only indices. Each sorted adjacency vector
/// contains unique indices, and every edge appears at both endpoints. Cuts retain
/// vertices. Logical storage is O(V + E). Allocated storage is O(V + H), where H is
/// the sum of per-vertex historical maximum degrees (capacity is retained). No query answers are cached.
#[derive(Debug, Default)]
pub struct Graph {
    ids: BTreeMap<NodeId, usize>,
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
    /// Existing edges are unchanged. O(log V) amortized time; growing the outer
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
    /// O(log V + deg(source) + deg(target)) amortized time, including sorted
    /// vector shifts. A new vertex can trigger an O(V) capacity growth.
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] without creating vertices for equal IDs.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::check_distinct(source, target)?;
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
    /// Worst-case time: O(log V + deg(source) + deg(target)), due to vector shifts.
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] for equal IDs without changing the graph.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::check_distinct(source, target)?;
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
    /// O(log V) endpoint lookup and O(V) local scratch initialization/allocation.
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
        let mut visited = vec![false; self.adjacency.len()];
        let mut queue = Vec::new();
        visited[a] = true;
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
                if !visited[neighbor] {
                    visited[neighbor] = true;
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

    fn check_distinct(source: NodeId, target: NodeId) -> Result<(), GraphError> {
        if source == target {
            Err(GraphError::SelfLoop { node: source })
        } else {
            Ok(())
        }
    }
}
