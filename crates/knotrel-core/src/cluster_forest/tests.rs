//! Invariant checker, oracle comparisons and review regressions for the
//! cluster forest (ADR 0014, sections 8 and 9).

use super::*;
use std::collections::{BTreeSet, HashMap, VecDeque};

// ----- invariant checker (ADR 0014, section 8) -----

/// Union-find over vertex handles.
struct Components {
    parent: HashMap<u32, u32>,
}

impl Components {
    fn new(vertices: &[u32]) -> Self {
        Self {
            parent: vertices.iter().map(|&v| (v, v)).collect(),
        }
    }
    fn find(&mut self, x: u32) -> u32 {
        let parent = self.parent[&x];
        if parent == x {
            return x;
        }
        let root = self.find(parent);
        self.parent.insert(x, root);
        root
    }
    fn union(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent.insert(a, b);
        }
    }
}

impl ClusterForestGraph {
    /// Full checker: every invariant of ADR 0014 section 8. Panics on a
    /// violation. O((V + E) · L + nodes · L) with hashing; test-only.
    pub(super) fn check_invariants(&self) {
        let vertices: Vec<u32> = self.ids.values().copied().collect();
        let max = self.max_level;

        // 8: L_max formula.
        let count = self.ids.len();
        let expected = if count <= 1 {
            1
        } else {
            ((usize::BITS - (count - 1).leading_zeros()) as u8).max(1)
        };
        assert_eq!(max, expected, "L_max = max(1, ceil(log2 U))");

        // 1: edges and level lists.
        let mut entries = 0usize;
        for (&(x, y), &edge) in &self.edge_index {
            let record = self.edges[edge as usize];
            assert_eq!(Self::key(record.ends[0], record.ends[1]), (x, y));
            assert!((1..=max).contains(&record.level), "edge level in 1..=L_max");
            for end in 0..2 {
                let vertex = record.ends[end];
                let bitmap = self.nodes[vertex as usize].bitmap;
                assert_ne!(bitmap & level_bit(record.level), 0, "bit set for an edge");
                let lists = &self.vertex_lists[self.nodes[vertex as usize].aux as usize];
                let list = &lists[ClusterForestGraph::list_index(bitmap, record.level)];
                assert_eq!(list.level, record.level);
                assert_eq!(list.edges[record.position[end] as usize], edge);
            }
        }
        for &vertex in &vertices {
            let node = &self.nodes[vertex as usize];
            assert_eq!(node.kind, Kind::Vertex);
            assert_eq!(node.size, 1);
            let lists = &self.vertex_lists[node.aux as usize];
            assert_eq!(node.bitmap.count_ones() as usize, lists.len());
            let mut previous = 0;
            for list in lists {
                assert!(list.level > previous, "levels ascending");
                previous = list.level;
                assert!(!list.edges.is_empty(), "no empty level entry");
                assert_ne!(node.bitmap & level_bit(list.level), 0);
                for &edge in &list.edges {
                    let record = self.edges[edge as usize];
                    assert!(record.ends.contains(&vertex));
                    assert_eq!(record.level, list.level);
                    let other = record.ends[0] ^ record.ends[1] ^ vertex;
                    assert_eq!(self.edge_index[&Self::key(vertex, other)], edge);
                    entries += 1;
                }
            }
        }
        assert_eq!(entries, 2 * self.edge_index.len());

        // 2: quiescence.
        for side in &self.sides {
            assert!(side.detached.is_empty(), "search scratch empty");
        }

        // Arena bookkeeping (CF2 review, finding 4): the node free list holds
        // exactly the free slots, once each; the edge free list and the live
        // edges partition the edge arena.
        let free_nodes: BTreeSet<u32> = self.free_nodes.iter().copied().collect();
        assert_eq!(
            free_nodes.len(),
            self.free_nodes.len(),
            "no node freed twice"
        );
        for (index, node) in self.nodes.iter().enumerate() {
            assert_eq!(
                node.kind == Kind::Free,
                free_nodes.contains(&(index as u32)),
                "free list holds exactly the free node slots"
            );
        }
        let free_edges: BTreeSet<u32> = self.free_edges.iter().copied().collect();
        assert_eq!(
            free_edges.len(),
            self.free_edges.len(),
            "no edge freed twice"
        );
        let live_edges: BTreeSet<u32> = self.edge_index.values().copied().collect();
        assert_eq!(
            live_edges.len(),
            self.edge_index.len(),
            "edge handles unique"
        );
        assert!(
            free_edges.is_disjoint(&live_edges),
            "no live edge on the free list"
        );
        assert_eq!(
            free_edges.len() + live_edges.len(),
            self.edges.len(),
            "every edge record is live or free"
        );

        // 3, 5, 6: forest shape, local trees, aggregates, size invariant.
        let mut cluster_count = 0usize;
        for (index, node) in self.nodes.iter().enumerate() {
            let handle = index as u32;
            if node.kind == Kind::Free {
                continue;
            }
            if node.kind == Kind::Vertex {
                assert!(
                    node.left == NONE && node.right == NONE,
                    "vertices are leaves"
                );
            }
            if node.parent != NONE {
                let parent = &self.nodes[node.parent as usize];
                assert!(
                    parent.left == handle || parent.right == handle,
                    "parent pointer agrees with child pointers"
                );
                assert!(matches!(
                    parent.kind,
                    Kind::Cluster | Kind::Rank | Kind::Path
                ));
            }
            if node.kind != Kind::Vertex {
                let (mut size, mut bitmap) = (0, 0);
                for child in [node.left, node.right] {
                    if child != NONE {
                        assert_eq!(self.nodes[child as usize].parent, handle);
                        size += self.nodes[child as usize].size;
                        bitmap |= self.nodes[child as usize].bitmap;
                    }
                }
                assert_eq!(node.size, size, "size aggregate");
                assert_eq!(node.bitmap, bitmap, "bitmap aggregate");
            }
            match node.kind {
                Kind::Rank => {
                    let (l, r) = (node.left, node.right);
                    assert!(l != NONE && r != NONE, "rank nodes are binary");
                    for child in [l, r] {
                        assert_eq!(self.nodes[child as usize].rank + 1, node.rank);
                    }
                }
                Kind::Cluster => {
                    cluster_count += 1;
                    assert!(node.level >= 1 && node.level <= max, "cluster level");
                    assert!(
                        u64::from(node.size) <= 1u64 << node.level,
                        "size invariant 2.1"
                    );
                    self.check_local_tree(handle);
                }
                Kind::Path => {
                    assert!(node.left != NONE && node.right != NONE);
                }
                Kind::Vertex | Kind::Free => {}
            }
            if matches!(node.kind, Kind::Vertex | Kind::Cluster) && node.parent != NONE {
                assert_eq!(node.rank, floor_log2(node.size), "leaf rank");
                let parent = self.forest_parent(handle);
                assert!(self.nodes[parent as usize].level > node.level);
            }
        }

        // Members of every forest node, from vertex chains.
        let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
        for &vertex in &vertices {
            let mut current = vertex;
            while current != NONE {
                members.entry(current).or_default().push(vertex);
                current = self.forest_parent(current);
            }
        }
        assert_eq!(
            members.len(),
            vertices.len() + cluster_count,
            "every cluster holds vertices"
        );
        for (&node, list) in &members {
            assert_eq!(self.nodes[node as usize].size as usize, list.len());
        }

        // 4 and 7: compression and cluster semantics at every spanned level.
        let mut by_level: Vec<Components> = Vec::new();
        let mut component_size: Vec<HashMap<u32, usize>> = Vec::new();
        for level in 0..=max {
            let mut components = Components::new(&vertices);
            for &edge in self.edge_index.values() {
                let record = self.edges[edge as usize];
                if record.level <= level {
                    components.union(record.ends[0], record.ends[1]);
                }
            }
            let mut sizes = HashMap::new();
            for &vertex in &vertices {
                *sizes.entry(components.find(vertex)).or_insert(0) += 1;
            }
            by_level.push(components);
            component_size.push(sizes);
        }
        for (&node, list) in &members {
            let own = self.nodes[node as usize].level;
            let parent = self.forest_parent(node);
            let top = if parent == NONE {
                max + 1
            } else {
                self.nodes[parent as usize].level
            };
            assert!(own < top);
            for level in own..top {
                let components = &mut by_level[level as usize];
                let first = components.find(list[0]);
                for &vertex in list {
                    assert_eq!(
                        components.find(vertex),
                        first,
                        "a stored node is connected in G_{level}"
                    );
                }
                assert_eq!(
                    component_size[level as usize][&first],
                    list.len(),
                    "a stored node is a whole component of G_{level}"
                );
            }
            let inside: BTreeSet<u32> = list.iter().copied().collect();
            for &vertex in list {
                let bitmap = self.nodes[vertex as usize].bitmap;
                for level in own + 1..top {
                    assert_eq!(
                        bitmap & level_bit(level),
                        0,
                        "no edge strictly inside a compressed span"
                    );
                }
            }
            if own >= 1 {
                let holds_own_edge = self.edge_index.values().any(|&edge| {
                    let record = self.edges[edge as usize];
                    record.level == own
                        && inside.contains(&record.ends[0])
                        && inside.contains(&record.ends[1])
                });
                assert!(
                    holds_own_edge,
                    "a stored cluster holds an edge of its level"
                );
            }
        }
    }

