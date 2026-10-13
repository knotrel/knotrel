//! Explicit engine selection, independent of any transport or environment.

use crate::{ForestGraph, Graph, GraphError, HdtGraph, NodeId};

/// Immutable cardinality ceilings for one graph instance.
///
/// `None` is unlimited (the default); zero forbids new entries. Cuts retain
/// vertices. These limits do not bound allocator capacity or resident memory.
/// Enforced by [`ConnectivityGraph`]; raw algorithm types remain unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphLimits {
    /// Maximum registered vertices, including isolated vertices.
    pub max_nodes: Option<usize>,
    /// Maximum live undirected edges.
    pub max_edges: Option<usize>,
}

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
    limits: GraphLimits,
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
            limits: GraphLimits::default(),
        }
    }
}

impl ConnectivityGraph {
    /// Creates an empty graph. Every typed configuration is supported.
    pub fn new(config: EngineConfig) -> Self {
        Self::with_limits(config, GraphLimits::default())
    }

    /// Creates an empty graph with immutable cardinality ceilings.
    pub fn with_limits(config: EngineConfig, limits: GraphLimits) -> Self {
        Self {
            limits,
            backend: match config {
                EngineConfig::CompactBfs => Backend::Compact(Graph::new()),
                EngineConfig::EulerTour => Backend::EulerTour(ForestGraph::new()),
                EngineConfig::Hdt => Backend::Hdt(HdtGraph::new()),
            },
        }
    }

    /// Builds an owned graph by registering `nodes`, then inserting `edges`,
    /// in the supplied order using [`Self::add_node`] and [`Self::link`].
    ///
    /// Duplicate nodes and duplicate/reversed edges are no-ops, including at
    /// capacity. Edges create missing endpoints; `nodes` is not a closed universe.
    /// Empty inputs, isolated vertices and disconnected components are valid.
    /// The returned graph supports the same subsequent dynamic operations and
    /// retains the selected engine and limits.
    ///
    /// Construction is atomic with respect to publication: on error the local
    /// graph is dropped, not returned partially built. This is not an in-place
    /// batch or snapshot restore, and returns no per-item change flags.
    ///
    /// Work and allocation behavior are those of the ordered incremental calls;
    /// the constructor adds O(nodes.len() + edges.len()) iteration overhead and
    /// O(1) auxiliary space. It does not reserve, sort or stage the input.
    /// No performance improvement is promised.
    ///
    /// # Errors
    ///
    /// Returns the first [`GraphError`] from the ordered calls: all explicit
    /// nodes precede every edge; each edge checks self-loop, node limit, then
    /// edge limit. Later invalid input is not inspected ahead of that error.
    /// Limits bound cardinality, not memory. As with incremental construction,
    /// allocation failure is not a recoverable `GraphError`; collection capacity
    /// overflow may panic and memory exhaustion may abort.
    ///
    /// ```
    /// use knotrel_core::{ConnectivityGraph, EngineConfig, GraphLimits};
    /// let mut graph = ConnectivityGraph::try_from_edges(
    ///     EngineConfig::EulerTour,
    ///     GraphLimits::default(),
    ///     &[99, 99],
    ///     &[(1, 2), (2, 1), (2, 3)],
    /// )?;
    /// assert_eq!(graph.node_count(), 4);
    /// assert_eq!(graph.edge_count(), 2);
    /// assert!(graph.connected(1, 3)?);
    /// assert!(!graph.connected(99, 1)?);
    /// assert!(graph.cut(2, 3)?);
    /// assert!(!graph.connected(1, 3)?);
    /// # Ok::<(), knotrel_core::GraphError>(())
    /// ```
    pub fn try_from_edges(
        engine: EngineConfig,
        limits: GraphLimits,
        nodes: &[NodeId],
        edges: &[(NodeId, NodeId)],
    ) -> Result<Self, GraphError> {
        let mut graph = Self::with_limits(engine, limits);
        for &node in nodes {
            graph.add_node(node)?;
        }
        for &(source, target) in edges {
            graph.link(source, target)?;
        }
        Ok(graph)
    }

    /// Returns the immutable limits configured for this instance.
    pub fn limits(&self) -> GraphLimits {
        self.limits
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
    /// # Errors
    /// Returns [`GraphError::NodeLimitExceeded`] without mutation at capacity.
    /// An existing vertex always returns `Ok(false)`.
    pub fn add_node(&mut self, node: NodeId) -> Result<bool, GraphError> {
        if let Some(limit) = self.limits.max_nodes {
            if self.connected(node, node).is_ok() {
                return Ok(false);
            }
            if self.node_count() >= limit {
                return Err(GraphError::NodeLimitExceeded { limit });
            }
        }
        Ok(match &mut self.backend {
            Backend::Compact(graph) => graph.add_node(node),
            Backend::EulerTour(graph) => graph.add_node(node),
            Backend::Hdt(graph) => graph.add_node(node),
        })
    }

    /// Adds an undirected edge, creating absent endpoints.
    ///
    /// Returns whether the edge was absent. A self-loop returns
    /// [`GraphError::SelfLoop`] without creating its endpoint.
    /// Limit checks precede all mutation; node limits take precedence over edge
    /// limits. Duplicate edges succeed unchanged even at capacity. Each check
    /// uses ordered index lookups, never a graph traversal or speculative insert.
    /// # Errors
    /// Self-loops or cardinality violations leave the entire graph unchanged.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        if source == target {
            return Err(GraphError::SelfLoop { node: source });
        }
        if let Some(limit) = self.limits.max_nodes {
            let missing = usize::from(self.connected(source, source).is_err())
                + usize::from(self.connected(target, target).is_err());
            if missing > limit.saturating_sub(self.node_count()) {
                return Err(GraphError::NodeLimitExceeded { limit });
            }
        }
        if let Some(limit) = self.limits.max_edges
            && self.edge_count() >= limit
        {
            let exists = match &self.backend {
                Backend::Compact(g) => g.contains_edge(source, target),
                Backend::EulerTour(g) => g.contains_edge(source, target),
                Backend::Hdt(g) => g.contains_edge(source, target),
            };
            return if exists {
                Ok(false)
            } else {
                Err(GraphError::EdgeLimitExceeded { limit })
            };
        }
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
