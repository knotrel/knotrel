//! The properties of the stable diagnostic structs that their documentation
//! lists as guaranteed: `LifecycleStorage` (from
//! `LifecycleGraph::storage_stats`), `ForestStats`, `HdtStats`,
//! `HdtStorageStats` and `HdtLevelStorage` (docs/compatibility.md, "Diagnostic values").
//!
//! Only guaranteed properties are asserted here. Exact counter, capacity and
//! byte values are implementation-dependent and deliberately not checked.

use knotrel_core::{
    ConnectivityGraph, EngineConfig, ForestGraph, ForestStats, HdtGraph, HdtStats, HdtStorageStats,
    LifecycleGraph, LifecycleStorage,
};
use std::collections::BTreeSet;

/// Deterministic 64-bit linear congruential generator (Knuth's MMIX
/// constants), so every history is reproducible without a dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Number of connected components of `nodes` under `edges`, by union-find.
/// Independent of every engine.
fn components(nodes: &BTreeSet<u64>, edges: &BTreeSet<(u64, u64)>) -> usize {
    let index: std::collections::BTreeMap<u64, usize> =
        nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut count = nodes.len();
    for &(a, b) in edges {
        let (ra, rb) = (find(&mut parent, index[&a]), find(&mut parent, index[&b]));
        if ra != rb {
            parent[ra] = rb;
            count -= 1;
        }
    }
    count
}

/// Asserts every guaranteed property of an HDT storage snapshot.
fn check_hdt_storage(
    graph: &HdtGraph,
    storage: &HdtStorageStats,
    nodes: &BTreeSet<u64>,
    edges: &BTreeSet<(u64, u64)>,
) {
    assert_eq!(storage.vertex_count, graph.node_count());
    assert_eq!(storage.edge_count, graph.edge_count());
    assert_eq!(storage.vertex_count, nodes.len());
    assert_eq!(storage.edge_count, edges.len());

    let per_level_capacity: usize = storage
        .levels
        .iter()
        .map(|level| {
            level.token_capacity_bytes
                + level.vertex_index_capacity_bytes
                + level.free_list_capacity_bytes
                + level.adjacency_capacity_bytes
                + level.mapping_capacity_bytes
        })
        .sum();
    assert_eq!(
        storage.vector_capacity_bytes,
        storage.level_capacity_bytes + storage.edge_arc_capacity_bytes + per_level_capacity
    );

    assert_eq!(storage.levels.is_empty(), storage.vertex_count == 0);
    assert_eq!(storage.available_levels == 0, storage.vertex_count == 0);
    assert!(storage.levels.len() <= storage.available_levels);
    for (i, level) in storage.levels.iter().enumerate() {
        assert_eq!(level.level, i, "levels ascend from 0");
    }
    for pair in storage.levels.windows(2) {
        assert!(pair[1].tree_edges <= pair[0].tree_edges, "nested forests");
    }

    if let Some(base) = storage.levels.first() {
        assert_eq!(
            base.tree_edges,
            storage.vertex_count - components(nodes, edges),
            "level 0 is a spanning forest"
        );
        let tree_incidences: usize = storage.levels.iter().map(|l| l.tree_incidences).sum();
        assert_eq!(tree_incidences, 2 * base.tree_edges);
    }
    let incidences: usize = storage
        .levels
        .iter()
        .map(|level| level.tree_incidences + level.non_tree_incidences)
        .sum();
    assert_eq!(incidences, 2 * storage.edge_count);
}

/// Asserts the guaranteed relations between `HdtStats` counters.
fn check_hdt_stats_relations(stats: HdtStats) {
    assert!(stats.replacements <= stats.tree_cuts);
    assert!(stats.replacements <= stats.candidate_edges);
    assert!(stats.tree_cuts <= stats.levels_visited);
}

/// Asserts that every counter of `after` is at least the one of `before`.
fn assert_counters_grew(before: HdtStats, after: HdtStats) {
    assert!(after.tree_cuts >= before.tree_cuts);
    assert!(after.candidate_edges >= before.candidate_edges);
    assert!(after.replacements >= before.replacements);
    assert!(after.tree_promotions >= before.tree_promotions);
    assert!(after.non_tree_promotions >= before.non_tree_promotions);
    assert!(after.levels_visited >= before.levels_visited);
}

#[test]
fn hdt_empty_graph_reports_zero_counts_and_no_levels() {
    let graph = HdtGraph::new();
    assert_eq!(graph.stats(), HdtStats::default());
    assert_eq!(HdtGraph::default().stats(), HdtStats::default());
    let storage = graph.storage_stats();
    assert_eq!(storage.vertex_count, 0);
    assert_eq!(storage.edge_count, 0);
    assert_eq!(storage.available_levels, 0);
    assert!(storage.levels.is_empty());
    check_hdt_storage(&graph, &storage, &BTreeSet::new(), &BTreeSet::new());
}