    /// Rank path of `cluster`: distinct root ranks in decreasing order; every
    /// leaf is a vertex or cluster whose forest parent is `cluster`.
    fn check_local_tree(&self, cluster: u32) {
        let node = &self.nodes[cluster as usize];
        let mut roots = Vec::new();
        assert_ne!(node.left, NONE, "a cluster has children");
        roots.push(node.left);
        let mut current = node.right;
        while current != NONE {
            if self.nodes[current as usize].kind == Kind::Path {
                roots.push(self.nodes[current as usize].left);
                current = self.nodes[current as usize].right;
            } else {
                roots.push(current);
                current = NONE;
            }
        }
        for pair in roots.windows(2) {
            assert!(
                self.nodes[pair[0] as usize].rank > self.nodes[pair[1] as usize].rank,
                "rank path in strictly decreasing rank"
            );
        }
        let mut children = 0;
        let mut stack = roots;
        while let Some(x) = stack.pop() {
            match self.nodes[x as usize].kind {
                Kind::Rank => {
                    stack.push(self.nodes[x as usize].left);
                    stack.push(self.nodes[x as usize].right);
                }
                Kind::Vertex | Kind::Cluster => {
                    assert_eq!(self.forest_parent(x), cluster);
                    children += 1;
                }
                kind => panic!("unexpected {kind:?} inside a rank tree"),
            }
        }
        let level_edge = node.left != NONE
            && self.single_child(cluster).is_some_and(|child| {
                self.nodes[child as usize].bitmap & level_bit(node.level) != 0
            });
        assert!(
            children >= 2 || level_edge,
            "compression: unary only with an own-level edge"
        );
    }

    /// Builds the canonical cluster forest for `edges` with explicit levels
    /// on vertices `0..vertices` (test-only; used to reproduce review
    /// counterexamples exactly). Levels must satisfy the size invariant.
    fn from_levels(vertices: u64, edges: &[(u64, u64, u8)]) -> Self {
        let mut graph = Self::new();
        for node in 0..vertices {
            graph.add_node(node).unwrap();
        }
        let handles: Vec<u32> = (0..vertices).map(|node| graph.ids[&node]).collect();
        for &(x, y, level) in edges {
            assert!(level >= 1 && level <= graph.max_level, "level in range");
            let (a, b) = (handles[x as usize], handles[y as usize]);
            let edge = graph.allocate_edge(a, b, level);
            graph.edge_index.insert(Self::key(a, b), edge);
            graph.list_insert(edge, 0);
            graph.list_insert(edge, 1);
        }
        let mut top = handles.clone();
        for level in 1..=graph.max_level {
            let mut components = Components::new(&handles);
            for &(x, y, l) in edges {
                if l <= level {
                    components.union(handles[x as usize], handles[y as usize]);
                }
            }
            let mut holders: BTreeSet<u32> = BTreeSet::new();
            for &(x, _, l) in edges {
                if l == level {
                    holders.insert(components.find(handles[x as usize]));
                }
            }
            for holder in holders {
                let inside: Vec<usize> = (0..handles.len())
                    .filter(|&i| components.find(handles[i]) == holder)
                    .collect();
                let mut children: Vec<u32> = inside.iter().map(|&i| top[i]).collect();
                children.sort_unstable();
                children.dedup();
                let cluster = graph.new_cluster(level);
                graph.rebuild_with(cluster, &children);
                for i in inside {
                    top[i] = cluster;
                }
            }
        }
        graph.check_invariants();
        graph
    }
}

