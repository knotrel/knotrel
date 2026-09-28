//! Exact dynamic-forest behavior, independent of the sequence representation.
use knotrel_core::{ForestGraph, GraphError, ReferenceGraph};

#[test]
fn forest_replaces_cycles_preserves_bridges_and_recycles_handles() {
    let mut g = ForestGraph::new();
    assert_eq!(g.link(9, 9), Err(GraphError::SelfLoop { node: 9 }));
    assert_eq!(g.node_count(), 0);
    for _ in 0..200 {
        for (a, b) in [(0, 1), (1, 2), (2, 0), (2, u64::MAX)] {
            assert_eq!(g.link(a, b), Ok(true));
        }
        assert_eq!(g.link(1, 0), Ok(false));
        assert_eq!(g.cut(0, 1), Ok(true));
        assert_eq!(g.connected(0, 1), Ok(true));
        assert_eq!(g.cut(2, u64::MAX), Ok(true));
        assert_eq!(g.connected(0, u64::MAX), Ok(false));
        assert_eq!(g.cut(1, 2), Ok(true));
        assert_eq!(g.cut(2, 0), Ok(true));
        assert_eq!(g.cut(2, 0), Ok(false));
        assert_eq!(g.edge_count(), 0);
        assert_eq!(g.connected(u64::MAX, u64::MAX), Ok(true));
    }
    assert_eq!(g.node_count(), 4);
    assert!(g.stats().replacements >= 200);
}

#[test]
fn growing_random_histories_match_matrix_and_original() {
    const N: usize = 24;
    let ids: Vec<_> = (0..N).map(|i| u64::MAX - i as u64 * 997).collect();
    let mut g = ForestGraph::new();
    let mut reference = ReferenceGraph::new();
    let mut adjacency = [[false; N]; N];
    let mut present = [false; N];
    let mut state = 31_u64;
    for step in 0..2000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let limit = N.min(2 + step / 30);
        let a = (state >> 32) as usize % limit;
        let b = (state >> 48) as usize % limit;
        match step % 5 {
            0 => {
                assert_eq!(g.add_node(ids[a]), reference.add_node(ids[a]));
                present[a] = true;
            }
            1 | 2 => {
                assert_eq!(g.link(ids[a], ids[b]), reference.link(ids[a], ids[b]));
                if a != b {
                    adjacency[a][b] = true;
                    adjacency[b][a] = true;
                    present[a] = true;
                    present[b] = true;
                }
            }
            _ => {
                assert_eq!(g.cut(ids[a], ids[b]), reference.cut(ids[a], ids[b]));
                if a != b {
                    adjacency[a][b] = false;
                    adjacency[b][a] = false;
                }
            }
        }
        let mut closure = adjacency;
        for (i, row) in closure.iter_mut().enumerate() {
            row[i] = present[i];
        }
        for k in 0..N {
            for i in 0..N {
                for j in 0..N {
                    closure[i][j] |= closure[i][k] && closure[k][j];
                }
            }
        }
        for (i, row) in closure.iter().enumerate() {
            for (j, &value) in row.iter().enumerate() {
                let expected = if !present[i] {
                    Err(GraphError::UnknownNode { node: ids[i] })
                } else if !present[j] {
                    Err(GraphError::UnknownNode { node: ids[j] })
                } else {
                    Ok(value)
                };
                assert_eq!(
                    g.connected(ids[i], ids[j]),
                    expected,
                    "step {step}, pair {i},{j}"
                );
            }
        }
        assert_eq!(g.edge_count(), reference.edge_count());
        assert_eq!(g.node_count(), reference.node_count());
    }
}

#[test]
fn shared_queries_on_large_growing_forest() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<ForestGraph>();
    let mut g = ForestGraph::new();
    for i in 0..10_000 {
        g.link(i, i + 1).unwrap();
    }
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let g = &g;
            scope.spawn(move || {
                for i in (0..10_000).step_by(101) {
                    assert_eq!(g.connected(i, 10_000), Ok(true));
                }
            });
        }
    });
    g.cut(4999, 5000).unwrap();
    assert_eq!(g.connected(0, 10_000), Ok(false));
    g.link(0, 10_000).unwrap();
    assert_eq!(g.connected(4999, 5000), Ok(true));
}

#[test]
fn bridge_without_candidates_skips_vertex_enumeration() {
    let mut graph = ForestGraph::new();
    for i in 0..1000 {
        graph.link(i, i + 1).unwrap();
    }
    graph.reset_stats();
    graph.cut(499, 500).unwrap();
    assert_eq!(graph.connected(0, 1000), Ok(false));
    assert_eq!(graph.stats().scanned_vertices, 0);
    assert_eq!(graph.stats().candidate_edges, 0);
}

#[test]
fn replacement_stops_before_enumerating_empty_vertices() {
    let mut graph = ForestGraph::new();
    for i in 0..1000 {
        graph.link(i, i + 1).unwrap();
    }
    graph.link(0, 1000).unwrap();
    graph.reset_stats();
    graph.cut(499, 500).unwrap();
    assert_eq!(graph.connected(0, 1000), Ok(true));
    assert_eq!(graph.stats().scanned_vertices, 1);
    assert_eq!(graph.stats().candidate_edges, 1);
    assert_eq!(graph.stats().replacements, 1);
    // Promoted candidates must be removed from the aggregates on both sides.
    graph.reset_stats();
    graph.cut(0, 1000).unwrap();
    assert_eq!(graph.connected(0, 1000), Ok(false));
    assert_eq!(graph.stats().scanned_vertices, 0);
}

#[test]
fn deleting_last_non_tree_edge_clears_candidate_flags() {
    let mut graph = ForestGraph::new();
    for (a, b) in [(0, 1), (1, 2), (2, 3), (0, 3)] {
        graph.link(a, b).unwrap();
    }
    graph.cut(3, 0).unwrap();
    graph.reset_stats();
    graph.cut(1, 2).unwrap();
    assert_eq!(graph.connected(0, 3), Ok(false));
    assert_eq!(graph.stats().scanned_vertices, 0);
}
