//! Explicit engine selection, independent of any transport or environment.

use crate::{ForestGraph, Graph, GraphError, HdtGraph, NodeId};

/// The implementation chosen when creating a graph.
///
/// Selection is immutable for the lifetime of a [`ConnectivityGraph`].
/// No automatic switching or answer cache is introduced by this configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EngineConfig {
    /// Compact adjacency with a fresh BFS for each connectivity query.
    #[default]
    CompactBfs,
    /// Experimental Euler-tour forest with candidate-pruned replacement search.
    ///
    /// This is not HDT and does not provide polylogarithmic update bounds.
    EulerTour,
    /// Experimental deterministic HDT with level-based replacement search.
    Hdt,
}

impl EngineConfig {
    /// Returns the stable external selector for this engine.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompactBfs => "compact-bfs",
            Self::EulerTour => "ett",
            Self::Hdt => "hdt",
        }
    }

    /// Whether this engine is experimental rather than the default backend.
    pub fn is_experimental(self) -> bool {
        matches!(self, Self::EulerTour | Self::Hdt)
    }
}

/// An owned graph with an explicitly selected, immutable implementation.
///
/// All engines preserve the [`Graph`] operation contract, including growing
/// `u64` IDs and retained vertices after cuts. This wrapper adds constant-time
/// enum dispatch; operation costs are those of [`Graph`], [`ForestGraph`] or [`HdtGraph`].
/// It reads no environment variables or files and performs no internal locking.
///
/// ```
/// use knotrel_core::{ConnectivityGraph, EngineConfig};
/// let mut graph = ConnectivityGraph::new(EngineConfig::EulerTour);
/// graph.link(1, u64::MAX)?;
/// assert!(graph.connected(1, u64::MAX)?);
/// # Ok::<(), knotrel_core::GraphError>(())
/// ```
#[derive(Debug)]
pub struct ConnectivityGraph {
    backend: Backend,
}

#[derive(Debug)]
enum Backend {
    Compact(Graph),
    EulerTour(ForestGraph),
    Hdt(HdtGraph),
}

impl Default for ConnectivityGraph {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

impl From<Graph> for ConnectivityGraph {
    /// Takes ownership without rebuilding or losing existing graph state.
    fn from(graph: Graph) -> Self {
        Self {
            backend: Backend::Compact(graph),
        }
    }
}

impl ConnectivityGraph {
    /// Creates an empty graph. Every typed configuration is supported.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            backend: match config {
                EngineConfig::CompactBfs => Backend::Compact(Graph::new()),
                EngineConfig::EulerTour => Backend::EulerTour(ForestGraph::new()),
                EngineConfig::Hdt => Backend::Hdt(HdtGraph::new()),
            },
        }
    }

    /// Returns the engine that actually owns this graph's state.
    pub fn engine(&self) -> EngineConfig {
        match &self.backend {
            Backend::Compact(_) => EngineConfig::CompactBfs,
            Backend::EulerTour(_) => EngineConfig::EulerTour,
            Backend::Hdt(_) => EngineConfig::Hdt,
        }
    }

    /// Adds a vertex; returns whether it was absent.
    pub fn add_node(&mut self, node: NodeId) -> bool {
        match &mut self.backend {
            Backend::Compact(graph) => graph.add_node(node),
            Backend::EulerTour(graph) => graph.add_node(node),
            Backend::Hdt(graph) => graph.add_node(node),
        }
    }

    /// Adds an undirected edge, creating absent endpoints.
    ///
    /// Returns whether the edge was absent. A self-loop returns
    /// [`GraphError::SelfLoop`] without creating its endpoint.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        match &mut self.backend {
            Backend::Compact(graph) => graph.link(source, target),
            Backend::EulerTour(graph) => graph.link(source, target),
            Backend::Hdt(graph) => graph.link(source, target),
        }
    }

    /// Removes an edge while retaining vertices.
    ///
    /// Returns false for an absent edge or endpoint. A self-loop returns
    /// [`GraphError::SelfLoop`].
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        match &mut self.backend {
            Backend::Compact(graph) => graph.cut(source, target),
            Backend::EulerTour(graph) => graph.cut(source, target),
            Backend::Hdt(graph) => graph.cut(source, target),
        }
    }

    /// Returns exact connectivity in the current graph.
    ///
    /// Returns [`GraphError::UnknownNode`] for an absent endpoint, checking
    /// the source first. A known vertex is connected to itself.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        match &self.backend {
            Backend::Compact(graph) => graph.connected(source, target),
            Backend::EulerTour(graph) => graph.connected(source, target),
            Backend::Hdt(graph) => graph.connected(source, target),
        }
    }

    /// Returns the number of registered vertices.
    pub fn node_count(&self) -> usize {
        match &self.backend {
            Backend::Compact(graph) => graph.node_count(),
            Backend::EulerTour(graph) => graph.node_count(),
            Backend::Hdt(graph) => graph.node_count(),
        }
    }

    /// Returns the number of undirected edges.
    pub fn edge_count(&self) -> usize {
        match &self.backend {
            Backend::Compact(graph) => graph.edge_count(),
            Backend::EulerTour(graph) => graph.edge_count(),
            Backend::Hdt(graph) => graph.edge_count(),
        }
    }
}