// ----- oracle and deterministic generator -----

/// Adjacency-set oracle with BFS connectivity.
#[derive(Default)]
struct Oracle {
    adjacency: HashMap<u64, BTreeSet<u64>>,
}

impl Oracle {
    fn add_node(&mut self, node: u64) {
        self.adjacency.entry(node).or_default();
    }
    fn link(&mut self, a: u64, b: u64) -> bool {
        self.add_node(a);
        self.add_node(b);
        let new = self.adjacency.get_mut(&a).unwrap().insert(b);
        self.adjacency.get_mut(&b).unwrap().insert(a);
        new
    }
    fn cut(&mut self, a: u64, b: u64) -> bool {
        let removed = self.adjacency.get_mut(&a).is_some_and(|set| set.remove(&b));
        if removed {
            self.adjacency.get_mut(&b).unwrap().remove(&a);
        }
        removed
    }
    fn components(&self) -> HashMap<u64, u64> {
        let mut label = HashMap::new();
        for &start in self.adjacency.keys() {
            if label.contains_key(&start) {
                continue;
            }
            let mut queue = VecDeque::from([start]);
            label.insert(start, start);
            while let Some(x) = queue.pop_front() {
                for &y in &self.adjacency[&x] {
                    if label.insert(y, start).is_none() {
                        queue.push_back(y);
                    }
                }
            }
        }
        label
    }
}

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

/// Asserts that every pair's answer equals the oracle's.
fn assert_same_answers(graph: &ClusterForestGraph, oracle: &Oracle, context: &str) {
    let labels = oracle.components();
    let nodes: Vec<u64> = labels.keys().copied().collect();
    assert_eq!(graph.node_count(), nodes.len(), "{context}: node count");
    for &a in &nodes {
        for &b in &nodes {
            assert_eq!(
                graph.connected(a, b).unwrap(),
                labels[&a] == labels[&b],
                "{context}: connected({a}, {b})"
            );
        }
    }
}

/// Runs `operations` random links and cuts over `vertices` IDs, checking the
/// full invariants (when `check`) and every pair against the oracle after
/// each operation.
fn random_history(seed: u64, vertices: u64, operations: usize, link_bias: u64, check: bool) {
    let mut rng = Rng(seed);
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    let mut present: Vec<(u64, u64)> = Vec::new();
    for step in 0..operations {
        let roll = rng.below(100);
        if roll < link_bias || present.is_empty() {
            let a = rng.below(vertices);
            let b = rng.below(vertices);
            if a == b {
                assert!(graph.link(a, b).is_err());
                continue;
            }
            let expected = oracle.link(a, b);
            assert_eq!(
                graph.link(a, b).unwrap(),
                expected,
                "seed {seed} step {step}"
            );
            if expected {
                present.push((a, b));
            }
        } else {
            let index = rng.below(present.len() as u64) as usize;
            let (a, b) = present.swap_remove(index);
            assert!(oracle.cut(a, b));
            assert!(graph.cut(a, b).unwrap(), "seed {seed} step {step}");
            assert!(!graph.cut(a, b).unwrap());
        }
        assert_eq!(graph.edge_count(), present.len());
        if check {
            graph.check_invariants();
        }
        assert_same_answers(&graph, &oracle, &format!("seed {seed} step {step}"));
    }
}

/// Applies `cut(a, b)` to a forest built by [`ClusterForestGraph::from_levels`]
/// and its oracle, then checks invariants and answers.
fn cut_and_check(graph: &mut ClusterForestGraph, oracle: &mut Oracle, a: u64, b: u64) {
    assert!(oracle.cut(a, b));
    assert!(graph.cut(a, b).unwrap());
    graph.check_invariants();
    assert_same_answers(graph, oracle, &format!("after cut({a}, {b})"));
}

fn oracle_for(vertices: u64, edges: &[(u64, u64, u8)]) -> Oracle {
    let mut oracle = Oracle::default();
    for node in 0..vertices {
        oracle.add_node(node);
    }
    for &(a, b, _) in edges {
        oracle.link(a, b);
    }
    oracle
}

fn level_of(graph: &ClusterForestGraph, a: u64, b: u64) -> u8 {
    let (x, y) = (graph.ids[&a], graph.ids[&b]);
    graph.edges[graph.edge_index[&ClusterForestGraph::key(x, y)] as usize].level
}

// ----- basic contract -----

#[test]
fn contract_matches_the_other_engines() {
    let mut graph = ClusterForestGraph::new();
    assert_eq!(
        graph.connected(1, 2),
        Err(GraphError::UnknownNode { node: 1 })
    );
    assert_eq!(graph.link(3, 3), Err(GraphError::SelfLoop { node: 3 }));
    assert_eq!(
        graph.node_count(),
        0,
        "a rejected self-loop registers nothing"
    );
    assert!(graph.add_node(u64::MAX).unwrap());
    assert!(!graph.add_node(u64::MAX).unwrap());
    assert!(graph.connected(u64::MAX, u64::MAX).unwrap());
    assert_eq!(
        graph.connected(u64::MAX, 7),
        Err(GraphError::UnknownNode { node: 7 })
    );
    assert!(graph.link(1, u64::MAX).unwrap());
    assert!(!graph.link(u64::MAX, 1).unwrap(), "reversed duplicate");
    assert!(graph.contains_edge(1, u64::MAX));
    assert!(graph.connected(1, u64::MAX).unwrap());
    assert_eq!(graph.cut(2, 2), Err(GraphError::SelfLoop { node: 2 }));
    assert!(!graph.cut(1, 99).unwrap(), "absent endpoint");
    assert!(graph.cut(u64::MAX, 1).unwrap());
    assert!(!graph.cut(1, u64::MAX).unwrap());
    assert!(!graph.connected(1, u64::MAX).unwrap());
    assert_eq!((graph.node_count(), graph.edge_count()), (2, 0));
    graph.check_invariants();
}

// ----- review regressions (ADR 0014, section 12) -----

