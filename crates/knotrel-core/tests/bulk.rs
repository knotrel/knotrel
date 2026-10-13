//! Ordered bulk construction and dynamic continuation against incremental oracles.

use knotrel_core::{
    ConnectivityGraph, EngineConfig, GraphError, GraphLimits, NodeId, ReferenceGraph,
};

const ENGINES: [EngineConfig; 3] = [
    EngineConfig::CompactBfs,
    EngineConfig::EulerTour,
    EngineConfig::Hdt,
];

fn incremental(
    engine: EngineConfig,
    limits: GraphLimits,
    nodes: &[NodeId],
    edges: &[(NodeId, NodeId)],
) -> Result<ConnectivityGraph, GraphError> {
    let mut graph = ConnectivityGraph::with_limits(engine, limits);
    for &node in nodes {
        graph.add_node(node)?;
    }
    for &(a, b) in edges {
        graph.link(a, b)?;
    }
    Ok(graph)
}

fn same_state(a: &ConnectivityGraph, b: &ConnectivityGraph, ids: &[NodeId]) {
    assert_eq!(a.engine(), b.engine());
    assert_eq!(a.limits(), b.limits());
    assert_eq!(a.node_count(), b.node_count());
    assert_eq!(a.edge_count(), b.edge_count());
    for &x in ids {
        for &y in ids {
            assert_eq!(a.connected(x, y), b.connected(x, y), "{x}, {y}");
        }
    }
}

fn reference_state(graph: &ConnectivityGraph, reference: &ReferenceGraph, ids: &[NodeId]) {
    assert_eq!(graph.node_count(), reference.node_count());
    assert_eq!(graph.edge_count(), reference.edge_count());
    for &x in ids {
        for &y in ids {
            assert_eq!(graph.connected(x, y), reference.connected(x, y), "{x}, {y}");
        }
    }
}

fn exercise(engine: EngineConfig, nodes: &[NodeId], edges: &[(NodeId, NodeId)]) {
    let mut bulk =
        ConnectivityGraph::try_from_edges(engine, GraphLimits::default(), nodes, edges).unwrap();
    let mut sequential = incremental(engine, GraphLimits::default(), nodes, edges).unwrap();
    let mut reference = ReferenceGraph::new();
    for &node in nodes {
        reference.add_node(node);
    }
    for &(a, b) in edges {
        reference.link(a, b).unwrap();
    }
    let mut ids: Vec<_> = nodes
        .iter()
        .copied()
        .chain(edges.iter().flat_map(|&(a, b)| [a, b]))
        .collect();
    ids.extend([0, u64::MAX, 1 << 63, 999]);
    ids.sort_unstable();
    ids.dedup();
    same_state(&bulk, &sequential, &ids);
    reference_state(&bulk, &reference, &ids);

    // Cut every input edge, including duplicates, to exercise replacement and
    // bridge splits and to detect edge-set differences hidden by connectivity.
    for &(a, b) in edges {
        let expected = reference.cut(a, b);
        assert_eq!(bulk.cut(a, b), expected);
        assert_eq!(sequential.cut(a, b), expected);
        same_state(&bulk, &sequential, &ids);
        reference_state(&bulk, &reference, &ids);
    }
    assert_eq!(bulk.edge_count(), 0);

    // Grow across HDT level boundaries, then reuse forest tokens through churn.
    for node in 0..17 {
        let expected = Ok(reference.add_node(node));
        assert_eq!(bulk.add_node(node), expected);
        assert_eq!(sequential.add_node(node), expected);
        ids.push(node);
    }
    ids.sort_unstable();
    ids.dedup();
    let mut state = 31_u64;
    for step in 0..160 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = ids[(state >> 32) as usize % ids.len()];
        let b = ids[(state >> 48) as usize % ids.len()];
        let expected = if step % 3 == 0 {
            reference.cut(a, b)
        } else {
            reference.link(a, b)
        };
        if step % 3 == 0 {
            assert_eq!(bulk.cut(a, b), expected);
            assert_eq!(sequential.cut(a, b), expected);
        } else {
            assert_eq!(bulk.link(a, b), expected);
            assert_eq!(sequential.link(a, b), expected);
        }
        same_state(&bulk, &sequential, &ids);
        reference_state(&bulk, &reference, &ids);
    }
}

#[test]
fn topologies_and_dynamic_histories_match_incremental_and_reference() {
    let nodes: Vec<_> = (0..8).rev().collect();
    let path: Vec<_> = (0..7).map(|n| (n, n + 1)).collect();
    let mut cycle = path.clone();
    cycle.push((7, 0));
    let dense: Vec<_> = (0..8)
        .flat_map(|a| (a + 1..8).map(move |b| (a, b)))
        .collect();
    for engine in ENGINES {
        for edges in [
            &path[..],
            &cycle,
            &dense,
            &[(0, 1), (1, 2), (4, 5), (5, 6), (6, 4)],
        ] {
            exercise(engine, &nodes, edges);
        }
        exercise(engine, &[], &[]);
        exercise(engine, &[u64::MAX, u64::MAX], &[]);
        exercise(engine, &[], &[(u64::MAX, 0), (0, 1 << 63), (0, u64::MAX)]);
        exercise(
            engine,
            &[9, 9, 0],
            &[(2, 1), (1, 0), (0, 2), (1, 2), (2, 1)],
        );
    }
}