/// Dense churn on a small vertex set forces tree cuts, replacements and
/// promotions to several levels; every guaranteed property holds after every
/// operation, and queries change neither the counters nor the graph.
#[test]
fn hdt_guaranteed_properties_hold_along_random_histories() {
    for seed in [1_u64, 7, 42, 2026] {
        let mut rng = Lcg(seed);
        let mut graph = HdtGraph::new();
        let mut nodes = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut previous_stats = graph.stats();
        let mut previous_levels = 0;
        let mut promoted = false;
        for step in 0..3_000 {
            // The vertex universe grows slowly, so available_levels grows too.
            let universe = 8 + step / 100;
            let a = rng.below(universe);
            let b = rng.below(universe);
            match rng.below(10) {
                0 => {
                    graph.add_node(a);
                    nodes.insert(a);
                }
                1..=5 if a != b => {
                    graph.link(a, b).unwrap();
                    nodes.extend([a, b]);
                    edges.insert((a.min(b), a.max(b)));
                }
                6..=9 if a != b => {
                    graph.cut(a, b).unwrap();
                    edges.remove(&(a.min(b), a.max(b)));
                }
                _ => {}
            }

            let stats = graph.stats();
            check_hdt_stats_relations(stats);
            assert_counters_grew(previous_stats, stats);
            previous_stats = stats;

            let storage = graph.storage_stats();
            check_hdt_storage(&graph, &storage, &nodes, &edges);
            assert!(
                storage.available_levels >= previous_levels,
                "never decreases"
            );
            previous_levels = storage.available_levels;
            promoted |= storage.levels.len() > 1;

            // Queries change neither the counters nor the graph.
            let _ = graph.connected(a, b);
            let _ = graph.node_count();
            let _ = graph.edge_count();
            assert_eq!(graph.stats(), stats);
            assert_eq!(graph.storage_stats(), storage);
        }
        assert!(
            previous_stats.tree_cuts > 0,
            "seed {seed}: history cut tree edges"
        );
        assert!(promoted, "seed {seed}: history reached a second level");

        graph.reset_stats();
        assert_eq!(graph.stats(), HdtStats::default());
        assert_eq!(graph.node_count(), nodes.len(), "reset keeps the graph");
        assert_eq!(graph.edge_count(), edges.len(), "reset keeps the graph");
    }
}

/// A snapshot is computed at the call and does not follow later mutations.
#[test]
fn hdt_storage_snapshot_is_taken_at_the_call() {
    let mut graph = HdtGraph::new();
    graph.link(1, 2).unwrap();
    let snapshot = graph.storage_stats();
    graph.link(2, 3).unwrap();
    assert_eq!(snapshot.vertex_count, 2);
    assert_eq!(snapshot.edge_count, 1);
    assert_eq!(graph.storage_stats().edge_count, 2);
}

#[test]
fn hdt_selector_and_qualification_are_stable_values() {
    assert_eq!(EngineConfig::Hdt.as_str(), "hdt");
    assert!(EngineConfig::Hdt.is_experimental());
    assert!(!EngineConfig::CompactBfs.is_experimental());
    assert_eq!(
        ConnectivityGraph::new(EngineConfig::Hdt).engine(),
        EngineConfig::Hdt
    );
}

/// Asserts every guaranteed property of a lifecycle storage snapshot.
fn check_lifecycle_storage(graph: &LifecycleGraph, storage: LifecycleStorage) {
    assert_eq!(storage.catalog_nodes, graph.node_count());
    assert_eq!(storage.engine_vertices, graph.engine_vertex_count());
    assert_eq!(
        storage.engine_vertices,
        storage.catalog_nodes + storage.retired_vertices
    );
    assert_eq!(storage.incident_entries, 2 * graph.edge_count());
    assert!(storage.catalog_node_capacity >= storage.catalog_nodes);
    assert!(storage.incident_capacity >= storage.incident_entries);
    assert!(storage.engine_vertices <= 2 * storage.catalog_nodes + 1024);
}

#[test]
fn lifecycle_new_graph_reports_zero_logical_sizes() {
    let graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
    let storage = graph.storage_stats();
    assert_eq!(storage.engine_vertices, 0);
    assert_eq!(storage.retired_vertices, 0);
    assert_eq!(storage.catalog_nodes, 0);
    assert_eq!(storage.incident_entries, 0);
    check_lifecycle_storage(&graph, storage);
}