/// First review, finding 1: the meeting cluster belongs to the side that
/// reached it first. P (level 3, size 8) has children C_u = {u} (1),
/// X = {x1..x4} (level 2, 4) and C_v = {v, y1, y2} (level 2, 3); level-3
/// edges e = (u, v), g = (u, x1), f = (v, x2). Excluding X from both sides
/// would push g and build a level-2 cluster of size 5.
#[test]
fn review1_meeting_cluster_counts_on_the_owner_side() {
    // u=0, v=1, x1..x4=2..5, y1=6, y2=7.
    let edges = [
        (2, 3, 1),
        (4, 5, 1),
        (2, 4, 2),
        (1, 6, 1),
        (6, 7, 2),
        (0, 1, 3),
        (0, 2, 3),
        (1, 3, 3),
    ];
    let mut graph = ClusterForestGraph::from_levels(8, &edges);
    let mut oracle = oracle_for(8, &edges);
    cut_and_check(&mut graph, &mut oracle, 0, 1);
    assert!(graph.connected(0, 1).unwrap());
    assert_eq!(level_of(&graph, 0, 2), 3, "g stays at level 3");
}

/// First review, finding 2: an edge kept at a level above its unary span
/// must be searched when a deletion below splits the span. {v, w} at level
/// 1, e = (u, v) at level 2, f = (u, w) at level 3 (a self-loop of the unary
/// level-3 cluster), (w, z) at level 4; cut(u, v) must keep u connected to w.
#[test]
fn review1_unary_span_edges_keep_their_cluster() {
    // u=0, v=1, w=2, z=3, padding up to 9 vertices for L_max = 4.
    let edges = [(1, 2, 1), (0, 1, 2), (0, 2, 3), (2, 3, 4)];
    let mut graph = ClusterForestGraph::from_levels(9, &edges);
    let mut oracle = oracle_for(9, &edges);
    cut_and_check(&mut graph, &mut oracle, 0, 1);
    assert!(graph.connected(0, 2).unwrap());
    assert!(graph.connected(0, 3).unwrap());
}

/// The same state reached by a push: after a connected-outcome push merges
/// two children while a second edge between them stays at the upper level,
/// later cuts below must still find it.
#[test]
fn review1_unary_span_reached_through_operations() {
    for seed in 0..40 {
        random_history(0xA11CE + seed, 9, 120, 55, true);
    }
}

/// First review, finding 3, and CF2 review, finding 2: searches detach every
/// fetched edge, so an edge internal to a side's reached set (here the level-4
/// triangle on 3, 4, 5 and the level-4 edge (0, 2) inside {0, 1, 2}) is fetched
/// once, classified as internal, and the search terminates.
#[test]
fn review1_self_loops_are_fetched_once() {
    let edges = [
        (0, 1, 1),
        (1, 2, 2),
        (0, 2, 4),
        (3, 4, 4),
        (4, 5, 4),
        (3, 5, 4),
        (3, 0, 4),
    ];
    let mut graph = ClusterForestGraph::from_levels(9, &edges);
    let mut oracle = oracle_for(9, &edges);
    cut_and_check(&mut graph, &mut oracle, 3, 0);
    assert!(
        graph.stats().internal_fetches > 0,
        "the internal-edge branch of a search step ran"
    );
    assert!(!graph.connected(3, 0).unwrap());
}

/// CF2 review, finding 3: a reached set of one cluster whose internal edges
/// are pushed gets a level-(i-1) cluster over it (`wrap`), both when the
/// smaller exhausted side is merged and when the trigger side of a meeting is.
#[test]
fn review_cf2_wrap_creates_the_lower_cluster() {
    let exhausted = [
        (0, 1, 1),
        (1, 2, 2),
        (0, 2, 4),
        (3, 4, 4),
        (4, 5, 4),
        (3, 5, 4),
        (3, 0, 4),
    ];
    let mut graph = ClusterForestGraph::from_levels(9, &exhausted);
    let mut oracle = oracle_for(9, &exhausted);
    cut_and_check(&mut graph, &mut oracle, 3, 0);
    cut_and_check(&mut graph, &mut oracle, 1, 2);
    assert!(graph.stats().wraps > 0, "exhausted side: wrap ran");

    let meeting = [
        (0, 1, 1),
        (1, 2, 2),
        (0, 2, 4),
        (3, 4, 1),
        (5, 6, 1),
        (3, 5, 2),
        (7, 3, 4),
        (0, 7, 4),
        (2, 4, 4),
    ];
    let mut graph = ClusterForestGraph::from_levels(9, &meeting);
    let mut oracle = oracle_for(9, &meeting);
    cut_and_check(&mut graph, &mut oracle, 0, 7);
    assert!(
        graph.stats().wraps > 0,
        "meeting, trigger side merged: wrap ran"
    );
    assert!(graph.connected(0, 7).unwrap());
}

/// The parallel-edge replacement of the first review's fixture: two level-2
/// edges between the same two level-1 clusters.
#[test]
fn review1_replacement_through_a_parallel_edge() {
    // P (level 2) = {a, b, c, d}: {a, b} and {c, d} at level 1, level-2
    // edges (a, c), (b, d) and the self-loop-free cut target; plus a level-2
    // edge internal to nothing else. cut(a, c) must terminate connected.
    let edges = [(0, 1, 1), (2, 3, 1), (0, 2, 2), (1, 3, 2)];
    let mut graph = ClusterForestGraph::from_levels(4, &edges);
    let mut oracle = oracle_for(4, &edges);
    cut_and_check(&mut graph, &mut oracle, 0, 2);
    assert!(graph.connected(0, 3).unwrap());
    cut_and_check(&mut graph, &mut oracle, 1, 3);
    assert!(!graph.connected(0, 3).unwrap());
}

/// First review, finding 4: linking to a root below `L_max` keeps that
/// root's lower-level component as one cluster. R1 = {x, x2} at level 4,
/// R2 = {a, b, c} at level 2; link(x, a), then cut(b, c).
#[test]
fn review1_root_below_top_joins_as_one_child() {
    // x=0, x2=1, a=2, b=3, c=4, padding to 9 vertices for L_max = 4.
    let edges = [(0, 1, 4), (2, 3, 1), (3, 4, 2)];
    let mut graph = ClusterForestGraph::from_levels(9, &edges);
    let mut oracle = oracle_for(9, &edges);
    assert!(graph.link(0, 2).unwrap());
    oracle.link(0, 2);
    graph.check_invariants();
    assert_eq!(level_of(&graph, 0, 2), 4);
    cut_and_check(&mut graph, &mut oracle, 3, 4);
    assert!(!graph.connected(0, 4).unwrap());
    assert!(graph.connected(0, 3).unwrap());
}

