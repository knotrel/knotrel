//! Exact general-graph connectivity using one Euler-tour spanning forest.
use crate::{GraphError, NodeId, forest::Forest};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

/// Cumulative replacement-search work; counters saturate at u64::MAX.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ForestStats {
    /// Deleted edges that belonged to the maintained spanning forest.
    pub tree_cuts: u64,
    /// Candidate-bearing vertices yielded on the smaller side after tree cuts.
    /// Pruned empty regions and unvisited vertices after success are excluded.
    pub scanned_vertices: u64,
    /// Non-tree adjacency entries tested for a crossing replacement.
    pub candidate_edges: u64,
    /// Successful promotions of non-tree edges into the forest.
    pub replacements: u64,
}

/// Experimental exact connectivity with deterministic Euler-tour trees.
///
/// Maintains a spanning forest: each graph component has exactly one tree.
/// Non-tree edges are stored at both endpoints. Removing a forest edge scans
/// non-tree edges incident to its smaller side; a crossing edge reconnects the
/// trees, or exhaustion proves a real split. This is not HDT: repeated scans have
/// no polylogarithmic amortized bound. The default [`crate::Graph`] is unchanged.
///
/// AVL root queries cost O(log V), plus O(log V) ID translation. Link costs
/// O(log V + log E) amortized including arena growth. A tree cut costs (amortized for free-list growth)
/// O(log V + log E + min(s, (k+1) log V) + c log V), where s is the
/// smaller side size, k the yielded candidate-bearing vertices, and c the
/// examined non-tree incidences. Empty candidate sets require no enumeration. Non-tree cuts cost O(log V + log E).
/// A single token-arena or free-list growth can add O(V) time. Logical storage is O(V + E); allocated
/// arenas retain O(V) high-water capacity, and removed tree-edge slots are reused.
/// Queries allocate no scratch, cache no answers and use no internal locks.
#[derive(Debug, Default)]
pub struct ForestGraph {
    ids: BTreeMap<NodeId, usize>,
    non_tree: Vec<BTreeSet<usize>>,
    edges: BTreeMap<(usize, usize), Option<(usize, usize)>>,
    forest: Forest,
    stats: ForestStats,
}
impl ForestGraph {
    /// Creates an empty graph in O(1) time.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an isolated vertex; returns false for an existing ID.
    ///
    /// O(log V) amortized; arena/vector growth can take O(V) in one call.
    pub fn add_node(&mut self, node: NodeId) -> bool {
        if let Entry::Vacant(entry) = self.ids.entry(node) {
            let index = self.non_tree.len();
            self.non_tree.push(BTreeSet::new());
            self.forest.add_vertex();
            entry.insert(index);
            true
        } else {
            false
        }
    }
    /// Inserts a simple undirected edge, registering absent endpoints.
    ///
    /// Returns false for an existing edge, including reversed endpoints.
    /// # Errors
    /// Rejects self-loops before registering either endpoint.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::distinct(source, target)?;
        self.add_node(source);
        self.add_node(target);
        let (a, b) = (self.ids[&source], self.ids[&target]);
        let key = (a.min(b), a.max(b));
        if self.edges.contains_key(&key) {
            return Ok(false);
        }
        let arcs = if self.forest.connected(a, b) {
            self.non_tree[a].insert(b);
            self.non_tree[b].insert(a);
            self.forest.set_candidate(a, true);
            self.forest.set_candidate(b, true);
            None
        } else {
            Some(self.forest.link(a, b))
        };
        self.edges.insert(key, arcs);
        Ok(true)
    }
    /// Removes an edge while retaining both vertices; absent edges return false.
    ///
    /// Tree cuts synchronously complete replacement search before returning.
    /// # Errors
    /// Rejects self-loops without changing graph state.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        Self::distinct(source, target)?;
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return Ok(false);
        };
        let key = (a.min(b), a.max(b));
        let Some(arcs) = self.edges.remove(&key) else {
            return Ok(false);
        };
        if let Some(arcs) = arcs {
            self.forest.cut(arcs);
            self.stats.tree_cuts = self.stats.tree_cuts.saturating_add(1);
            let mut replacement = None;
            'search: for v in self.forest.smaller_candidates(a, b) {
                self.stats.scanned_vertices = self.stats.scanned_vertices.saturating_add(1);
                for &w in &self.non_tree[v] {
                    self.stats.candidate_edges = self.stats.candidate_edges.saturating_add(1);
                    if !self.forest.connected(v, w) {
                        replacement = Some((v, w));
                        break 'search;
                    }
                }
            }
            if let Some((v, w)) = replacement {
                self.non_tree[v].remove(&w);
                self.non_tree[w].remove(&v);
                self.forest.set_candidate(v, !self.non_tree[v].is_empty());
                self.forest.set_candidate(w, !self.non_tree[w].is_empty());
                let arcs = self.forest.link(v, w);
                *self
                    .edges
                    .get_mut(&(v.min(w), v.max(w)))
                    .expect("replacement edge exists") = Some(arcs);
                self.stats.replacements = self.stats.replacements.saturating_add(1);
            }
        } else {
            self.non_tree[a].remove(&b);
            self.non_tree[b].remove(&a);
            self.forest.set_candidate(a, !self.non_tree[a].is_empty());
            self.forest.set_candidate(b, !self.non_tree[b].is_empty());
        }
        Ok(true)
    }
    /// Answers exact connectivity in O(log V) worst-case time without allocation.
    ///
    /// Existing vertices connect to themselves, including isolated vertices.
    /// # Errors
    /// Reports an absent source before an absent target.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        let a = *self
            .ids
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let b = *self
            .ids
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        Ok(self.forest.connected(a, b))
    }
    /// Returns the number of registered vertices in O(1) time.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.ids.len()
    }
    /// Returns the number of undirected graph edges in O(1) time.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
    /// Returns cumulative replacement-search counters in O(1) time.
    #[must_use]
    pub fn stats(&self) -> ForestStats {
        self.stats
    }
    /// Clears only diagnostic counters, preserving all graph state.
    pub fn reset_stats(&mut self) {
        self.stats = ForestStats::default();
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
    use super::ForestGraph;
    #[test]
    fn general_graph_updates_preserve_avl_and_edge_classification() {
        let mut g = ForestGraph::new();
        let mut seed = 123_u64;
        for step in 0..3000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = (seed >> 32) % 50;
            let b = (seed >> 48) % 50;
            if step % 3 == 0 {
                let _ = g.cut(a, b);
            } else {
                let _ = g.link(a, b);
            }
            g.forest.validate();
            for (&(u, v), arcs) in &g.edges {
                assert!(g.forest.connected(u, v));
                assert_eq!(g.non_tree[u].contains(&v), arcs.is_none());
                assert_eq!(g.non_tree[v].contains(&u), arcs.is_none());
            }
            for (u, neighbors) in g.non_tree.iter().enumerate() {
                assert_eq!(g.forest.is_candidate(u), !neighbors.is_empty());
                for &v in neighbors {
                    assert_eq!(g.edges.get(&(u.min(v), u.max(v))), Some(&None));
                }
            }
        }
        let counts = (g.node_count(), g.edge_count());
        g.reset_stats();
        assert_eq!(g.stats(), super::ForestStats::default());
        assert_eq!(counts, (g.node_count(), g.edge_count()));
    }
}
