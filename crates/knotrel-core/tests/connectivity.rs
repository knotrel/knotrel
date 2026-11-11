//! Behavioral and differential tests for exact dynamic connectivity.

use fixedbitset::FixedBitSet;
use knotrel_core::{Graph, GraphError};

#[test]
fn visited_put_preserves_prior_state_and_enqueue_at_word_boundaries() {
    const WORD_BITS: usize = usize::BITS as usize;
    const BIT_COUNT: usize = WORD_BITS * 2 + 1;
    let indices = [
        0,
        WORD_BITS - 1,
        WORD_BITS,
        WORD_BITS + 1,
        WORD_BITS * 2 - 1,
        BIT_COUNT - 1,
    ];
    let mut original = FixedBitSet::with_capacity(BIT_COUNT);
    let mut candidate = FixedBitSet::with_capacity(BIT_COUNT);
    let mut original_queue = Vec::new();
    let mut candidate_queue = Vec::new();
    for pass in 0..2 {
        for index in indices {
            let enqueue_original = !original.contains(index);
            if enqueue_original {
                original.insert(index);
                original_queue.push(index);
            }
            let previous = candidate.put(index);
            assert_eq!(previous, pass != 0);
            if !previous {
                candidate_queue.push(index);
            }
            assert_eq!(original_queue, candidate_queue);
            assert_eq!(original, candidate);
        }
    }
    assert_eq!(candidate_queue, indices);
}

#[test]
fn visited_put_and_original_pair_reject_out_of_bounds() {
    const BIT_COUNT: usize = usize::BITS as usize + 1;
    for (length, index) in [(0, 0), (BIT_COUNT, BIT_COUNT), (BIT_COUNT, usize::MAX)] {
        let mut original = FixedBitSet::with_capacity(length);
        let mut candidate = FixedBitSet::with_capacity(length);
        assert!(!original.contains(index));
        let original_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !original.contains(index) {
                original.insert(index);
            }
        }));
        let candidate_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| candidate.put(index)));
        assert!(original_result.is_err());
        assert!(candidate_result.is_err());
        assert_eq!(original, candidate);
    }
}

#[test]
fn visited_query_representative_topologies_match_reference() {
    const IDS: [u64; 7] = [0, 1, 64, 128, u64::MAX, u64::MAX - 1, 1 << 63];
    const TOPOLOGIES: [&[(usize, usize)]; 4] = [
        &[(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)],
        &[(0, 1), (1, 2), (2, 3), (3, 0)],
        &[(3, 0), (3, 1), (3, 2), (3, 4), (3, 5)],
        &[(0, 1), (1, 2), (3, 4)],
    ];
    const UNKNOWN_SOURCE: u64 = u64::MAX - 2;
    const UNKNOWN_TARGET: u64 = u64::MAX - 3;
    let verify = |graph: &Graph, reference: &knotrel_core::ReferenceGraph| {
        for source in IDS {
            for target in IDS {
                assert_eq!(
                    graph.connected(source, target),
                    reference.connected(source, target)
                );
            }
        }
        for (source, target) in [
            (UNKNOWN_SOURCE, UNKNOWN_TARGET),
            (UNKNOWN_SOURCE, IDS[0]),
            (IDS[0], UNKNOWN_TARGET),
            (UNKNOWN_SOURCE, UNKNOWN_SOURCE),
        ] {
            assert_eq!(
                graph.connected(source, target),
                reference.connected(source, target)
            );
        }
        assert_eq!(graph.node_count(), reference.node_count());
        assert_eq!(graph.edge_count(), reference.edge_count());
    };
    for edges in TOPOLOGIES {
        let mut graph = Graph::new();
        let mut reference = knotrel_core::ReferenceGraph::new();
        verify(&graph, &reference);
        for node in IDS {
            assert_eq!(graph.add_node(node), reference.add_node(node));
        }
        verify(&graph, &reference);
        for &(source_index, target_index) in edges {
            let source = IDS[source_index];
            let target = IDS[target_index];
            assert_eq!(graph.link(source, target), reference.link(source, target));
            assert_eq!(graph.link(source, target), reference.link(source, target));
            assert_eq!(graph.link(target, source), reference.link(target, source));
            verify(&graph, &reference);
        }
        for &(source_index, target_index) in edges {
            let source = IDS[source_index];
            let target = IDS[target_index];
            assert_eq!(graph.cut(source, target), reference.cut(source, target));
            assert_eq!(graph.cut(target, source), reference.cut(target, source));
            verify(&graph, &reference);
            assert_eq!(graph.link(target, source), reference.link(target, source));
            verify(&graph, &reference);
        }
    }
}

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