/// L_max grows after pushes: roots left below the new top join correctly.
#[test]
fn review1_growth_of_the_top_level_after_pushes() {
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    // A 4-cycle, then cuts that push edges down.
    for &(a, b) in &[(0, 1), (1, 2), (2, 3), (3, 0)] {
        graph.link(a, b).unwrap();
        oracle.link(a, b);
    }
    graph.cut(0, 1).unwrap();
    oracle.cut(0, 1);
    graph.check_invariants();
    // Grow U from 4 to 17 vertices: L_max 2 -> 5.
    for node in 4..17 {
        graph.link(node, node - 1).unwrap();
        oracle.link(node, node - 1);
        graph.check_invariants();
    }
    assert_eq!(graph.max_level, 5);
    for &(a, b) in &[(1, 2), (5, 6), (16, 15)] {
        graph.cut(a, b).unwrap();
        oracle.cut(a, b);
        graph.check_invariants();
        assert_same_answers(&graph, &oracle, "growth");
    }
}

/// First review, finding 6, and second review, finding 1: the exhausted
/// side is the larger, its clusters move unmerged; a later cut whose level
/// is above the LCA splices the cluster that held it. L_max = 3; root P has
/// children S = {s1..s4} (level 2, with level-3 self-loop t1 = (s2, s4)) and
/// a, b, c; level-3 edges (a, b), (a, c), (b, c), e0 = (a, s1).
#[test]
fn review2_early_stop_splices_the_holder() {
    // s1..s4 = 0..3, a=4, b=5, c=6.
    let edges = [
        (0, 1, 1),
        (2, 3, 1),
        (0, 2, 2),
        (1, 3, 3),
        (4, 5, 3),
        (4, 6, 3),
        (5, 6, 3),
        (4, 0, 3),
    ];
    let mut graph = ClusterForestGraph::from_levels(7, &edges);
    let mut oracle = oracle_for(7, &edges);
    cut_and_check(&mut graph, &mut oracle, 4, 0);
    assert!(!graph.connected(4, 0).unwrap());
    assert!(graph.connected(4, 6).unwrap());
    cut_and_check(&mut graph, &mut oracle, 5, 6);
    assert!(graph.connected(5, 6).unwrap());
}

/// Second review, finding 2: the first fetched edge is the replacement, the
/// trigger side reached one cluster and pushed nothing: no cluster changes.
#[test]
fn review2_single_cluster_merge_changes_nothing() {
    let edges = [(0, 1, 2), (0, 2, 2), (1, 2, 2)];
    let mut graph = ClusterForestGraph::from_levels(3, &edges);
    let mut oracle = oracle_for(3, &edges);
    let clusters_before = graph
        .nodes
        .iter()
        .filter(|node| node.kind == Kind::Cluster)
        .count();
    cut_and_check(&mut graph, &mut oracle, 0, 1);
    let clusters_after = graph
        .nodes
        .iter()
        .filter(|node| node.kind == Kind::Cluster)
        .count();
    assert_eq!(clusters_before, clusters_after);
    assert!(graph.connected(0, 1).unwrap());
}

/// Second review, finding 3: merging reached clusters of different levels,
/// with and without an existing cluster at the target level.
#[test]
fn review2_merges_across_levels() {
    // u=0, v=1, A = {a1=2, a2=3} (level 2), b=4, C = {c1=5, c2=6} (level 1),
    // padding vertex 7; level-3 edges e = (u, v), (u, a1), (a2, b), (v, c1).
    let edges = [
        (2, 3, 2),
        (5, 6, 1),
        (0, 1, 3),
        (0, 2, 3),
        (3, 4, 3),
        (1, 5, 3),
    ];
    let mut graph = ClusterForestGraph::from_levels(8, &edges);
    let mut oracle = oracle_for(8, &edges);
    cut_and_check(&mut graph, &mut oracle, 0, 1);
    // The other side: the merge now uses an existing level-2 host.
    let mut graph = ClusterForestGraph::from_levels(8, &edges);
    let mut oracle = oracle_for(8, &edges);
    cut_and_check(&mut graph, &mut oracle, 0, 2);
    cut_and_check(&mut graph, &mut oracle, 3, 4);
}

/// Second review, finding 4: a stored cluster whose children are not
/// connected inside it must fail the checker (check 7, BFS form).
#[test]
#[should_panic(expected = "connected in G_")]
fn review2_checker_rejects_a_disconnected_cluster() {
    // R (level 3) with children c = {a, b} (level 2, no a-b edge at <= 2)
    // and x, edges (a, x), (b, x) at level 3.
    let mut graph = ClusterForestGraph::from_levels(8, &[(0, 2, 3), (1, 2, 3)]);
    let (a, b) = (graph.ids[&0], graph.ids[&1]);
    let root = graph.root(a);
    graph.remove_child(root, a);
    graph.remove_child(root, b);
    let fake = graph.new_cluster(2);
    graph.rebuild_with(fake, &[a, b]);
    graph.add_child(root, fake);
    graph.check_invariants();
}

/// Second review, finding 5: vertices count as level-0 clusters; a missing
/// level-1 cluster must fail the checker.
#[test]
#[should_panic(expected = "whole component of G_1")]
fn review2_checker_rejects_a_missing_level_one_cluster() {
    let mut graph = ClusterForestGraph::new();
    for node in 0..8 {
        graph.add_node(node).unwrap();
    }
    let handles: Vec<u32> = (0..3).map(|node| graph.ids[&node]).collect();
    for &(x, y, level) in &[(0usize, 1usize, 1u8), (0, 2, 3)] {
        let (a, b) = (handles[x], handles[y]);
        let edge = graph.allocate_edge(a, b, level);
        graph.edge_index.insert(ClusterForestGraph::key(a, b), edge);
        graph.list_insert(edge, 0);
        graph.list_insert(edge, 1);
    }
    let cluster = graph.new_cluster(3);
    graph.rebuild_with(cluster, &handles);
    graph.check_invariants();
}

// ----- differential and adversarial histories -----

