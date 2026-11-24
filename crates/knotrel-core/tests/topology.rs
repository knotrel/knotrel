//! Lifecycle facade tests against an independent oracle (M10).
//!
//! The oracle stores registered nodes and edges with their flags in ordered
//! maps and answers connectivity with a fresh union-find over the effective
//! edges. It shares no code with `LifecycleGraph`.

use knotrel_core::{
    EdgeSpec, EdgeState, EngineConfig, GraphError, GraphLimits, LifecycleError, LifecycleGraph,
    NodeSpec,
};
use std::collections::BTreeMap;

/// Engines with lifecycle support in v0.5.0 (`ett` and `hdt` are deferred and
/// rejected, see `unsupported_engines_are_rejected_without_a_graph`).
const ENGINES: [EngineConfig; 2] = [EngineConfig::CompactBfs, EngineConfig::ClusterForest];

#[derive(Default, Clone)]
struct Oracle {
    nodes: BTreeMap<u64, bool>,
    edges: BTreeMap<(u64, u64), bool>,
    limits: GraphLimits,
}

fn key(a: u64, b: u64) -> (u64, u64) {
    (a.min(b), a.max(b))
}

impl Oracle {
    fn add_node(&mut self, n: u64) -> Result<bool, GraphError> {
        if self.nodes.contains_key(&n) {
            return Ok(false);
        }
        if let Some(limit) = self.limits.max_nodes
            && self.nodes.len() >= limit
        {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        self.nodes.insert(n, true);
        Ok(true)
    }

    fn link(&mut self, a: u64, b: u64) -> Result<bool, GraphError> {
        if a == b {
            return Err(GraphError::SelfLoop { node: a });
        }
        let missing = [a, b]
            .iter()
            .filter(|n| !self.nodes.contains_key(n))
            .count();
        if let Some(limit) = self.limits.max_nodes
            && self.nodes.len() + missing > limit
        {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        if self.edges.contains_key(&key(a, b)) {
            return Ok(false);
        }
        if let Some(limit) = self.limits.max_edges
            && self.edges.len() >= limit
        {
            return Err(GraphError::EdgeLimitExceeded { limit });
        }
        self.nodes.entry(a).or_insert(true);
        self.nodes.entry(b).or_insert(true);
        self.edges.insert(key(a, b), true);
        Ok(true)
    }

    fn cut(&mut self, a: u64, b: u64) -> Result<bool, GraphError> {
        if a == b {
            return Err(GraphError::SelfLoop { node: a });
        }
        Ok(self.edges.remove(&key(a, b)).is_some())
    }

    fn set_node_active(&mut self, n: u64, active: bool) -> Result<bool, LifecycleError> {
        let flag = self
            .nodes
            .get_mut(&n)
            .ok_or(LifecycleError::Graph(GraphError::UnknownNode { node: n }))?;
        let changed = *flag != active;
        *flag = active;
        Ok(changed)
    }

    fn set_edge_enabled(&mut self, a: u64, b: u64, enabled: bool) -> Result<bool, LifecycleError> {
        if a == b {
            return Err(GraphError::SelfLoop { node: a }.into());
        }
        for n in [a, b] {
            if !self.nodes.contains_key(&n) {
                return Err(GraphError::UnknownNode { node: n }.into());
            }
        }
        let flag = self
            .edges
            .get_mut(&key(a, b))
            .ok_or(LifecycleError::UnknownEdge {
                source: a,
                target: b,
            })?;
        let changed = *flag != enabled;
        *flag = enabled;
        Ok(changed)
    }

    fn remove_node(&mut self, n: u64) -> bool {
        if self.nodes.remove(&n).is_none() {
            return false;
        }
        self.edges.retain(|&(a, b), _| a != n && b != n);
        true
    }

    fn effective(&self, (a, b): (u64, u64), enabled: bool) -> bool {
        enabled && self.nodes[&a] && self.nodes[&b]
    }

    fn connected(&self, a: u64, b: u64) -> Result<bool, GraphError> {
        for n in [a, b] {
            if !self.nodes.contains_key(&n) {
                return Err(GraphError::UnknownNode { node: n });
            }
        }
        if !self.nodes[&a] || !self.nodes[&b] {
            return Ok(false);
        }
        let mut parent: BTreeMap<u64, u64> = self.nodes.keys().map(|&n| (n, n)).collect();
        fn find(parent: &mut BTreeMap<u64, u64>, mut n: u64) -> u64 {
            while parent[&n] != n {
                n = parent[&n];
            }
            n
        }
        for (&edge, &enabled) in &self.edges {
            if self.effective(edge, enabled) {
                let (ra, rb) = (find(&mut parent, edge.0), find(&mut parent, edge.1));
                parent.insert(ra, rb);
            }
        }
        Ok(find(&mut parent, a) == find(&mut parent, b))
    }

    fn edge_state(&self, a: u64, b: u64) -> Option<EdgeState> {
        if a == b {
            return None;
        }
        self.edges
            .get(&key(a, b))
            .map(|&enabled| EdgeState::new(enabled, self.effective(key(a, b), enabled)))
    }

    fn counts(&self) -> [usize; 5] {
        [
            self.nodes.len(),
            self.edges.len(),
            self.nodes.values().filter(|&&a| a).count(),
            self.edges.values().filter(|&&e| e).count(),
            self.edges
                .iter()
                .filter(|&(&edge, &enabled)| self.effective(edge, enabled))
                .count(),
        ]
    }
}

fn counts(graph: &LifecycleGraph) -> [usize; 5] {
    [
        graph.node_count(),
        graph.edge_count(),
        graph.active_node_count(),
        graph.enabled_edge_count(),
        graph.effective_edge_count(),
    ]
}

/// Compares every pairwise query, state and count over `universe`.
fn assert_matches(graph: &LifecycleGraph, oracle: &Oracle, universe: &[u64]) {
    assert_eq!(counts(graph), oracle.counts());
    for &a in universe {
        assert_eq!(
            graph.node_active(a),
            oracle.nodes.get(&a).copied(),
            "node {a}"
        );
        for &b in universe {
            assert_eq!(graph.connected(a, b), oracle.connected(a, b), "{a}-{b}");
            assert_eq!(graph.edge_state(a, b), oracle.edge_state(a, b), "{a}-{b}");
        }
    }
}

/// Deterministic SplitMix64 generator: no dependency, reproducible seeds.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Applies one random operation to both and compares results.
fn step(graph: &mut LifecycleGraph, oracle: &mut Oracle, rng: &mut Rng, universe: &[u64]) {
    let pick = |rng: &mut Rng| universe[rng.below(universe.len() as u64) as usize];
    let (a, b) = (pick(rng), pick(rng));
    let flag = rng.below(2) == 0;
    match rng.below(100) {
        0..=21 => assert_eq!(graph.link(a, b), oracle.link(a, b)),
        22..=31 => assert_eq!(graph.cut(a, b), oracle.cut(a, b)),
        32..=37 => assert_eq!(graph.add_node(a), oracle.add_node(a)),
        38..=57 => assert_eq!(
            graph.set_node_active(a, flag),
            oracle.set_node_active(a, flag)
        ),
        58..=79 => assert_eq!(
            graph.set_edge_enabled(a, b, flag),
            oracle.set_edge_enabled(a, b, flag)
        ),
        80..=89 => assert_eq!(graph.remove_node(a), oracle.remove_node(a)),
        _ => assert_eq!(graph.connected(a, b), oracle.connected(a, b)),
    }
}

#[test]
fn randomized_mixed_sequences_match_the_oracle_on_every_engine() {
    let universes: [&[u64]; 3] = [
        &[0, 1, 2, 3, 4, 5],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        &[0, 1, u64::MAX - 1, u64::MAX, 1 << 63, 42],
    ];
    for engine in ENGINES {
        for (index, universe) in universes.iter().enumerate() {
            for seed in 0..40_u64 {
                let mut rng = Rng(seed * 7919 + index as u64);
                let mut graph = LifecycleGraph::new(engine).unwrap();
                let mut oracle = Oracle::default();
                for _ in 0..250 {
                    step(&mut graph, &mut oracle, &mut rng, universe);
                }
                assert_matches(&graph, &oracle, universe);
            }
        }
    }
}

#[test]
fn every_step_matches_the_oracle_in_short_sequences() {
    let universe = [1, 2, 3, 4, 5];
    for engine in ENGINES {
        for seed in 0..60_u64 {
            let mut rng = Rng(seed ^ 0x5eed);
            let mut graph = LifecycleGraph::new(engine).unwrap();
            let mut oracle = Oracle::default();
            for _ in 0..60 {
                step(&mut graph, &mut oracle, &mut rng, &universe);
                assert_matches(&graph, &oracle, &universe);
            }
        }
    }
}

#[test]
fn limits_count_registered_topology_and_cannot_be_bypassed() {
    let limits = GraphLimits {
        max_nodes: Some(4),
        max_edges: Some(3),
    };
    for engine in ENGINES {
        let mut rng = Rng(99);
        let universe = [1, 2, 3, 4, 5, 6];
        let mut graph = LifecycleGraph::with_limits(engine, limits).unwrap();
        let mut oracle = Oracle {
            limits,
            ..Oracle::default()
        };
        for _ in 0..2_000 {
            step(&mut graph, &mut oracle, &mut rng, &universe);
            assert!(graph.node_count() <= 4 && graph.edge_count() <= 3);
        }
        assert_matches(&graph, &oracle, &universe);

        // Deactivating and disabling never frees capacity.
        let mut graph = LifecycleGraph::with_limits(engine, limits).unwrap();
        for (a, b) in [(1, 2), (2, 3), (3, 4)] {
            graph.link(a, b).unwrap();
        }
        graph.set_node_active(2, false).unwrap();
        graph.set_edge_enabled(3, 4, false).unwrap();
        assert_eq!(
            graph.add_node(5),
            Err(GraphError::NodeLimitExceeded { limit: 4 })
        );
        assert_eq!(
            graph.link(1, 4),
            Err(GraphError::EdgeLimitExceeded { limit: 3 })
        );
        // An existing edge is still a no-op at the limit, as in ConnectivityGraph.
        assert_eq!(graph.link(2, 1), Ok(false));
        // Removal frees both kinds of capacity.
        assert!(graph.remove_node(2));
        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.link(1, 5), Ok(true));
    }
}

#[test]
fn deactivation_preserves_edge_flags_and_reactivation_respects_them() {
    for engine in ENGINES {
        // Hub 0 with spokes 1..=6 and a ring among the spokes broken at 6-1.
        let mut graph = LifecycleGraph::new(engine).unwrap();
        for spoke in 1..=6 {
            graph.link(0, spoke).unwrap();
        }
        for spoke in 1..6 {
            graph.link(spoke, spoke + 1).unwrap();
        }
        graph.set_edge_enabled(0, 3, false).unwrap();
        assert!(graph.set_node_active(0, false).unwrap());
        assert_eq!(graph.effective_edge_count(), 5);
        assert!(graph.connected(1, 6).unwrap());
        assert!(!graph.connected(0, 0).unwrap());
        // Change flags while the hub is inactive: preserved, not effective.
        assert!(graph.set_edge_enabled(0, 3, true).unwrap());
        assert!(graph.set_edge_enabled(0, 4, false).unwrap());
        assert_eq!(graph.edge_state(0, 3), Some(EdgeState::new(true, false)));
        // Break the ring so only the hub can join 1..=3 and 4..=6.
        graph.set_edge_enabled(3, 4, false).unwrap();
        assert!(!graph.connected(1, 6).unwrap());
        assert!(graph.set_node_active(0, true).unwrap());
        assert!(graph.connected(1, 6).unwrap());
        assert_eq!(graph.edge_state(0, 4).map(|s| s.effective), Some(false));
        assert_eq!(graph.edge_state(0, 3).map(|s| s.effective), Some(true));
        // Repeated setters are no-ops.
        assert_eq!(graph.set_node_active(0, true), Ok(false));
        assert_eq!(graph.set_edge_enabled(0, 4, false), Ok(false));
    }
}

#[test]
fn adjacent_inactive_nodes_and_links_to_inactive_endpoints() {
    for engine in ENGINES {
        let mut graph = LifecycleGraph::new(engine).unwrap();
        graph.link(1, 2).unwrap();
        graph.link(2, 3).unwrap();
        graph.link(3, 4).unwrap();
        graph.set_node_active(2, false).unwrap();
        graph.set_node_active(3, false).unwrap();
        assert_eq!(graph.effective_edge_count(), 0);
        graph.set_node_active(2, true).unwrap();
        assert!(graph.connected(1, 2).unwrap());
        assert!(!graph.connected(2, 4).unwrap());
        // Linking to an inactive endpoint registers an enabled, ineffective edge.
        assert_eq!(graph.link(3, 5), Ok(true));
        assert_eq!(graph.edge_state(5, 3), Some(EdgeState::new(true, false)));
        // add_node never reactivates; link never re-enables.
        assert_eq!(graph.add_node(3), Ok(false));
        assert_eq!(graph.node_active(3), Some(false));
        graph.set_edge_enabled(1, 2, false).unwrap();
        assert_eq!(graph.link(1, 2), Ok(false));
        assert_eq!(graph.edge_state(1, 2).map(|s| s.enabled), Some(false));
        // cut removes an ineffective edge.
        assert_eq!(graph.cut(3, 5), Ok(true));
        assert_eq!(graph.edge_state(3, 5), None);
    }
}

#[test]
fn removal_cascades_and_reused_ids_start_fresh() {
    for engine in ENGINES {
        let mut graph = LifecycleGraph::new(engine).unwrap();
        for spoke in 1..=50 {
            graph.link(0, spoke).unwrap();
        }
        graph.link(1, 2).unwrap();
        graph.set_edge_enabled(0, 7, false).unwrap();
        graph.set_node_active(9, false).unwrap();
        assert!(graph.remove_node(0));
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.node_count(), 50);
        assert!(!graph.remove_node(0));
        assert_eq!(
            graph.connected(0, 1),
            Err(GraphError::UnknownNode { node: 0 })
        );
        assert!(graph.add_node(0).unwrap());
        assert_eq!(graph.node_active(0), Some(true));
        for spoke in 1..=50 {
            assert_eq!(graph.edge_state(0, spoke), None);
            assert!(!graph.connected(0, spoke).unwrap());
        }
        assert!(graph.connected(1, 2).unwrap());
        // Re-creating through link also starts from no edges.
        assert!(graph.remove_node(0));
        assert!(graph.link(0, 3).unwrap());
        assert_eq!(graph.edge_count(), 2);
        assert!(!graph.connected(0, 1).unwrap());
    }
}

