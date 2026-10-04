//! Deterministic level-based dynamic connectivity, following HDT (2001), §3.
//!
//! <https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf>
//! F_i contains tree edges at levels >= i. Exact-level incidences are indexed
//! separately. Each component of F_i has at most U / 2^i vertices, with U the
//! next power of two of the current vertex count. Growing U only relaxes bounds.

use crate::{GraphError, NodeId, hdt_forest::Forest};
use std::collections::{BTreeMap, BTreeSet};

type Key = (usize, usize);
type Arcs = (usize, usize);

/// Cumulative HDT repair work; each counter saturates at u64::MAX.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HdtStats {
    /// Removed edges that belonged to the spanning forest.
    pub tree_cuts: u64,
    /// Exact-level non-tree edges examined for replacement (once per visit).
    pub candidate_edges: u64,
    /// Successful replacements of a removed spanning edge.
    pub replacements: u64,
    /// Increases of a tree edge's level, each adding it to one higher forest.
    pub tree_promotions: u64,
    /// Increases of an internal non-tree edge's level.
    pub non_tree_promotions: u64,
    /// Replacement-search levels visited after tree cuts.
    pub levels_visited: u64,
}

#[derive(Debug)]
struct Edge {
    level: usize,
    // Empty for non-tree edges; otherwise one handle pair for each F_0..F_level.
    arcs: Vec<Arcs>,
}

// F_0 is dense to keep queries free of an extra translation. Higher levels
// materialize only touched vertices. Missing vertices are implicit isolated
// singletons with no incidences. Maps and local handles remain stable after cuts.
#[derive(Debug, Default)]
struct Level {
    forest: Forest,
    tree: Vec<BTreeSet<usize>>,
    non_tree: Vec<BTreeSet<usize>>,
    dense: bool,
    local_ids: BTreeMap<usize, usize>,
    global_ids: Vec<usize>,
}
impl Level {
    fn local(&self, v: usize) -> Option<usize> {
        if self.dense {
            (v < self.tree.len()).then_some(v)
        } else {
            self.local_ids.get(&v).copied()
        }
    }
    fn ensure(&mut self, v: usize) -> usize {
        if let Some(local) = self.local(v) {
            return local;
        }
        let local = self.tree.len();
        if self.dense {
            assert_eq!(v, local);
        } else {
            self.local_ids.insert(v, local);
            self.global_ids.push(v);
        }
        self.forest.add_vertex();
        self.tree.push(BTreeSet::new());
        self.non_tree.push(BTreeSet::new());
        local
    }
    fn connected(&self, a: usize, b: usize) -> bool {
        if a == b {
            return true;
        }
        match (self.local(a), self.local(b)) {
            (Some(a), Some(b)) => self.forest.connected(a, b),
            _ => false,
        }
    }
    fn component_size(&self, v: usize) -> usize {
        self.local(v)
            .map_or(1, |local| self.forest.component_size(local))
    }
    fn marked_vertex(&self, v: usize, tree: bool) -> Option<usize> {
        let local = self.forest.marked_vertex(self.local(v)?, tree)?;
        Some(if self.dense {
            local
        } else {
            self.global_ids[local]
        })
    }
    fn first_neighbor(&self, v: usize, tree: bool) -> usize {
        let local = self.local(v).expect("marked vertex exists");
        let sets = if tree { &self.tree } else { &self.non_tree };
        *sets[local].first().expect("marked incidence exists")
    }
    fn link(&mut self, a: usize, b: usize) -> Arcs {
        let a = self.ensure(a);
        let b = self.ensure(b);
        self.forest.link(a, b)
    }
    fn refresh(&mut self, local: usize) {
        self.forest.set_marks(
            local,
            !self.tree[local].is_empty(),
            !self.non_tree[local].is_empty(),
        );
    }
    // Sets always store global indices; only the owning vertex is translated.
    // Creating a sparse record is charged to its first edge promotion, O(log V).
    // Return the stable local endpoints so callers can reuse this translation.
    fn incidence(&mut self, a: usize, b: usize, tree: bool, insert: bool) -> (usize, usize) {
        let (local_a, local_b) = if insert {
            (self.ensure(a), self.ensure(b))
        } else {
            (
                self.local(a).expect("live endpoint"),
                self.local(b).expect("live endpoint"),
            )
        };
        let sets = if tree {
            &mut self.tree
        } else {
            &mut self.non_tree
        };
        if insert {
            sets[local_a].insert(b);
            sets[local_b].insert(a);
        } else {
            sets[local_a].remove(&b);
            sets[local_b].remove(&a);
        }
        self.refresh(local_a);
        self.refresh(local_b);
        (local_a, local_b)
    }
}