#[test]
fn random_histories_match_the_oracle_with_full_checks() {
    for seed in 0..60 {
        random_history(seed, 12, 150, 60, true);
    }
    for seed in 0..20 {
        random_history(1_000 + seed, 33, 300, 55, true);
    }
}

/// Small dense histories: they reach unary clusters that keep an edge of
/// their own level, which the compression rule must not splice (first
/// review, finding 2). Splicing them regardless fails several of these seeds.
#[test]
fn compression_keeps_unary_clusters_with_own_level_edges() {
    for (vertices, bias) in [(6u64, 70u64), (8, 65)] {
        for seed in 0..300u64 {
            random_history(seed * 31 + vertices, vertices, 200, bias, true);
        }
    }
}

#[test]
fn dense_histories_match_the_oracle() {
    for seed in 0..10 {
        random_history(2_000 + seed, 16, 600, 75, true);
    }
}

#[test]
fn longer_histories_match_the_oracle() {
    for seed in 0..4 {
        random_history(3_000 + seed, 64, 2_000, 52, false);
    }
}

/// Insert a dense graph, then delete every edge in random order: deep
/// replacement searches and pushes to level 1.
#[test]
fn insert_all_then_delete_all() {
    let mut rng = Rng(77);
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    let mut edges = Vec::new();
    for a in 0..24u64 {
        for b in a + 1..24 {
            if rng.below(3) == 0 {
                edges.push((a, b));
            }
        }
    }
    for &(a, b) in &edges {
        graph.link(a, b).unwrap();
        oracle.link(a, b);
    }
    graph.check_invariants();
    let mut lowest = u8::MAX;
    while !edges.is_empty() {
        let (a, b) = edges.swap_remove(rng.below(edges.len() as u64) as usize);
        cut_and_check(&mut graph, &mut oracle, a, b);
        for &edge in graph.edge_index.values() {
            lowest = lowest.min(graph.edges[edge as usize].level);
        }
    }
    assert_eq!(lowest, 1, "some edge was pushed to the lowest level");
    assert!(graph.stats().pushes > 0, "deletions pushed edges down");
    assert!(graph.stats().splits > 0);
    assert!(graph.nodes.iter().all(|node| node.kind != Kind::Cluster));
}

/// CF3 churn: repeated build-and-destroy cycles reuse freed node and edge
/// slots, so the arenas stop growing after the first cycle, and the graph
/// returns to isolated vertices with no stored cluster.
#[test]
fn churn_reuses_arena_slots() {
    let mut rng = Rng(99);
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    let mut peak_edges = 0usize;
    for cycle in 0..8 {
        let mut edges = Vec::new();
        for _ in 0..200 {
            let (a, b) = (rng.below(40), rng.below(40));
            if a != b && graph.link(a, b).unwrap() {
                oracle.link(a, b);
                edges.push((a, b));
                peak_edges = peak_edges.max(graph.edge_count());
            }
        }
        graph.check_invariants();
        while let Some((a, b)) = edges.pop() {
            assert!(graph.cut(a, b).unwrap());
            oracle.cut(a, b);
        }
        graph.check_invariants();
        assert_same_answers(&graph, &oracle, &format!("cycle {cycle}"));
        assert!(graph.nodes.iter().all(|node| node.kind != Kind::Cluster));
        assert_eq!(
            graph.edges.len(),
            peak_edges,
            "edge records never exceed the peak of live edges"
        );
        let vertices = graph.node_count();
        assert!(
            graph.nodes.len() <= 3 * (2 * vertices + peak_edges),
            "node arena within the documented bound"
        );
    }
}

/// Allocated capacity after mass deletions (plain engine, ADR 0014 section 7:
/// documented as retained, not shrunk). Without asserting allocator details
/// (exact capacities or growth factors), it checks that: deleting every edge
/// returns the logical structure to isolated vertices with every edge record
/// and cluster slot on a free list; the arenas never exceed the peak of live
/// elements (edges) or the documented node bound; and refilling the same graph
/// reuses the freed slots instead of growing the arenas.
#[test]
fn allocated_capacity_after_mass_deletions() {
    let mut rng = Rng(4242);
    let vertices = 200u64;
    let mut graph = ClusterForestGraph::new();
    let mut edges = Vec::new();
    while edges.len() < 800 {
        let (a, b) = (rng.below(vertices), rng.below(vertices));
        if a != b && graph.link(a, b).unwrap() {
            edges.push((a, b));
        }
    }
    let peak_edges = graph.edge_count();
    let node_slots = graph.nodes.len();
    let edge_capacity = graph.edges.capacity();
    let node_capacity = graph.nodes.capacity();
    assert!(node_slots <= 3 * (2 * vertices as usize + peak_edges));
    // Mass deletion in random order.
    let mut order = edges.clone();
    while !order.is_empty() {
        let (a, b) = order.swap_remove(rng.below(order.len() as u64) as usize);
        assert!(graph.cut(a, b).unwrap());
    }
    graph.check_invariants();
    assert_eq!(graph.edge_count(), 0);
    assert_eq!(
        graph.free_edges.len(),
        graph.edges.len(),
        "every edge record free"
    );
    let live = graph
        .nodes
        .iter()
        .filter(|node| node.kind != Kind::Free)
        .count();
    assert_eq!(live, vertices as usize, "only vertices remain");
    assert!(
        graph.edges.len() <= peak_edges,
        "edge arena within the peak"
    );
    assert!(
        graph.vertex_lists.iter().all(Vec::is_empty),
        "no level entry survives"
    );
    // Refill: freed slots are reused, the arenas do not grow.
    for &(a, b) in &edges {
        assert!(graph.link(a, b).unwrap());
    }
    graph.check_invariants();
    assert_eq!(graph.edges.len(), peak_edges, "edge records reused");
    assert_eq!(
        graph.edges.capacity(),
        edge_capacity,
        "no edge reallocation"
    );
    assert!(
        graph.nodes.len() <= 3 * (2 * vertices as usize + peak_edges),
        "node arena within the documented bound"
    );
    if graph.nodes.len() <= node_capacity {
        assert_eq!(
            graph.nodes.capacity(),
            node_capacity,
            "no node reallocation"
        );
    }
}

/// Long CF3 variant with the full checker after every operation; run by the
/// `cluster-forest-long` workflow.
#[test]
#[ignore = "long CF3 variant; run by the cluster-forest-long workflow"]
fn long_histories_with_full_checks() {
    for seed in 0..2_000 {
        random_history(50_000 + seed, 6 + seed % 20, 300, 50 + seed % 30, true);
    }
    for seed in 0..20 {
        random_history(60_000 + seed, 128, 3_000, 55, false);
    }
}

