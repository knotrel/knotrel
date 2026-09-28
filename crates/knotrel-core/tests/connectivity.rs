//! Behavioral and differential tests for exact dynamic connectivity.

use knotrel_core::{Graph, GraphError};

#[test]
fn updates_are_undirected_and_idempotent() {
    let mut graph = Graph::new();
    assert_eq!((graph.node_count(), graph.edge_count()), (0, 0));
    assert_eq!(graph.link(1, u64::MAX), Ok(true));
    assert_eq!(graph.link(u64::MAX, 1), Ok(false));
    assert_eq!(graph.connected(u64::MAX, 1), Ok(true));
    assert_eq!((graph.node_count(), graph.edge_count()), (2, 1));
    assert_eq!(graph.cut(u64::MAX, 1), Ok(true));
    assert_eq!(graph.cut(1, u64::MAX), Ok(false));
    assert_eq!(graph.cut(1, 99), Ok(false));
    assert_eq!(graph.connected(1, u64::MAX), Ok(false));
    assert_eq!((graph.node_count(), graph.edge_count()), (2, 0));
}

#[test]
fn rejects_loops_without_creating_vertices() {
    let mut graph = Graph::new();
    assert_eq!(graph.link(7, 7), Err(GraphError::SelfLoop { node: 7 }));
    assert_eq!(graph.cut(7, 7), Err(GraphError::SelfLoop { node: 7 }));
    assert_eq!(graph.node_count(), 0);
}

#[test]
fn distinguishes_unknown_and_isolated_vertices() {
    let mut graph = Graph::new();
    assert_eq!(
        graph.connected(0, 0),
        Err(GraphError::UnknownNode { node: 0 })
    );
    assert!(graph.add_node(0));
    assert!(!graph.add_node(0));
    assert!(graph.add_node(9));
    assert_eq!(graph.connected(0, 0), Ok(true));
    assert_eq!(graph.connected(0, 9), Ok(false));
    assert_eq!(
        graph.connected(0, 3),
        Err(GraphError::UnknownNode { node: 3 })
    );
}

#[test]
fn cycle_edge_and_bridge_deletion_have_different_effects() {
    let mut graph = Graph::new();
    for (a, b) in [(1, 2), (2, 3), (3, 1), (3, 4)] {
        graph.link(a, b).unwrap();
    }
    assert_eq!(graph.cut(1, 2), Ok(true));
    assert_eq!(graph.connected(1, 2), Ok(true));
    assert_eq!(graph.cut(3, 4), Ok(true));
    assert_eq!(graph.connected(1, 4), Ok(false));
    assert_eq!(graph.link(2, 4), Ok(true));
    assert_eq!(graph.connected(1, 4), Ok(true));
}

#[test]
fn updates_match_independent_transitive_closure() {
    const N: usize = 9;
    let mut graph = Graph::new();
    let mut edges = [[false; N]; N];
    for node in 0..N {
        graph.add_node(node as u64);
    }
    let mut seed = 0x006b_6e6f_7472_656c_u64;
    for step in 0..400 {
        // Fixed LCG yields reproducible operations; the oracle below uses matrix
        // transitive closure, independent of adjacency-list traversal.
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = ((seed >> 32) as usize) % N;
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let b = ((seed >> 32) as usize) % N;
        if a == b {
            continue;
        }
        if step % 3 == 0 {
            assert_eq!(graph.cut(a as u64, b as u64), Ok(edges[a][b]));
            edges[a][b] = false;
        } else {
            assert_eq!(graph.link(a as u64, b as u64), Ok(!edges[a][b]));
            edges[a][b] = true;
        }
        edges[b][a] = edges[a][b];
        let mut closure = edges;
        for (i, row) in closure.iter_mut().enumerate() {
            row[i] = true;
        }
        for k in 0..N {
            for i in 0..N {
                for j in 0..N {
                    closure[i][j] |= closure[i][k] && closure[k][j];
                }
            }
        }
        for (i, row) in closure.iter().enumerate() {
            for (j, expected) in row.iter().enumerate() {
                assert_eq!(
                    graph.connected(i as u64, j as u64),
                    Ok(*expected),
                    "step={step}, pair=({i},{j})"
                );
            }
        }
        assert_eq!(
            graph.edge_count(),
            edges.iter().flatten().filter(|edge| **edge).count() / 2
        );
    }
}

#[test]
fn growth_preserves_indices_isolation_and_shared_queries() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Graph>();
    let mut graph = Graph::new();
    let ids: Vec<_> = (0..257).map(|i| u64::MAX - i * 7919).collect();
    graph.add_node(0);
    for pair in ids.windows(2) {
        assert_eq!(graph.link(pair[0], pair[1]), Ok(true));
        assert_eq!(graph.connected(ids[0], pair[1]), Ok(true));
        assert_eq!(graph.connected(0, pair[1]), Ok(false));
    }
    for pair in ids.windows(2).step_by(3) {
        assert_eq!(graph.cut(pair[1], pair[0]), Ok(true));
        assert_eq!(graph.connected(pair[0], pair[1]), Ok(false));
        assert_eq!(graph.link(pair[1], pair[0]), Ok(true));
        assert_eq!(graph.connected(pair[0], pair[1]), Ok(true));
    }
    assert_eq!((graph.node_count(), graph.edge_count()), (258, 256));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let graph = &graph;
            let ids = &ids;
            scope.spawn(move || {
                for _ in 0..20 {
                    assert_eq!(graph.connected(ids[0], ids[256]), Ok(true));
                    assert_eq!(graph.connected(0, ids[0]), Ok(false));
                }
            });
        }
    });
}

#[test]
fn arbitrary_growing_histories_match_original_reference() {
    let mut compact = Graph::new();
    let mut reference = knotrel_core::ReferenceGraph::new();
    let mut seed = 913_u64;
    for step in 0..1500 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let universe = 2 + step / 30;
        let a = u64::MAX - ((seed >> 32) % universe) * 7919;
        let b = u64::MAX - ((seed >> 48) % universe) * 7919;
        match step % 5 {
            0 => assert_eq!(compact.add_node(a), reference.add_node(a)),
            1 | 2 => assert_eq!(compact.link(a, b), reference.link(a, b)),
            _ => assert_eq!(compact.cut(a, b), reference.cut(a, b)),
        }
        for (source, target) in [(a, b), (b, a), (0, a), (a, a), (u64::MAX, b)] {
            for _ in 0..2 {
                assert_eq!(
                    compact.connected(source, target),
                    reference.connected(source, target),
                    "step {step}"
                );
            }
        }
        assert_eq!(compact.node_count(), reference.node_count());
        assert_eq!(compact.edge_count(), reference.edge_count());
    }
}
