//! CF3 (ADR 0014, section 9): the experimental `cluster-forest` engine against
//! `ReferenceGraph` and `compact-bfs` on the same seeded operation sequences,
//! through the public API only, plus lifecycle churn across engine
//! compactions. The long variants are `#[ignore]` and run in the
//! `cluster-forest-long` workflow.

use knotrel_core::{
    ConnectivityGraph, EdgeSpec, EngineConfig, GraphLimits, LifecycleGraph, NodeSpec,
    ReferenceGraph,
};

/// SplitMix64: deterministic, seedable, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Applies the same random history to `cluster-forest`, `compact-bfs` and
/// `ReferenceGraph`; every result and sampled query must agree. IDs are
/// spread over the full `u64` range to exercise the ID map.
fn differential(seed: u64, vertices: u64, operations: usize, link_percent: u64, queries: usize) {
    let mut rng = Rng(seed);
    let id = |x: u64| x.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut forest = ConnectivityGraph::new(EngineConfig::ClusterForest);
    let mut compact = ConnectivityGraph::new(EngineConfig::CompactBfs);
    let mut reference = ReferenceGraph::new();
    let mut present: Vec<(u64, u64)> = Vec::new();
    for step in 0..operations {
        let context = format!("seed {seed} step {step}");
        match rng.below(100) {
            roll if roll < 3 => {
                let node = id(rng.below(vertices));
                let expected = reference.add_node(node);
                assert_eq!(forest.add_node(node), Ok(expected), "{context}");
                assert_eq!(compact.add_node(node), Ok(expected), "{context}");
            }
            roll if roll < link_percent || present.is_empty() => {
                let (a, b) = (id(rng.below(vertices)), id(rng.below(vertices)));
                let expected = reference.link(a, b);
                assert_eq!(forest.link(a, b), expected, "{context}");
                assert_eq!(compact.link(a, b), expected, "{context}");
                if expected == Ok(true) {
                    present.push((a, b));
                }
            }
            _ => {
                let (a, b) = present.swap_remove(rng.below(present.len() as u64) as usize);
                let (a, b) = if rng.below(2) == 0 { (a, b) } else { (b, a) };
                assert_eq!(reference.cut(a, b), Ok(true), "{context}");
                assert_eq!(forest.cut(a, b), Ok(true), "{context}");
                assert_eq!(compact.cut(a, b), Ok(true), "{context}");
            }
        }
        assert_eq!(forest.edge_count(), reference.edge_count(), "{context}");
        assert_eq!(forest.node_count(), reference.node_count(), "{context}");
        for _ in 0..queries {
            let (a, b) = (id(rng.below(vertices + 2)), id(rng.below(vertices + 2)));
            let expected = reference.connected(a, b);
            assert_eq!(forest.connected(a, b), expected, "{context}: ({a}, {b})");
            assert_eq!(compact.connected(a, b), expected, "{context}: ({a}, {b})");
        }
    }
}

#[test]
fn differential_against_reference_and_compact_bfs() {
    for seed in 0..40 {
        differential(seed, 24, 400, 60, 8);
    }
    for seed in 0..6 {
        differential(500 + seed, 200, 1_500, 55, 4);
    }
}

#[test]
#[ignore = "long CF3 variant; run by the cluster-forest-long workflow"]
fn differential_long() {
    for seed in 0..30 {
        differential(10_000 + seed, 64, 20_000, 52, 4);
    }
    for seed in 0..4 {
        differential(20_000 + seed, 2_000, 60_000, 55, 2);
    }
}

/// The bulk constructor and incremental insertion agree, and the graph keeps
/// working after construction.
#[test]
fn bulk_construction_then_dynamic_updates() {
    let nodes: Vec<u64> = (0..64).collect();
    let edges: Vec<(u64, u64)> = (0..63)
        .map(|x| (x, x + 1))
        .chain([(0, 63), (5, 40)])
        .collect();
    let mut forest = ConnectivityGraph::try_from_edges(
        EngineConfig::ClusterForest,
        GraphLimits::default(),
        &nodes,
        &edges,
    )
    .unwrap();
    let mut reference = ReferenceGraph::new();
    for &(a, b) in &edges {
        reference.link(a, b).unwrap();
    }
    for &(a, b) in &[(10, 11), (50, 51), (5, 40), (0, 63), (30, 31)] {
        assert_eq!(forest.cut(a, b), reference.cut(a, b));
        for x in (0..64).step_by(7) {
            for y in (0..64).step_by(5) {
                assert_eq!(forest.connected(x, y), reference.connected(x, y));
            }
        }
    }
}