#[test]
fn link_endpoint_resolution_and_registration_invariants() {
    let mut graph = Graph::new();

    // 1. Rejected self-loop on empty graph leaves graph empty.
    assert_eq!(graph.link(42, 42), Err(GraphError::SelfLoop { node: 42 }));
    assert_eq!((graph.node_count(), graph.edge_count()), (0, 0));

    // 2. Link with zero existing endpoints (both fresh).
    // Invariant: source is registered first, then target.
    assert_eq!(graph.link(100, 200), Ok(true));
    assert_eq!((graph.node_count(), graph.edge_count()), (2, 1));
    assert_eq!(graph.connected(100, 200), Ok(true));

    // 3. Rejected self-loop on non-empty graph leaves existing state unchanged.
    assert_eq!(
        graph.link(100, 100),
        Err(GraphError::SelfLoop { node: 100 })
    );
    assert_eq!(
        graph.link(999, 999),
        Err(GraphError::SelfLoop { node: 999 })
    );
    assert_eq!((graph.node_count(), graph.edge_count()), (2, 1));
    assert_eq!(
        graph.connected(999, 999),
        Err(GraphError::UnknownNode { node: 999 })
    );

    // 4. Link with one existing endpoint:
    // a) source exists (100), target fresh (300).
    assert_eq!(graph.link(100, 300), Ok(true));
    assert_eq!((graph.node_count(), graph.edge_count()), (3, 2));
    assert_eq!(graph.connected(200, 300), Ok(true));

    // b) target exists (200), source fresh (400).
    assert_eq!(graph.link(400, 200), Ok(true));
    assert_eq!((graph.node_count(), graph.edge_count()), (4, 3));
    assert_eq!(graph.connected(400, 300), Ok(true));

    // 5. Link with two existing endpoints (300 and 400).
    assert_eq!(graph.link(300, 400), Ok(true));
    assert_eq!((graph.node_count(), graph.edge_count()), (4, 4));

    // 6. Duplicate and reversed links.
    assert_eq!(graph.link(300, 400), Ok(false));
    assert_eq!(graph.link(400, 300), Ok(false));
    assert_eq!((graph.node_count(), graph.edge_count()), (4, 4));

    // 7. Arbitrary u64 IDs, including boundary values (0, 1, u64::MAX, u64::MAX - 1).
    let boundary_edges = [
        (0, u64::MAX),
        (u64::MAX, u64::MAX - 1),
        (1, 1 << 63),
        (u64::MAX - 1, 0),
    ];
    for &(u, v) in &boundary_edges {
        assert_eq!(graph.link(u, v), Ok(true));
        assert_eq!(graph.connected(u, v), Ok(true));
        assert_eq!(graph.link(v, u), Ok(false));
    }
    assert_eq!(graph.connected(0, u64::MAX - 1), Ok(true));

    // 8. Subsequent cuts and reachability queries.
    assert_eq!(graph.cut(0, u64::MAX), Ok(true));
    assert_eq!(graph.cut(0, u64::MAX), Ok(false));
    assert_eq!(graph.connected(0, u64::MAX), Ok(true)); // Still connected via u64::MAX - 1

    assert_eq!(graph.cut(u64::MAX - 1, 0), Ok(true));
    assert_eq!(graph.connected(0, u64::MAX), Ok(false)); // Broken path to 0

    // Endpoints remain registered after cut.
    assert_eq!(graph.connected(0, 0), Ok(true));
    assert_eq!(graph.connected(u64::MAX, u64::MAX), Ok(true));
}