#[test]
fn setter_errors_follow_the_documented_precedence_and_change_nothing() {
    let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
    graph.link(1, 2).unwrap();
    graph.add_node(3).unwrap();
    let before = counts(&graph);
    assert_eq!(
        graph.set_node_active(9, false),
        Err(LifecycleError::Graph(GraphError::UnknownNode { node: 9 }))
    );
    assert_eq!(
        graph.set_edge_enabled(9, 9, false),
        Err(LifecycleError::Graph(GraphError::SelfLoop { node: 9 }))
    );
    assert_eq!(
        graph.set_edge_enabled(8, 9, false),
        Err(LifecycleError::Graph(GraphError::UnknownNode { node: 8 }))
    );
    assert_eq!(
        graph.set_edge_enabled(1, 9, false),
        Err(LifecycleError::Graph(GraphError::UnknownNode { node: 9 }))
    );
    assert_eq!(
        graph.set_edge_enabled(3, 1, true),
        Err(LifecycleError::UnknownEdge {
            source: 3,
            target: 1
        })
    );
    assert_eq!(graph.cut(4, 4), Err(GraphError::SelfLoop { node: 4 }));
    assert_eq!(graph.edge_state(1, 1), None);
    assert_eq!(counts(&graph), before);
}

#[test]
fn try_from_topology_matches_incremental_construction() {
    let nodes = [
        NodeSpec {
            node: 1,
            active: true,
        },
        NodeSpec {
            node: 2,
            active: false,
        },
        NodeSpec {
            node: 7,
            active: true,
        },
        NodeSpec {
            node: 2,
            active: false,
        },
    ];
    let edge = |source, target, enabled| EdgeSpec {
        source,
        target,
        enabled,
    };
    let edges = [
        edge(1, 2, true),
        edge(2, 3, true),
        edge(3, 4, false),
        edge(4, 1, true),
        edge(1, 4, true),
        edge(5, 6, true),
    ];
    for engine in ENGINES {
        let bulk =
            LifecycleGraph::try_from_topology(engine, GraphLimits::default(), &nodes, &edges)
                .unwrap();
        let mut incremental = LifecycleGraph::new(engine).unwrap();
        let mut oracle = Oracle::default();
        for spec in &nodes {
            incremental.add_node(spec.node).unwrap();
            oracle.add_node(spec.node).unwrap();
        }
        for spec in &edges {
            incremental.link(spec.source, spec.target).unwrap();
            oracle.link(spec.source, spec.target).unwrap();
            if !spec.enabled {
                incremental
                    .set_edge_enabled(spec.source, spec.target, false)
                    .unwrap();
                oracle
                    .set_edge_enabled(spec.source, spec.target, false)
                    .unwrap();
            }
        }
        for spec in &nodes {
            if !spec.active {
                incremental.set_node_active(spec.node, false).unwrap();
                oracle.set_node_active(spec.node, false).unwrap();
            }
        }
        let universe = [1, 2, 3, 4, 5, 6, 7];
        assert_matches(&bulk, &oracle, &universe);
        assert_matches(&incremental, &oracle, &universe);
    }
}

