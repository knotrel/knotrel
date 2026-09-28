//! Behavior of an explicitly selected connectivity engine.

use knotrel_core::{ConnectivityGraph, EngineConfig, Graph, GraphError};

#[test]
fn selected_engines_preserve_exact_connectivity_after_mutations() {
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        let mut graph = ConnectivityGraph::new(engine);
        assert_eq!(graph.engine(), engine);
        assert!(graph.add_node(u64::MAX));
        assert!(!graph.add_node(u64::MAX));
        assert_eq!(
            graph.connected(8, 9),
            Err(GraphError::UnknownNode { node: 8 })
        );
        assert_eq!(graph.link(8, 8), Err(GraphError::SelfLoop { node: 8 }));
        assert_eq!(graph.node_count(), 1);
        for (a, b) in [(0, 1), (1, 2), (2, 0), (2, u64::MAX)] {
            assert_eq!(graph.link(a, b), Ok(true));
        }
        assert_eq!(graph.link(1, 0), Ok(false));
        assert_eq!(graph.edge_count(), 4);
        assert_eq!(graph.cut(0, 1), Ok(true));
        assert_eq!(graph.connected(0, u64::MAX), Ok(true));
        assert_eq!(graph.cut(0, 2), Ok(true));
        for _ in 0..3 {
            assert_eq!(graph.connected(0, u64::MAX), Ok(false));
        }
        assert_eq!(graph.link(0, u64::MAX), Ok(true));
        assert_eq!(graph.connected(0, 1), Ok(true));
        assert_eq!(graph.node_count(), 4);
        assert_eq!(graph.edge_count(), 3);
    }
}

#[test]
fn wrapping_a_populated_graph_preserves_isolated_nodes_and_edges() {
    let mut original = Graph::new();
    original.add_node(99);
    original.link(1, 2).unwrap();
    let graph = ConnectivityGraph::from(original);
    assert_eq!(graph.engine(), EngineConfig::CompactBfs);
    assert_eq!(graph.connected(99, 99), Ok(true));
    assert_eq!(graph.connected(1, 2), Ok(true));
    assert_eq!(graph.connected(99, 1), Ok(false));
}