/// Effective-graph comparison of two lifecycle graphs over `ids`.
fn assert_lifecycle_agree(a: &LifecycleGraph, b: &LifecycleGraph, ids: &[u64], context: &str) {
    assert_eq!(a.node_count(), b.node_count(), "{context}");
    assert_eq!(a.edge_count(), b.edge_count(), "{context}");
    assert_eq!(
        a.effective_edge_count(),
        b.effective_edge_count(),
        "{context}"
    );
    for &x in ids {
        for &y in ids {
            assert_eq!(
                a.connected(x, y),
                b.connected(x, y),
                "{context}: ({x}, {y})"
            );
        }
    }
}

/// Lifecycle churn: flags, edge toggles and permanent removals drive retired
/// engine slots past the compaction threshold several times. Answers stay
/// equal to `compact-bfs` across every compaction, and engine vertices stay
/// within the documented bound `2 × catalog + 1024`.
fn lifecycle_churn(seed: u64, rounds: usize) {
    let mut rng = Rng(seed);
    let mut forest = LifecycleGraph::new(EngineConfig::ClusterForest).unwrap();
    let mut compact = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
    let mut next_id = 0u64;
    let mut live: Vec<u64> = Vec::new();
    let mut compactions = 0;
    let mut retired_before = 0usize;
    for round in 0..rounds {
        // Add a batch of nodes wired to the live set.
        for _ in 0..40 {
            let node = next_id;
            next_id += 1;
            for graph in [&mut forest, &mut compact] {
                graph.add_node(node).unwrap();
            }
            for _ in 0..2 {
                if let Some(&other) = live.get(rng.below(live.len().max(1) as u64) as usize) {
                    assert_eq!(forest.link(node, other), compact.link(node, other));
                }
            }
            live.push(node);
        }
        // Toggle flags and edges, cut some edges.
        for _ in 0..30 {
            let node = live[rng.below(live.len() as u64) as usize];
            let active = rng.below(3) != 0;
            assert_eq!(
                forest.set_node_active(node, active).ok(),
                compact.set_node_active(node, active).ok()
            );
            let other = live[rng.below(live.len() as u64) as usize];
            if node != other {
                let enabled = rng.below(2) == 0;
                assert_eq!(
                    forest.set_edge_enabled(node, other, enabled).ok(),
                    compact.set_edge_enabled(node, other, enabled).ok()
                );
                if rng.below(4) == 0 {
                    assert_eq!(forest.cut(node, other), compact.cut(node, other));
                }
            }
        }
        // Permanently remove most of the batch's predecessors.
        let removals = live.len().saturating_sub(60);
        for _ in 0..removals {
            let node = live.swap_remove(rng.below(live.len() as u64) as usize);
            assert_eq!(forest.remove_node(node), compact.remove_node(node));
        }
        let sample: Vec<u64> = live.iter().copied().step_by(3).collect();
        assert_lifecycle_agree(
            &forest,
            &compact,
            &sample,
            &format!("seed {seed} round {round}"),
        );
        assert!(
            forest.engine_vertex_count() <= 2 * forest.node_count() + 1024,
            "seed {seed} round {round}: engine vertices bounded"
        );
        // Retired slots only grow between compactions; a drop is one.
        let retired = forest.engine_vertex_count() - forest.node_count();
        if retired < retired_before {
            compactions += 1;
        }
        retired_before = retired;
    }
    assert!(
        compactions > 0,
        "seed {seed}: the churn reached a compaction"
    );
}

#[test]
fn lifecycle_churn_across_compactions() {
    for seed in 0..3 {
        lifecycle_churn(seed, 60);
    }
}

#[test]
#[ignore = "long CF3 variant; run by the cluster-forest-long workflow"]
fn lifecycle_churn_long() {
    for seed in 0..10 {
        lifecycle_churn(100 + seed, 400);
    }
}

/// An atomic lifecycle import builds the cluster-forest engine off the
/// catalog and answers like `compact-bfs`.
#[test]
fn lifecycle_import_matches_compact_bfs() {
    let nodes: Vec<NodeSpec> = (0..50)
        .map(|node| NodeSpec {
            node,
            active: node % 7 != 0,
        })
        .collect();
    let edges: Vec<EdgeSpec> = (0..49)
        .map(|x| EdgeSpec {
            source: x,
            target: x + 1,
            enabled: x % 5 != 0,
        })
        .chain((0..10).map(|x| EdgeSpec {
            source: x,
            target: x + 25,
            enabled: true,
        }))
        .collect();
    let forest = LifecycleGraph::try_from_topology(
        EngineConfig::ClusterForest,
        GraphLimits::default(),
        &nodes,
        &edges,
    )
    .unwrap();
    let compact = LifecycleGraph::try_from_topology(
        EngineConfig::CompactBfs,
        GraphLimits::default(),
        &nodes,
        &edges,
    )
    .unwrap();
    let ids: Vec<u64> = (0..50).collect();
    assert_lifecycle_agree(&forest, &compact, &ids, "import");
}