#[test]
fn try_from_topology_rejects_conflicts_and_limits_in_input_order() {
    let engine = EngineConfig::CompactBfs;
    let unlimited = GraphLimits::default();
    let node = |node, active| NodeSpec { node, active };
    let edge = |source, target, enabled| EdgeSpec {
        source,
        target,
        enabled,
    };
    let result =
        LifecycleGraph::try_from_topology(engine, unlimited, &[node(1, true), node(1, false)], &[]);
    assert_eq!(
        result.err(),
        Some(LifecycleError::ConflictingNode { node: 1 })
    );
    let result = LifecycleGraph::try_from_topology(
        engine,
        unlimited,
        &[],
        &[edge(1, 2, true), edge(2, 1, false)],
    );
    assert_eq!(
        result.err(),
        Some(LifecycleError::ConflictingEdge {
            source: 2,
            target: 1
        })
    );
    let result = LifecycleGraph::try_from_topology(engine, unlimited, &[], &[edge(3, 3, true)]);
    assert_eq!(
        result.err(),
        Some(LifecycleError::Graph(GraphError::SelfLoop { node: 3 }))
    );
    let limits = GraphLimits {
        max_nodes: Some(2),
        max_edges: Some(1),
    };
    let result = LifecycleGraph::try_from_topology(
        engine,
        limits,
        &[node(1, false)],
        &[edge(1, 2, true), edge(2, 1, true), edge(1, 3, true)],
    );
    assert_eq!(
        result.err(),
        Some(LifecycleError::Graph(GraphError::NodeLimitExceeded {
            limit: 2
        }))
    );
    let result = LifecycleGraph::try_from_topology(
        engine,
        limits,
        &[],
        &[edge(1, 2, false), edge(2, 1, false)],
    )
    .unwrap();
    assert_eq!(result.edge_count(), 1);
    assert_eq!(result.enabled_edge_count(), 0);
}

