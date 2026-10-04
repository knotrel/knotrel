//! Reusable traversal state must never retain connectivity answers.
use knotrel_core::{BfsWorkspace, Graph, GraphError};

#[test]
fn workspace_survives_early_returns_errors_mutations_and_graph_changes() {
    let mut graph = Graph::new();
    let mut scratch = BfsWorkspace::default();
    for node in [1, 2, 3, 4] {
        graph.add_node(node);
    }
    graph.link(1, 2).unwrap();
    graph.link(2, 3).unwrap();
    assert!(graph.connected_with_workspace(1, 3, &mut scratch).unwrap());
    assert!(graph.connected_with_workspace(1, 1, &mut scratch).unwrap());
    assert_eq!(
        graph.connected_with_workspace(99, 98, &mut scratch),
        Err(GraphError::UnknownNode { node: 99 })
    );
    graph.cut(2, 3).unwrap();
    assert!(!graph.connected_with_workspace(1, 3, &mut scratch).unwrap());
    graph.link(3, u64::MAX).unwrap();
    graph.link(2, u64::MAX).unwrap();
    assert!(graph.connected_with_workspace(1, 3, &mut scratch).unwrap());
    let mut smaller = Graph::new();
    smaller.add_node(2);
    smaller.add_node(1);
    assert!(
        !smaller
            .connected_with_workspace(1, 2, &mut scratch)
            .unwrap()
    );
    assert!(graph.connected_with_workspace(1, 3, &mut scratch).unwrap());
}

#[test]
fn independent_workspaces_allow_concurrent_shared_graph_queries() {
    let mut graph = Graph::new();
    for node in 0..32 {
        graph.add_node(node);
    }
    for node in 0..15 {
        graph.link(node, node + 1).unwrap();
    }
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let graph = &graph;
            scope.spawn(move || {
                let mut scratch = BfsWorkspace::default();
                for a in 0..32 {
                    for b in 0..32 {
                        assert_eq!(
                            graph.connected_with_workspace(a, b, &mut scratch).unwrap(),
                            a == b || (a < 16 && b < 16)
                        );
                    }
                }
            });
        }
    });
}
