//! Exact, in-memory connectivity for undirected simple graphs.
//!
//! This ordered-adjacency implementation is a correctness and performance
//! baseline. It recomputes reachability for each query; it is not an optimized
//! dynamic connectivity data structure.
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
    collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry},
    error::Error,
    fmt,
};

/// A vertex identifier.
pub type NodeId = u64;

/// An invalid graph operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphError {
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
            Self::SelfLoop { node } => write!(formatter, "self-loops are not supported: {node}"),
            Self::UnknownNode { node } => write!(formatter, "unknown node: {node}"),
        }
    }
}

impl Error for GraphError {}

/// An undirected simple graph with exact connectivity queries.
///
/// Every stored edge appears in both endpoints' adjacency sets. Sets exclude
/// self-loops and duplicate edges; every endpoint has a vertex entry. Deleting
/// an edge preserves its vertices. Storage is O(V + E), where V and E are the
/// current vertex and undirected edge counts.
#[derive(Debug, Default)]
pub struct Graph {
    adjacency: BTreeMap<NodeId, BTreeSet<NodeId>>,
    edges: usize,
}

impl Graph {
    /// Creates an empty graph in O(1) time.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an isolated vertex, returning `true` only if it was absent.
    ///
    /// An existing vertex and its edges are unchanged. Worst-case time: O(log V).
    pub fn add_node(&mut self, node: NodeId) -> bool {
        if let Entry::Vacant(entry) = self.adjacency.entry(node) {
            entry.insert(BTreeSet::new());
            true
        } else {
            false
        }
    }

    /// Inserts an undirected edge, creating absent endpoints.
    ///
    /// Returns `true` only when the edge is new. Reversed endpoints identify
    /// the same edge. Worst-case time: O(log V).
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] for equal endpoints, without changing the graph.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::check_distinct(source, target)?;
        let inserted = self.adjacency.entry(source).or_default().insert(target);
        self.adjacency.entry(target).or_default().insert(source);
        self.edges += usize::from(inserted);
        Ok(inserted)
    }

    /// Removes an undirected edge while preserving both vertices.
    ///
    /// Returns `false` if the edge or either endpoint is absent. Worst-case
    /// time: O(log V).
    ///
    /// # Errors
    /// Returns [`GraphError::SelfLoop`] for equal endpoints, without changing the graph.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::check_distinct(source, target)?;
        let removed = self
            .adjacency
            .get_mut(&source)
            .is_some_and(|neighbors| neighbors.remove(&target));
        if removed {
            // Symmetric adjacency guarantees the other endpoint exists. Update
            // both directions before returning to preserve the graph invariant.
            self.adjacency
                .get_mut(&target)
                .expect("edge endpoint exists")
                .remove(&source);
            self.edges -= 1;
        }
        Ok(removed)
    }

    /// Reports whether a path exists between two existing vertices.
    ///
    /// An existing vertex connects to itself by the empty path. Worst-case time
    /// is O((V + E) log V); scratch memory is O(V). These are worst-case bounds,
    /// not amortized bounds. Traversal stops early when the target is found.
    ///
    /// # Errors
    /// Returns [`GraphError::UnknownNode`] for an absent endpoint, checking
    /// `source` before `target`.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        for node in [source, target] {
            if !self.adjacency.contains_key(&node) {
                return Err(GraphError::UnknownNode { node });
            }
        }
        if source == target {
            return Ok(true);
        }

        // Breadth-first search: every queued vertex is reachable from source.
        // Marking at enqueue time means a vertex enters the queue at most once,
        // including on cycles. Exhausting the queue explores the entire source
        // component, so an undiscovered target cannot be connected. Ordered
        // sets make membership O(log V) and traversal reproducible.
        let mut visited = BTreeSet::from([source]);
        let mut pending = VecDeque::from([source]);
        while let Some(node) = pending.pop_front() {
            for &neighbor in &self.adjacency[&node] {
                if neighbor == target {
                    return Ok(true);
                }
                if visited.insert(neighbor) {
                    pending.push_back(neighbor);
                }
            }
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

    /// Validates endpoints before mutations so rejected loops cannot create vertices.
    fn check_distinct(source: NodeId, target: NodeId) -> Result<(), GraphError> {
        if source == target {
            Err(GraphError::SelfLoop { node: source })
        } else {
            Ok(())
        }
    }
}