/// Ring breaking and repeated split/merge of the same components.
#[test]
fn rings_split_and_merge_repeatedly() {
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    let n = 20u64;
    for node in 0..n {
        graph.link(node, (node + 1) % n).unwrap();
        oracle.link(node, (node + 1) % n);
    }
    for round in 0..30u64 {
        let a = round % n;
        let b = (round * 7 + 3) % n;
        cut_and_check(&mut graph, &mut oracle, a, (a + 1) % n);
        if a != b && (b + 1) % n != a {
            cut_and_check(&mut graph, &mut oracle, b, (b + 1) % n);
            graph.link(b, (b + 1) % n).unwrap();
            oracle.link(b, (b + 1) % n);
            graph.check_invariants();
        }
        graph.link(a, (a + 1) % n).unwrap();
        oracle.link(a, (a + 1) % n);
        graph.check_invariants();
        assert_same_answers(&graph, &oracle, "ring");
    }
}

/// Hub and star deletions.
#[test]
fn hubs_and_stars() {
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    for leaf in 1..=30u64 {
        graph.link(0, leaf).unwrap();
        oracle.link(0, leaf);
        if leaf % 3 == 0 {
            graph.link(leaf, leaf - 1).unwrap();
            oracle.link(leaf, leaf - 1);
        }
    }
    graph.check_invariants();
    for leaf in (1..=30u64).rev() {
        cut_and_check(&mut graph, &mut oracle, 0, leaf);
    }
}

/// Search marks survive the epoch wrap.
#[test]
fn epoch_wrap_resets_marks() {
    let mut graph = ClusterForestGraph::new();
    let mut oracle = Oracle::default();
    for &(a, b) in &[(0, 1), (1, 2), (2, 0), (2, 3)] {
        graph.link(a, b).unwrap();
        oracle.link(a, b);
    }
    graph.epoch = (1 << 31) - 1;
    cut_and_check(&mut graph, &mut oracle, 0, 1);
    assert!(graph.epoch < 8, "the epoch restarted after the wrap");
    cut_and_check(&mut graph, &mut oracle, 2, 3);
}

// ----- element limit (CF2 review, finding 1 and the user's request) -----

use super::with_max_elements;
use crate::{ConnectivityGraph, EngineConfig, GraphLimits, LifecycleGraph};

/// Snapshot of everything a rejected mutation must leave unchanged.
fn fingerprint(graph: &ClusterForestGraph) -> (usize, usize, usize, usize, Vec<bool>) {
    let answers = (0..12u64)
        .flat_map(|a| (0..12u64).map(move |b| (a, b)))
        .map(|(a, b)| graph.connected(a, b).unwrap_or(false))
        .collect();
    (
        graph.node_count(),
        graph.edge_count(),
        graph.nodes.len(),
        graph.edges.len(),
        answers,
    )
}

/// At the backend limit every rejected `add_node` and `link` leaves the
/// graph unchanged, including links with one or two missing endpoints, and
/// duplicates still succeed as no-ops.
#[test]
fn backend_limit_rejections_are_atomic() {
    with_max_elements(8, || {
        let mut graph = ClusterForestGraph::new();
        for node in 0..7 {
            graph.add_node(node).unwrap();
        }
        // Node limit: two missing endpoints with room for one.
        let before = fingerprint(&graph);
        assert_eq!(
            graph.link(100, 101),
            Err(GraphError::NodeLimitExceeded { limit: 8 })
        );
        assert_eq!(fingerprint(&graph), before);
        assert!(!graph.contains_node(100) && !graph.contains_node(101));
        // One missing endpoint fits.
        assert_eq!(graph.link(0, 7), Ok(true));
        let before = fingerprint(&graph);
        assert_eq!(
            graph.add_node(8),
            Err(GraphError::NodeLimitExceeded { limit: 8 })
        );
        assert_eq!(
            graph.link(0, 8),
            Err(GraphError::NodeLimitExceeded { limit: 8 })
        );
        assert_eq!(fingerprint(&graph), before);
        assert!(!graph.contains_node(8));
        // Fill the edge limit, then reject a new edge; duplicates still pass.
        for (a, b) in [(1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 0), (1, 5)] {
            assert_eq!(graph.link(a, b), Ok(true));
        }
        assert_eq!(graph.edge_count(), 8);
        let before = fingerprint(&graph);
        assert_eq!(
            graph.link(2, 6),
            Err(GraphError::EdgeLimitExceeded { limit: 8 })
        );
        assert_eq!(graph.link(5, 1), Ok(false), "duplicate at the limit");
        assert_eq!(graph.add_node(3), Ok(false));
        assert_eq!(fingerprint(&graph), before);
        graph.check_invariants();
        // Room again after a cut.
        assert_eq!(graph.cut(1, 5), Ok(true));
        assert_eq!(graph.link(2, 6), Ok(true));
        graph.check_invariants();
    });
}

/// The edge limit is checked before registering a missing endpoint.
#[test]
fn backend_edge_limit_registers_no_endpoint() {
    with_max_elements(4, || {
        let mut graph = ClusterForestGraph::new();
        for (a, b) in [(0, 1), (1, 2), (2, 0), (0, 3)] {
            graph.link(a, b).unwrap();
        }
        // Node limit reached too, so use an existing pair for the edge case.
        assert_eq!(
            graph.link(1, 3),
            Err(GraphError::EdgeLimitExceeded { limit: 4 })
        );
        graph.cut(0, 3).unwrap();
        graph.check_invariants();
    });
    with_max_elements(6, || {
        let mut graph = ClusterForestGraph::new();
        for (a, b) in [(0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3)] {
            graph.link(a, b).unwrap();
        }
        let before = fingerprint(&graph);
        assert_eq!(
            graph.link(0, 50),
            Err(GraphError::EdgeLimitExceeded { limit: 6 })
        );
        assert!(!graph.contains_node(50), "no endpoint registered");
        assert_eq!(fingerprint(&graph), before);
    });
}

