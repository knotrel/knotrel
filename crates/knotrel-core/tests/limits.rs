//! Cardinality limits are checked before any mutation in every engine.
use knotrel_core::{ConnectivityGraph, EngineConfig, GraphError, GraphLimits};

#[test]
fn limits_are_atomic_idempotent_and_reusable_after_cut() {
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        let mut g = ConnectivityGraph::with_limits(
            engine,
            GraphLimits {
                max_nodes: Some(3),
                max_edges: Some(1),
            },
        );
        assert_eq!(g.link(1, 2), Ok(true));
        assert_eq!(g.link(2, 1), Ok(false));
        assert_eq!(
            g.link(2, 3),
            Err(GraphError::EdgeLimitExceeded { limit: 1 })
        );
        assert_eq!(g.connected(3, 3), Err(GraphError::UnknownNode { node: 3 }));
        assert_eq!(
            g.link(4, 5),
            Err(GraphError::NodeLimitExceeded { limit: 3 })
        );
        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.add_node(1), Ok(false));
        assert_eq!(g.cut(1, 2), Ok(true));
        assert_eq!(g.link(2, 3), Ok(true));
        assert_eq!(
            g.add_node(4),
            Err(GraphError::NodeLimitExceeded { limit: 3 })
        );
        assert_eq!(g.cut(2, 3), Ok(true));
        assert_eq!(g.node_count(), 3); // Cuts do not release node capacity.
        assert_eq!(g.link(1, 3), Ok(true));
    }
}

#[test]
fn zero_capacity_preserves_validation_and_never_creates_endpoints() {
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        let mut g = ConnectivityGraph::with_limits(
            engine,
            GraphLimits {
                max_nodes: Some(0),
                max_edges: Some(0),
            },
        );
        assert_eq!(g.link(9, 9), Err(GraphError::SelfLoop { node: 9 }));
        assert_eq!(
            g.add_node(9),
            Err(GraphError::NodeLimitExceeded { limit: 0 })
        );
        assert_eq!(
            g.link(8, 9),
            Err(GraphError::NodeLimitExceeded { limit: 0 })
        );
        assert_eq!(g.node_count(), 0);
        let mut g = ConnectivityGraph::with_limits(
            engine,
            GraphLimits {
                max_nodes: None,
                max_edges: Some(0),
            },
        );
        assert_eq!(
            g.link(8, 9),
            Err(GraphError::EdgeLimitExceeded { limit: 0 })
        );
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.add_node(u64::MAX), Ok(true));
    }
}