/// Fresh-ID churn with removals creates retired slots and compactions;
/// disabled edges and inactive nodes stay registered. Every guaranteed
/// property holds after every operation, and the call changes no state.
#[test]
fn lifecycle_guaranteed_properties_hold_along_random_histories() {
    for seed in [3_u64, 11, 99] {
        let mut rng = Lcg(seed);
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        let mut saw_retired = false;
        let mut saw_compaction = false;
        let mut previous_retired = 0;
        let mut next_fresh = 1_000_000_u64;
        for _ in 0..6_000 {
            let a = rng.below(64);
            let b = rng.below(64);
            match rng.below(12) {
                0..=3 if a != b => {
                    graph.link(a, b).unwrap();
                }
                4 if a != b => {
                    graph.cut(a, b).unwrap();
                }
                5 if a != b => {
                    let _ = graph.set_edge_enabled(a, b, rng.below(2) == 0);
                }
                6 => {
                    let _ = graph.set_node_active(a, rng.below(2) == 0);
                }
                7 => {
                    graph.remove_node(a);
                }
                _ => {
                    // Fresh IDs, linked then removed, leave retired slots.
                    let fresh = next_fresh;
                    next_fresh += 1;
                    graph.link(a, fresh).unwrap();
                    assert!(graph.remove_node(fresh));
                }
            }
            let before = (
                graph.node_count(),
                graph.edge_count(),
                graph.active_node_count(),
                graph.enabled_edge_count(),
                graph.effective_edge_count(),
            );
            let storage = graph.storage_stats();
            check_lifecycle_storage(&graph, storage);
            saw_retired |= storage.retired_vertices > 0;
            // A compaction drops about 1024 retired slots at once; one step
            // without compaction retires or reuses at most two.
            saw_compaction |= storage.retired_vertices + 1000 < previous_retired;
            previous_retired = storage.retired_vertices;
            let after = (
                graph.node_count(),
                graph.edge_count(),
                graph.active_node_count(),
                graph.enabled_edge_count(),
                graph.effective_edge_count(),
            );
            assert_eq!(before, after, "storage_stats changes no state");
            assert_eq!(graph.storage_stats(), storage);
        }
        assert!(saw_retired, "seed {seed}: history created retired slots");
        assert!(
            saw_compaction,
            "seed {seed}: history compacted retired slots"
        );
    }
}

/// The returned value is a copy taken at the call.
#[test]
fn lifecycle_storage_snapshot_is_taken_at_the_call() {
    let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
    graph.link(1, 2).unwrap();
    let snapshot = graph.storage_stats();
    graph.link(2, 3).unwrap();
    assert_eq!(snapshot.catalog_nodes, 2);
    assert_eq!(snapshot.incident_entries, 2);
    let now = graph.storage_stats();
    assert_eq!(now.catalog_nodes, 3);
    assert_eq!(now.incident_entries, 4);
}

/// Asserts the guaranteed relations between `ForestStats` counters.
fn check_forest_stats_relations(stats: ForestStats) {
    assert!(stats.replacements <= stats.tree_cuts);
    assert!(stats.replacements <= stats.candidate_edges);
    assert!(stats.replacements <= stats.scanned_vertices);
}

#[test]
fn ett_new_graph_reports_zero_counters() {
    assert_eq!(ForestGraph::new().stats(), ForestStats::default());
    assert_eq!(ForestGraph::default().stats(), ForestStats::default());
}

/// Random links and cuts of existing edges force tree cuts with and without
/// replacements; the counter relations hold after every operation, counters
/// never decrease, queries change nothing, and a reset zeroes the counters but
/// keeps the graph.
#[test]
fn ett_guaranteed_counter_properties_hold_along_random_histories() {
    for seed in [5_u64, 13, 77] {
        let mut rng = Lcg(seed);
        let mut graph = ForestGraph::new();
        let mut edges = BTreeSet::new();
        let mut previous = graph.stats();
        for _ in 0..3_000 {
            let a = rng.below(24);
            let b = rng.below(24);
            match rng.below(10) {
                0 => {
                    graph.add_node(a);
                }
                1..=5 if a != b => {
                    graph.link(a, b).unwrap();
                    edges.insert((a.min(b), a.max(b)));
                }
                6..=9 if !edges.is_empty() => {
                    // Cut an existing edge, so the graph stays sparse enough
                    // for some tree cuts to split a component.
                    let index = usize::try_from(rng.below(edges.len() as u64)).unwrap();
                    let (x, y) = *edges.iter().nth(index).unwrap();
                    edges.remove(&(x, y));
                    assert_eq!(graph.cut(x, y), Ok(true));
                }
                _ => {}
            }
            let stats = graph.stats();
            check_forest_stats_relations(stats);
            assert!(stats.tree_cuts >= previous.tree_cuts);
            assert!(stats.scanned_vertices >= previous.scanned_vertices);
            assert!(stats.candidate_edges >= previous.candidate_edges);
            assert!(stats.replacements >= previous.replacements);
            previous = stats;

            let counts = (graph.node_count(), graph.edge_count());
            let _ = graph.connected(a, b);
            assert_eq!(graph.stats(), stats, "queries change no counter");
            assert_eq!((graph.node_count(), graph.edge_count()), counts);
        }
        assert!(
            previous.replacements > 0,
            "seed {seed}: history replaced edges"
        );
        assert!(
            previous.replacements < previous.tree_cuts,
            "seed {seed}: history also split components"
        );
        let counts = (graph.node_count(), graph.edge_count());
        graph.reset_stats();
        assert_eq!(graph.stats(), ForestStats::default());
        assert_eq!((graph.node_count(), graph.edge_count()), counts);
    }
}

#[test]
fn ett_selector_and_qualification_are_stable_values() {
    assert_eq!(EngineConfig::EulerTour.as_str(), "ett");
    assert!(EngineConfig::EulerTour.is_experimental());
    assert_eq!(
        ConnectivityGraph::new(EngineConfig::EulerTour).engine(),
        EngineConfig::EulerTour
    );
}