/// Through `ConnectivityGraph`: the configured limit applies when it is
/// lower than the backend's, and the backend's limit (with its own value)
/// when the configured one is higher or absent.
#[test]
fn connectivity_graph_reports_the_lower_limit() {
    with_max_elements(8, || {
        let mut higher = ConnectivityGraph::with_limits(
            EngineConfig::ClusterForest,
            GraphLimits {
                max_nodes: Some(100),
                max_edges: None,
            },
        );
        for node in 0..8 {
            higher.add_node(node).unwrap();
        }
        assert_eq!(
            higher.add_node(8),
            Err(GraphError::NodeLimitExceeded { limit: 8 })
        );
        assert_eq!(higher.node_count(), 8);
        let mut lower = ConnectivityGraph::with_limits(
            EngineConfig::ClusterForest,
            GraphLimits {
                max_nodes: Some(5),
                max_edges: Some(3),
            },
        );
        for (a, b) in [(0, 1), (1, 2), (2, 3)] {
            lower.link(a, b).unwrap();
        }
        assert_eq!(
            lower.link(3, 4),
            Err(GraphError::EdgeLimitExceeded { limit: 3 })
        );
        assert_eq!(lower.node_count(), 4, "rejected link registered nothing");
        let mut unlimited = ConnectivityGraph::new(EngineConfig::ClusterForest);
        for node in 0..8 {
            unlimited.add_node(node).unwrap();
        }
        assert_eq!(
            unlimited.link(8, 9),
            Err(GraphError::NodeLimitExceeded { limit: 8 })
        );
        assert_eq!(unlimited.node_count(), 8);
    });
}

/// CF2 review, finding 1: `LifecycleGraph` enforces `(capacity − 1024) / 2`
/// nodes and `capacity` edges for `cluster-forest`, so retired slots never
/// push the engine past its capacity: churn near the limit returns errors and
/// compacts, never panics, and `limits()` still reports the configured value.
#[test]
fn lifecycle_stays_within_the_engine_capacity() {
    with_max_elements(4096, || {
        let node_cap = (4096 - 1024) / 2;
        let mut graph = LifecycleGraph::new(EngineConfig::ClusterForest).unwrap();
        assert_eq!(graph.limits(), GraphLimits::default());
        let mut next = 0u64;
        let mut live = Vec::new();
        while graph.node_count() < node_cap {
            graph.add_node(next).unwrap();
            if let Some(&previous) = live.last() {
                graph.link(previous, next).unwrap();
            }
            live.push(next);
            next += 1;
        }
        let error = graph.add_node(next).unwrap_err();
        assert_eq!(error, GraphError::NodeLimitExceeded { limit: node_cap });
        assert_eq!(
            error.to_string(),
            format!("node limit exceeded: {node_cap}"),
            "the message names the enforced limit, not the configured one"
        );
        assert_eq!(
            graph.link(next, next + 1),
            Err(GraphError::NodeLimitExceeded { limit: node_cap })
        );
        assert_eq!(graph.node_count(), node_cap);
        let mut rng = Rng(5);
        for _ in 0..6 {
            // Remove most nodes (retiring engine slots), then refill to the
            // limit with fresh IDs.
            for _ in 0..node_cap - 100 {
                let node = live.swap_remove(rng.below(live.len() as u64) as usize);
                assert!(graph.remove_node(node));
                assert!(graph.engine_vertex_count() <= 4096);
            }
            while graph.node_count() < node_cap {
                graph.add_node(next).unwrap();
                let other = live[rng.below(live.len() as u64) as usize];
                graph.link(other, next).unwrap();
                live.push(next);
                next += 1;
                assert!(graph.engine_vertex_count() <= 4096);
            }
            assert_eq!(
                graph.add_node(next),
                Err(GraphError::NodeLimitExceeded { limit: node_cap })
            );
        }
        // Retired slots near their maximum (delta review, finding 6): at the
        // node limit, remove a few nodes and refill with fresh IDs, many
        // times. Retired slots then climb to about the catalog size before
        // each compaction, so engine vertices approach `2 × node_cap`; they
        // must stay within the capacity, and the climb must really happen
        // (a looser compaction policy would overflow the capacity here).
        let mut peak_vertices = 0;
        for _ in 0..600 {
            for _ in 0..8 {
                let node = live.swap_remove(rng.below(live.len() as u64) as usize);
                assert!(graph.remove_node(node));
            }
            for _ in 0..8 {
                graph.add_node(next).unwrap();
                let other = live[rng.below(live.len() as u64) as usize];
                graph.link(other, next).unwrap();
                live.push(next);
                next += 1;
            }
            peak_vertices = peak_vertices.max(graph.engine_vertex_count());
            assert!(graph.engine_vertex_count() <= 4096);
        }
        assert!(
            peak_vertices >= 2 * node_cap - 64,
            "retired slots reached their bound: peak {peak_vertices} of {}",
            2 * node_cap
        );

        // Edge limit: registered edges cap at the capacity.
        let mut edges = graph.edge_count();
        'fill: for &a in &live {
            for &b in &live {
                if edges >= 4096 {
                    break 'fill;
                }
                if a < b && graph.link(a, b) == Ok(true) {
                    edges += 1;
                }
            }
        }
        assert_eq!(graph.edge_count(), 4096);
        let (a, b) = (live[0], live[live.len() - 1]);
        if graph.edge_state(a, b).is_none() {
            assert_eq!(
                graph.link(a, b),
                Err(GraphError::EdgeLimitExceeded { limit: 4096 })
            );
        }
        // An import above the enforced node limit is rejected, not a panic.
        let nodes: Vec<crate::NodeSpec> = (0..node_cap as u64 + 1)
            .map(|node| crate::NodeSpec { node, active: true })
            .collect();
        let error = LifecycleGraph::try_from_topology(
            EngineConfig::ClusterForest,
            GraphLimits::default(),
            &nodes,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            error,
            crate::LifecycleError::Graph(GraphError::NodeLimitExceeded { limit: node_cap })
        );
        // A configured limit below the capacity is enforced and reported.
        let limits = GraphLimits {
            max_nodes: Some(10),
            max_edges: Some(5),
        };
        let mut small = LifecycleGraph::with_limits(EngineConfig::ClusterForest, limits).unwrap();
        assert_eq!(small.limits(), limits);
        for node in 0..10 {
            small.add_node(node).unwrap();
        }
        assert_eq!(
            small.add_node(10),
            Err(GraphError::NodeLimitExceeded { limit: 10 })
        );
    });
}