#[test]
fn repeated_remove_and_reinsert_keeps_engine_storage_bounded() {
    for engine in ENGINES {
        let mut graph = LifecycleGraph::new(engine).unwrap();
        for node in 0..64 {
            graph.link(node, node + 1).unwrap();
        }
        let mut peak = 0;
        for round in 0..20_000_u64 {
            // Distinct fresh IDs: every removal retires an engine slot.
            let fresh = 1_000 + round;
            graph.link(fresh, round % 64).unwrap();
            graph.remove_node(fresh);
            // Same-ID churn reuses the slot.
            graph.remove_node(64);
            graph.link(63, 64).unwrap();
            peak = peak.max(graph.engine_vertex_count());
        }
        assert!(peak <= 2 * 65 + 1024 + 1, "peak {peak}");
        assert!(graph.connected(0, 64).unwrap());
        assert_eq!(graph.node_count(), 65);
        assert_eq!(graph.edge_count(), 64);
    }
}

/// Lifecycle runs on `compact-bfs` and, from 1.1, `cluster-forest`. Every
/// constructor rejects `ett` and `hdt` explicitly (before examining any input)
/// and never falls back to another engine; the plain engines stay available.
#[test]
fn unsupported_engines_are_rejected_without_a_graph() {
    assert!(LifecycleGraph::supports_engine(EngineConfig::CompactBfs));
    assert!(LifecycleGraph::supports_engine(EngineConfig::ClusterForest));
    for engine in [EngineConfig::EulerTour, EngineConfig::Hdt] {
        assert!(!LifecycleGraph::supports_engine(engine));
        let expected = LifecycleError::UnsupportedEngine { engine };
        assert_eq!(LifecycleGraph::new(engine).err(), Some(expected));
        let limits = GraphLimits {
            max_nodes: Some(1),
            max_edges: Some(0),
        };
        assert_eq!(
            LifecycleGraph::with_limits(engine, limits).err(),
            Some(expected)
        );
        // Rejected before entries: a self-loop or a limit violation would
        // otherwise be reported first.
        let result = LifecycleGraph::try_from_topology(
            engine,
            limits,
            &[
                NodeSpec {
                    node: 1,
                    active: true,
                },
                NodeSpec {
                    node: 2,
                    active: true,
                },
            ],
            &[EdgeSpec {
                source: 3,
                target: 3,
                enabled: true,
            }],
        );
        assert_eq!(result.err(), Some(expected));
        let message = expected.to_string();
        assert!(
            message.contains(engine.as_str()) && message.contains("compact-bfs"),
            "{message}"
        );
        // Non-lifecycle use of the same engine is unchanged.
        let mut plain = knotrel_core::ConnectivityGraph::new(engine);
        assert_eq!(plain.link(1, 2), Ok(true));
        assert_eq!(plain.connected(1, 2), Ok(true));
        assert_eq!(plain.engine(), engine);
    }
    let graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
    assert_eq!(graph.engine(), EngineConfig::CompactBfs);
}
