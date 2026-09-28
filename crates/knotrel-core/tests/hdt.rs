//! Exact dynamic-forest behavior, independent of the sequence representation.
use knotrel_core::{GraphError, HdtGraph, ReferenceGraph};

#[test]
fn forest_replaces_cycles_preserves_bridges_and_recycles_handles() {
    let mut g = HdtGraph::new();
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
    let mut g = HdtGraph::new();
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
    send_sync::<HdtGraph>();
    let mut g = HdtGraph::new();
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
fn stable_dense_bridge_cuts_do_not_rescan_promoted_internal_edges() {
    let mut graph = HdtGraph::new();
    for base in [0, 16] {
        for a in base..base + 16 {
            for b in a + 1..base + 16 {
                graph.link(a, b).unwrap();
            }
        }
    }
    graph.link(0, 16).unwrap();
    graph.cut(0, 16).unwrap();
    assert!(graph.stats().non_tree_promotions > 0);
    // After each side has been promoted, stable internal edges are not revisited
    // by repeated level-zero bridge deletion, regardless of tie selection.
    graph.link(0, 16).unwrap();
    graph.cut(0, 16).unwrap();
    graph.reset_stats();
    for _ in 0..50 {
        graph.link(0, 16).unwrap();
        assert_eq!(graph.connected(4, 25), Ok(true));
        graph.cut(0, 16).unwrap();
        assert_eq!(graph.connected(4, 25), Ok(false));
    }
    assert_eq!(graph.stats().candidate_edges, 0);
    // Growing across several power-of-two boundaries must retain promoted state.
    for id in 32..130 {
        graph.add_node(id);
    }
    assert_eq!(graph.connected(0, 15), Ok(true));
    assert_eq!(graph.connected(0, 16), Ok(false));
    graph.link(15, 16).unwrap();
    assert_eq!(graph.connected(0, 31), Ok(true));
    graph.cut(15, 16).unwrap();
    assert_eq!(graph.connected(0, 31), Ok(false));
}

#[test]
fn candidate_free_cuts_do_not_promote_tree_edges() {
    let mut graph = HdtGraph::new();
    for v in 0..1024 {
        graph.link(v, v + 1).unwrap();
    }
    graph.reset_stats();
    graph.cut(511, 512).unwrap();
    assert_eq!(graph.connected(0, 1024), Ok(false));
    assert_eq!(graph.stats().candidate_edges, 0);
    assert_eq!(graph.stats().tree_promotions, 0);
    graph.link(0, 1024).unwrap();
    assert_eq!(graph.connected(511, 512), Ok(true));
}

#[test]
fn candidates_only_on_larger_side_do_not_trigger_promotions() {
    let mut graph = HdtGraph::new();
    for v in 0..7 {
        graph.link(v, v + 1).unwrap();
    }
    graph.link(2, 7).unwrap();
    graph.reset_stats();
    graph.cut(1, 2).unwrap();
    assert_eq!(graph.connected(0, 7), Ok(false));
    assert_eq!(graph.stats().tree_promotions, 0);
    assert_eq!(graph.stats().candidate_edges, 0);
    assert_eq!(graph.connected(2, 7), Ok(true));
}

#[test]
fn storage_snapshot_reports_live_incidence_and_does_not_change_state() {
    let mut graph = HdtGraph::new();
    for v in 0..8 {
        graph.add_node(v);
    }
    for (a, b) in [(0, 1), (1, 2), (2, 0), (2, 3)] {
        graph.link(a, b).unwrap();
    }
    let before = graph.stats();
    let storage = graph.storage_stats();
    assert_eq!(storage.vertex_count, 8);
    assert_eq!(storage.edge_count, 4);
    assert_eq!(storage.available_levels, 4);
    assert_eq!(storage.levels[0].vertices, 8);
    assert_eq!(storage.levels[0].tree_edges, 3);
    assert_eq!(storage.levels[0].tree_incidences, 6);
    assert_eq!(storage.levels[0].non_tree_incidences, 2);
    assert!(storage.vector_capacity_bytes > 0);
    assert!(storage.ordered_payload_bytes > 0);
    assert_eq!(graph.stats(), before);
    graph.cut(2, 3).unwrap();
    assert_eq!(graph.connected(0, 3), Ok(false));
    let after = graph.storage_stats();
    assert_eq!(after.edge_count, 3);
    assert_eq!(
        after
            .levels
            .iter()
            .map(|level| level.tree_incidences + level.non_tree_incidences)
            .sum::<usize>(),
        6
    );
}

#[test]
fn upper_levels_materialize_only_vertices_touched_by_promotions() {
    let mut graph = HdtGraph::new();
    // Sparse external IDs also exercise translation independently of local IDs.
    for v in 0..4096 {
        graph.add_node(u64::MAX - v);
    }
    let initial = graph.storage_stats();
    assert_eq!(initial.available_levels, 13);
    assert_eq!(initial.levels.len(), 1);
    for (a, b) in [(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 5)] {
        graph.link(u64::MAX - a, u64::MAX - b).unwrap();
    }
    graph.cut(u64::MAX - 2, u64::MAX - 3).unwrap();
    let storage = graph.storage_stats();
    assert_eq!(storage.levels.len(), 2);
    assert_eq!(storage.levels[1].vertices, 3);
    assert_eq!(storage.levels[1].tree_edges, 2);
    assert_eq!(graph.connected(u64::MAX, u64::MAX - 2), Ok(true));
    assert_eq!(graph.connected(u64::MAX, u64::MAX - 3), Ok(false));
    // Growing the global universe must not populate sparse upper levels.
    graph.add_node(1);
    assert_eq!(graph.storage_stats().levels[1].vertices, 3);
    graph.link(1, u64::MAX).unwrap();
    graph.cut(u64::MAX, u64::MAX - 1).unwrap();
    assert_eq!(graph.connected(1, u64::MAX - 1), Ok(true));
}