/// Experimental exact connectivity using deterministic HDT levels and AVL tours.
///
/// Preserves the [`crate::Graph`] contract, including growing arbitrary `u64`
/// vertices. This is an experimental implementation, not a production latency
/// guarantee. Compact BFS remains the default engine.
///
/// Queries cost O(log V) worst case, including ID translation, and allocate no
/// scratch. Across a history with at most N registered vertices, edge updates
/// have O(log² N) amortized cost; one cut may still inspect many edges. Logical
/// storage is O(E + V log V). Arenas/adjacency capacities retain historical highs.
/// Vertex insertion costs O(log V) amortized, including ID indexing. Upper
/// levels store only vertices touched by promotions; creating these records is
/// charged to promotions. Vector reallocations can still cause linear spikes.
/// Neither graph state nor query answers are protected by internal locks.
#[derive(Debug, Default)]
pub struct HdtGraph {
    ids: BTreeMap<NodeId, usize>,
    edges: BTreeMap<Key, Edge>,
    levels: Vec<Level>,
    stats: HdtStats,
}

impl HdtGraph {
    pub(crate) fn contains_edge(&self, source: NodeId, target: NodeId) -> bool {
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return false;
        };
        self.edges.contains_key(&(a.min(b), a.max(b)))
    }

    /// Creates an empty graph in O(1) time.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an isolated vertex, returning false if already present.
    ///
    /// Registers only in the base forest. Growing the universe relaxes upper
    /// level size bounds; implicit isolated vertices need no upper records.
    pub fn add_node(&mut self, node: NodeId) -> bool {
        if self.ids.contains_key(&node) {
            return false;
        }
        let index = self.ids.len();
        if self.levels.is_empty() {
            self.levels.push(Level {
                dense: true,
                ..Default::default()
            });
        }
        self.levels[0].ensure(index);
        self.ids.insert(node, index);
        true
    }

    /// Inserts an edge at level zero, creating absent endpoints.
    ///
    /// Returns false for a duplicate, including reversed endpoints.
    /// # Errors
    /// Rejects self-loops before creating any vertex.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::distinct(source, target)?;
        self.add_node(source);
        self.add_node(target);
        let (a, b) = (self.ids[&source], self.ids[&target]);
        let key = Self::key(a, b);
        if self.edges.contains_key(&key) {
            return Ok(false);
        }
        let tree = !self.levels[0].forest.connected(a, b);
        let arcs = if tree {
            vec![self.levels[0].forest.link(a, b)]
        } else {
            Vec::new()
        };
        self.levels[0].incidence(a, b, tree, true);
        self.edges.insert(key, Edge { level: 0, arcs });
        Ok(true)
    }

    /// Removes an edge, retaining all vertices; absent edges return false.
    ///
    /// Every repair completes before returning. A single expensive cut is still
    /// possible: the update bound is amortized over edge lifetimes and promotions.
    /// # Errors
    /// Rejects self-loops without changing state.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::distinct(source, target)?;
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return Ok(false);
        };
        let Some(edge) = self.edges.remove(&Self::key(a, b)) else {
            return Ok(false);
        };
        let tree = !edge.arcs.is_empty();
        self.levels[edge.level].incidence(a, b, tree, false);
        if tree {
            self.stats.tree_cuts = self.stats.tree_cuts.saturating_add(1);
            for (i, arcs) in edge.arcs.into_iter().enumerate() {
                self.levels[i].forest.cut(arcs);
            }
            self.replace(a, b, edge.level);
        }
        Ok(true)
    }

    /// HDT Replace: promote all exact-level tree edges of the smaller side before
    /// visiting its non-tree edges; skip a level if the side has no candidates.
    /// Each failed candidate moves up one level and
    /// can be charged only O(log N) times per lifetime. Marked root-to-leaf
    /// searches cost O(log V); no uncharged whole-component enumeration occurs.
    /// A crossing replacement rejoins F_0..F_i, restoring every lower forest.
    fn replace(&mut self, a: usize, b: usize, start: usize) {
        for i in (0..=start).rev() {
            self.stats.levels_visited = self.stats.levels_visited.saturating_add(1);
            let forest = &self.levels[i];
            let small = if forest.component_size(a) <= forest.component_size(b) {
                a
            } else {
                b
            };
            // No exact-level incidence on this side means no crossing edge at
            // this level. Skipping promotions preserves nesting and size bounds:
            // the cut only splits trees. Higher-level replacements were already
            // excluded. This extra lookup costs O(log V) per visited level.
            if self.levels[i].marked_vertex(small, false).is_none() {
                continue;
            }
            while let Some(v) = self.levels[i].marked_vertex(small, true) {
                let w = self.levels[i].first_neighbor(v, true);
                self.promote(v, w, i, true);
            }
            while let Some(v) = self.levels[i].marked_vertex(small, false) {
                let w = self.levels[i].first_neighbor(v, false);
                self.stats.candidate_edges = self.stats.candidate_edges.saturating_add(1);
                if self.levels[i].connected(v, w) {
                    self.promote(v, w, i, false);
                } else {
                    self.levels[i].incidence(v, w, false, false);
                    self.levels[i].incidence(v, w, true, true);
                    let mut arcs = Vec::with_capacity(i + 1);
                    for level in &mut self.levels[..=i] {
                        arcs.push(level.link(v, w));
                    }
                    self.edges
                        .get_mut(&Self::key(v, w))
                        .expect("live replacement")
                        .arcs = arcs;
                    self.stats.replacements = self.stats.replacements.saturating_add(1);
                    return;
                }
            }
        }
    }

    /// Raise one edge; the smaller-side size bound ensures the next level exists.
    /// Tree promotions precede non-tree promotions, so an internal non-tree edge
    /// already has a path in F_(i+1). Only a tree promotion adds a new tour copy.
    fn promote(&mut self, a: usize, b: usize, i: usize, tree: bool) {
        debug_assert!(i + 1 < (usize::BITS - (self.ids.len() - 1).leading_zeros()) as usize + 1);
        if i + 1 == self.levels.len() {
            self.levels.push(Level::default());
        }
        self.levels[i].incidence(a, b, tree, false);
        let (local_a, local_b) = self.levels[i + 1].incidence(a, b, tree, true);
        let arcs = if tree {
            // Incidence insertion already materialized and translated both endpoints.
            Some(self.levels[i + 1].forest.link(local_a, local_b))
        } else {
            debug_assert!(self.levels[i + 1].connected(a, b));
            None
        };
        let edge = self
            .edges
            .get_mut(&Self::key(a, b))
            .expect("live promoted edge");
        debug_assert_eq!(edge.level, i);
        edge.level += 1;
        if let Some(arcs) = arcs {
            edge.arcs.push(arcs);
            self.stats.tree_promotions = self.stats.tree_promotions.saturating_add(1);
        } else {
            self.stats.non_tree_promotions = self.stats.non_tree_promotions.saturating_add(1);
        }
    }

    /// Answers exact connectivity in O(log V), without allocation or mutation.
    ///
    /// A known vertex connects to itself. Unknown source is reported first.
    /// # Errors
    /// Returns [`GraphError::UnknownNode`] for an unregistered endpoint.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        let a = *self
            .ids
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let b = *self
            .ids
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        Ok(self.levels[0].forest.connected(a, b))
    }

    /// Returns the number of registered vertices in O(1).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.ids.len()
    }
    /// Returns the number of graph edges in O(1).
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
    /// Returns diagnostic counters in O(1).
    #[must_use]
    pub fn stats(&self) -> HdtStats {
        self.stats
    }
    /// Clears diagnostic counters without altering graph state.
    pub fn reset_stats(&mut self) {
        self.stats = HdtStats::default();
    }

    /// Collects structural storage accounting without changing graph state.
    ///
    /// Takes O(V log V + E) time in the worst case and allocates the returned
    /// per-level snapshot. Call outside measured operations. Vector capacities
    /// exclude allocator overhead; ordered payload excludes B-tree node overhead
    /// and spare slots. This is deliberately not a total heap or RSS estimate.
    #[must_use]
    pub fn storage_stats(&self) -> crate::HdtStorageStats {
        let levels: Vec<_> = self
            .levels
            .iter()
            .enumerate()
            .map(|(i, level)| {
                let mut storage = level.forest.storage();
                storage.level = i;
                storage.mapping_capacity_bytes = level.global_ids.capacity() * size_of::<usize>();
                storage.mapping_payload_bytes = level.local_ids.len() * size_of::<(usize, usize)>();
                storage.tree_incidences = level.tree.iter().map(BTreeSet::len).sum();
                storage.non_tree_incidences = level.non_tree.iter().map(BTreeSet::len).sum();
                storage.adjacency_capacity_bytes = (level.tree.capacity()
                    + level.non_tree.capacity())
                    * size_of::<BTreeSet<usize>>();
                storage
            })
            .collect();
        let edge_arc_capacity_bytes = self
            .edges
            .values()
            .map(|edge| edge.arcs.capacity() * size_of::<Arcs>())
            .sum::<usize>();
        let level_capacity_bytes = self.levels.capacity() * size_of::<Level>();
        let vector_capacity_bytes = level_capacity_bytes
            + edge_arc_capacity_bytes
            + levels
                .iter()
                .map(|s| {
                    s.token_capacity_bytes
                        + s.vertex_index_capacity_bytes
                        + s.free_list_capacity_bytes
                        + s.adjacency_capacity_bytes
                        + s.mapping_capacity_bytes
                })
                .sum::<usize>();
        let ordered_payload_bytes = self.ids.len() * size_of::<(NodeId, usize)>()
            + self.edges.len() * size_of::<(Key, Edge)>()
            + levels
                .iter()
                .map(|s| {
                    (s.tree_incidences + s.non_tree_incidences) * size_of::<usize>()
                        + s.mapping_payload_bytes
                })
                .sum::<usize>();
        crate::HdtStorageStats {
            vertex_count: self.ids.len(),
            edge_count: self.edges.len(),
            available_levels: if self.ids.is_empty() {
                0
            } else {
                (usize::BITS - (self.ids.len() - 1).leading_zeros()) as usize + 1
            },
            vector_capacity_bytes,
            ordered_payload_bytes,
            edge_arc_capacity_bytes,
            level_capacity_bytes,
            levels,
        }
    }

    fn key(a: usize, b: usize) -> Key {
        (a.min(b), a.max(b))
    }
    fn distinct(a: NodeId, b: NodeId) -> Result<(), GraphError> {
        if a == b {
            Err(GraphError::SelfLoop { node: a })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent closure of all edges at/above each level must match that
    /// forest. Also verify size bounds, exact-level incidences and arc counts.
    fn validate(graph: &HdtGraph) {
        let n = graph.node_count();
        let universe = n.max(1).next_power_of_two();
        for (i, level) in graph.levels.iter().enumerate() {
            level.forest.validate();
            let mut closure = vec![vec![false; n]; n];
            for (v, row) in closure.iter_mut().enumerate() {
                row[v] = true;
                assert!(level.component_size(v) <= universe >> i);
                if let Some(local) = level.local(v) {
                    assert_eq!(
                        level.forest.marks(local),
                        (
                            !level.tree[local].is_empty(),
                            !level.non_tree[local].is_empty()
                        )
                    );
                    for (tree, adjacency) in [(true, &level.tree), (false, &level.non_tree)] {
                        for &w in &adjacency[local] {
                            assert!(adjacency[level.local(w).unwrap()].contains(&v));
                            let edge = &graph.edges[&HdtGraph::key(v, w)];
                            assert_eq!(edge.level, i);
                            assert_eq!(!edge.arcs.is_empty(), tree);
                        }
                    }
                } else {
                    assert_eq!(level.marked_vertex(v, true), None);
                    assert_eq!(level.marked_vertex(v, false), None);
                }
            }
            for (&(a, b), edge) in &graph.edges {
                assert!(edge.level < graph.levels.len());
                assert_eq!(
                    edge.arcs.len(),
                    if edge.arcs.is_empty() {
                        0
                    } else {
                        edge.level + 1
                    }
                );
                if edge.level >= i {
                    closure[a][b] = true;
                    closure[b][a] = true;
                }
                if edge.level == i {
                    let sets = if edge.arcs.is_empty() {
                        &level.non_tree
                    } else {
                        &level.tree
                    };
                    assert!(
                        sets[level.local(a).unwrap()].contains(&b)
                            && sets[level.local(b).unwrap()].contains(&a)
                    );
                }
            }
            for k in 0..n {
                for a in 0..n {
                    for b in 0..n {
                        closure[a][b] |= closure[a][k] && closure[k][b];
                    }
                }
            }
            for (a, row) in closure.iter().enumerate() {
                for (b, &expected) in row.iter().enumerate() {
                    assert_eq!(level.connected(a, b), expected, "level {i}, pair {a},{b}");
                }
            }
        }
    }

    #[test]
    fn promoted_tree_uses_local_handles_for_sparse_global_indices() {
        let mut level = super::Level::default();
        level.ensure(900);
        level.ensure(400);
        let (a, b) = level.incidence(400, 900, true, true);
        let arcs = level.forest.link(a, b);
        assert!(level.connected(400, 900));
        assert_eq!(level.component_size(400), 2);
        assert!(level.marked_vertex(900, true).is_some());
        level.forest.cut(arcs);
        assert!(!level.connected(400, 900));
        level.incidence(400, 900, true, false);
        assert!(level.marked_vertex(900, true).is_none());
        assert!(level.marked_vertex(400, true).is_none());
    }

    #[test]
    fn growing_mixed_updates_preserve_every_level_and_incidence() {
        for initial_seed in [1_u64, 97, 1337] {
            let mut graph = HdtGraph::new();
            let mut seed = initial_seed;
            for step in 0..600 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let bound = 2 + (step / 40).min(18);
                let a = (seed >> 32) % bound;
                let b = (seed >> 48) % bound;
                if step % 2 == 0 {
                    let _ = graph.cut(a, b);
                } else {
                    let _ = graph.link(a, b);
                }
                validate(&graph);
            }
            let ids: Vec<_> = graph.ids.keys().copied().collect();
            for &a in &ids {
                for &b in &ids {
                    let _ = graph.cut(a, b);
                    validate(&graph);
                }
            }
            assert_eq!(graph.edge_count(), 0);
        }
    }

    #[test]
    fn empty_higher_level_search_still_finds_lower_replacement() {
        let mut graph = HdtGraph::new();
        for v in 0..8 {
            graph.add_node(v);
        }
        for v in 0..7 {
            graph.link(v, v + 1).unwrap();
        }
        graph.link(0, 2).unwrap();
        graph.cut(2, 3).unwrap();
        assert_eq!(graph.edges[&(0, 1)].level, 1);
        graph.cut(0, 2).unwrap();
        graph.link(0, 2).unwrap(); // Reinsert as a level-zero non-tree edge.
        graph.reset_stats();
        graph.cut(0, 1).unwrap();
        assert_eq!(graph.stats().levels_visited, 2);
        assert_eq!(graph.stats().replacements, 1);
        assert_eq!(graph.stats().tree_promotions, 0);
        assert_eq!(graph.connected(0, 1), Ok(true));
        validate(&graph);
    }

    #[test]
    fn promoted_tree_deletions_repair_all_lower_forests_after_growth() {
        let mut graph = HdtGraph::new();
        for v in 0..32 {
            graph.add_node(v);
        }
        for v in 0..31 {
            graph.link(v, v + 1).unwrap();
        }
        // Fixed four-vertex cliques supply internal candidates as the path's
        // inter-block bridges split progressively smaller components.
        for base in (0..32).step_by(4) {
            for a in base..base + 4 {
                for b in a + 1..base + 4 {
                    graph.link(a, b).unwrap();
                }
            }
        }
        for v in [15, 7, 23, 3, 11, 19, 27, 1, 5, 9, 13] {
            graph.cut(v, v + 1).unwrap();
            validate(&graph);
        }
        assert!(graph.edges.values().any(|edge| edge.level >= 3));
        for v in 32..34 {
            graph.add_node(v);
            validate(&graph);
        }
        for v in 0..33 {
            graph.link(v, v + 1).unwrap();
            validate(&graph);
        }
        // Add cycles, then delete every old tree edge including promoted ones.
        for v in 0..32 {
            graph.link(v, v + 2).unwrap();
        }
        for v in 0..33 {
            graph.cut(v, v + 1).unwrap();
            validate(&graph);
        }
        let state = (
            graph.node_count(),
            graph.edge_count(),
            graph.connected(0, 32),
        );
        graph.reset_stats();
        assert_eq!(graph.stats(), HdtStats::default());
        assert_eq!(
            state,
            (
                graph.node_count(),
                graph.edge_count(),
                graph.connected(0, 32)
            )
        );
    }
}