#[test]
fn every_small_graph_preserves_edges_in_both_insertion_orders() {
    let ids = [u64::MAX, 0, 1 << 63, 17, 3];
    for n in 0..=5 {
        let possible: Vec<_> = (0..n)
            .flat_map(|a| (a + 1..n).map(move |b| (ids[a], ids[b])))
            .collect();
        for mask in 0..(1 << possible.len()) {
            let mut edges: Vec<_> = possible
                .iter()
                .enumerate()
                .filter_map(|(bit, &e)| (mask & (1 << bit) != 0).then_some(e))
                .collect();
            for _ in 0..2 {
                for engine in ENGINES {
                    let mut bulk = ConnectivityGraph::try_from_edges(
                        engine,
                        GraphLimits::default(),
                        &ids[..n],
                        &edges,
                    )
                    .unwrap();
                    let mut sequential =
                        incremental(engine, GraphLimits::default(), &ids[..n], &edges).unwrap();
                    same_state(&bulk, &sequential, &ids);
                    for &(a, b) in &edges {
                        assert_eq!(bulk.link(b, a), Ok(false));
                        assert_eq!(bulk.cut(a, b), Ok(true));
                        assert_eq!(sequential.cut(a, b), Ok(true));
                        same_state(&bulk, &sequential, &ids);
                    }
                    assert_eq!(bulk.edge_count(), 0);
                }
                edges.reverse();
            }
        }
    }
}

#[test]
fn limits_and_first_errors_match_the_ordered_loop() {
    let node_inputs: &[&[NodeId]] = &[&[], &[1], &[1, 1], &[2, 1, 2], &[1, 2, 3, 4]];
    let edge_inputs: &[&[(NodeId, NodeId)]] = &[
        &[],
        &[(1, 1)],
        &[(1, 2), (2, 1)],
        &[(1, 2), (2, 3), (4, 4)],
        &[(1, 2), (3, 4)],
        &[(u64::MAX, 0), (0, 1)],
    ];
    let ids = [0, 1, 2, 3, 4, u64::MAX];
    for engine in ENGINES {
        for max_nodes in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
            for max_edges in [None, Some(0), Some(1), Some(2)] {
                let limits = GraphLimits {
                    max_nodes,
                    max_edges,
                };
                for &nodes in node_inputs {
                    for &edges in edge_inputs {
                        match (
                            ConnectivityGraph::try_from_edges(engine, limits, nodes, edges),
                            incremental(engine, limits, nodes, edges),
                        ) {
                            (Err(a), Err(b)) => assert_eq!(a, b),
                            (Ok(mut a), Ok(mut b)) => {
                                same_state(&a, &b, &ids);
                                for &(x, y) in &[(1, 2), (2, 1), (4, 4), (3, 4), (0, u64::MAX)] {
                                    assert_eq!(a.link(x, y), b.link(x, y));
                                    same_state(&a, &b, &ids);
                                    assert_eq!(a.cut(x, y), b.cut(x, y));
                                    same_state(&a, &b, &ids);
                                    assert_eq!(a.add_node(x), b.add_node(x));
                                    same_state(&a, &b, &ids);
                                }
                            }
                            _ => panic!(
                                "bulk and incremental success differ: {engine:?}, {limits:?}"
                            ),
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn competing_errors_have_explicit_expected_precedence() {
    use GraphError::{EdgeLimitExceeded, NodeLimitExceeded, SelfLoop};
    for engine in ENGINES {
        for (nodes, edges, max_nodes, max_edges, expected) in [
            (
                &[1, 2][..],
                &[(9, 9)][..],
                1,
                0,
                NodeLimitExceeded { limit: 1 },
            ),
            (&[][..], &[(9, 9)][..], 0, 0, SelfLoop { node: 9 }),
            (
                &[][..],
                &[(1, 2), (9, 9)][..],
                1,
                0,
                NodeLimitExceeded { limit: 1 },
            ),
            (
                &[][..],
                &[(1, 2), (9, 9)][..],
                2,
                0,
                EdgeLimitExceeded { limit: 0 },
            ),
            (
                &[][..],
                &[(1, 2), (2, 1), (3, 3)][..],
                2,
                1,
                SelfLoop { node: 3 },
            ),
            (
                &[][..],
                &[(1, 2), (2, 3), (4, 4)][..],
                3,
                1,
                EdgeLimitExceeded { limit: 1 },
            ),
        ] {
            let limits = GraphLimits {
                max_nodes: Some(max_nodes),
                max_edges: Some(max_edges),
            };
            assert_eq!(
                ConnectivityGraph::try_from_edges(engine, limits, nodes, edges).unwrap_err(),
                expected
            );
        }
    }
}
