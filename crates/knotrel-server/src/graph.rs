//! The service's graph: the plain engine, or the lifecycle facade when
//! `KNOTREL_TOPOLOGY_LIFECYCLE` is enabled (M10).
//!
//! The choice is fixed at startup. In plain mode the service holds the same
//! [`ConnectivityGraph`] as before, with no topology catalog; every legacy
//! operation dispatches through one enum match.

use knotrel_core::{
    ConnectivityGraph, EngineConfig, GraphError, GraphLimits, LifecycleError, LifecycleGraph,
};

/// Lifecycle counts, reported only by a lifecycle-enabled graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct LifecycleCounts {
    pub(crate) active_node_count: usize,
    pub(crate) enabled_edge_count: usize,
    pub(crate) effective_edge_count: usize,
}

/// The graph owned by the service, under the graph lock.
#[derive(Debug)]
pub(crate) enum ServiceGraph {
    /// Lifecycle support disabled: the plain engine.
    Plain(ConnectivityGraph),
    /// Lifecycle support enabled: registered topology with flags.
    Lifecycle(LifecycleGraph),
}

impl ServiceGraph {
    /// Creates an empty graph for the startup settings.
    ///
    /// `lifecycle` with an unsupported engine is unreachable:
    /// [`ServerConfig::with_topology_lifecycle`](crate::ServerConfig::with_topology_lifecycle)
    /// rejects it and is the only way to enable lifecycle support.
    pub(crate) fn empty(engine: EngineConfig, limits: GraphLimits, lifecycle: bool) -> Self {
        if lifecycle {
            Self::Lifecycle(
                LifecycleGraph::with_limits(engine, limits)
                    .expect("ServerConfig admits lifecycle support only for supported engines"),
            )
        } else {
            Self::Plain(ConnectivityGraph::with_limits(engine, limits))
        }
    }

    /// Whether lifecycle operations are supported.
    pub(crate) fn is_lifecycle(&self) -> bool {
        matches!(self, Self::Lifecycle(_))
    }

    pub(crate) fn engine(&self) -> EngineConfig {
        match self {
            Self::Plain(graph) => graph.engine(),
            Self::Lifecycle(graph) => graph.engine(),
        }
    }

    pub(crate) fn limits(&self) -> GraphLimits {
        match self {
            Self::Plain(graph) => graph.limits(),
            Self::Lifecycle(graph) => graph.limits(),
        }
    }

    /// Registered nodes (all nodes in plain mode).
    pub(crate) fn node_count(&self) -> usize {
        match self {
            Self::Plain(graph) => graph.node_count(),
            Self::Lifecycle(graph) => graph.node_count(),
        }
    }

    /// Registered edges (all edges in plain mode).
    pub(crate) fn edge_count(&self) -> usize {
        match self {
            Self::Plain(graph) => graph.edge_count(),
            Self::Lifecycle(graph) => graph.edge_count(),
        }
    }

    /// Lifecycle counts; `None` in plain mode. `O(1)`.
    pub(crate) fn lifecycle_counts(&self) -> Option<LifecycleCounts> {
        match self {
            Self::Plain(_) => None,
            Self::Lifecycle(graph) => Some(LifecycleCounts {
                active_node_count: graph.active_node_count(),
                enabled_edge_count: graph.enabled_edge_count(),
                effective_edge_count: graph.effective_edge_count(),
            }),
        }
    }

    pub(crate) fn add_node(&mut self, node: u64) -> Result<bool, GraphError> {
        match self {
            Self::Plain(graph) => graph.add_node(node),
            Self::Lifecycle(graph) => graph.add_node(node),
        }
    }

    pub(crate) fn link(&mut self, source: u64, target: u64) -> Result<bool, GraphError> {
        match self {
            Self::Plain(graph) => graph.link(source, target),
            Self::Lifecycle(graph) => graph.link(source, target),
        }
    }

    pub(crate) fn cut(&mut self, source: u64, target: u64) -> Result<bool, GraphError> {
        match self {
            Self::Plain(graph) => graph.cut(source, target),
            Self::Lifecycle(graph) => graph.cut(source, target),
        }
    }

    pub(crate) fn connected(&self, source: u64, target: u64) -> Result<bool, GraphError> {
        match self {
            Self::Plain(graph) => graph.connected(source, target),
            Self::Lifecycle(graph) => graph.connected(source, target),
        }
    }

    /// The lifecycle facade. Callers reject lifecycle requests for a plain
    /// graph before execution, so `None` is a request-validation bug that
    /// surfaces as an error, never as a silent no-op.
    pub(crate) fn lifecycle_mut(&mut self) -> Result<&mut LifecycleGraph, LifecycleUnsupported> {
        match self {
            Self::Plain(_) => Err(LifecycleUnsupported),
            Self::Lifecycle(graph) => Ok(graph),
        }
    }

    /// Shared-reference variant of [`lifecycle_mut`](Self::lifecycle_mut).
    pub(crate) fn lifecycle(&self) -> Result<&LifecycleGraph, LifecycleUnsupported> {
        match self {
            Self::Plain(_) => Err(LifecycleUnsupported),
            Self::Lifecycle(graph) => Ok(graph),
        }
    }
}

impl From<ConnectivityGraph> for ServiceGraph {
    fn from(graph: ConnectivityGraph) -> Self {
        Self::Plain(graph)
    }
}

/// A lifecycle operation reached a plain graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LifecycleUnsupported;

/// Result of a lifecycle mutation in service terms.
pub(crate) type LifecycleResult = Result<bool, LifecycleFailure>;

/// Why a lifecycle mutation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleFailure {
    /// The graph has no lifecycle support.
    Unsupported,
    /// A domain error from the facade.
    Domain(LifecycleError),
}

impl From<LifecycleUnsupported> for LifecycleFailure {
    fn from(_: LifecycleUnsupported) -> Self {
        Self::Unsupported
    }
}

impl From<LifecycleError> for LifecycleFailure {
    fn from(error: LifecycleError) -> Self {
        Self::Domain(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain graph has no lifecycle facade; a lifecycle graph routes the
    /// plain mutations to its registered topology.
    #[test]
    fn plain_and_lifecycle_variants_route_operations() {
        let mut plain =
            ServiceGraph::empty(EngineConfig::CompactBfs, GraphLimits::default(), false);
        assert_eq!(plain.lifecycle().err(), Some(LifecycleUnsupported));
        assert_eq!(plain.lifecycle_mut().err(), Some(LifecycleUnsupported));
        assert_eq!(
            LifecycleFailure::from(LifecycleUnsupported),
            LifecycleFailure::Unsupported
        );
        let mut lifecycle =
            ServiceGraph::empty(EngineConfig::CompactBfs, GraphLimits::default(), true);
        assert_eq!(lifecycle.link(1, 2), Ok(true));
        assert_eq!(lifecycle.cut(1, 2), Ok(true));
        assert_eq!(lifecycle.connected(1, 2), Ok(false));
        assert!(lifecycle.lifecycle().is_ok());
        let _ = plain.link(1, 2);
    }
}
