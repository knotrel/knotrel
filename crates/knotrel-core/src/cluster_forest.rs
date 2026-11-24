//! Experimental cluster forest connectivity backend (crate-private).
//!
//! Design: [ADR 0014](https://github.com/knotrel/knotrel/blob/main/docs/architecture/0014-cluster-forest-backend.md).
//! The structure is the basic sequential cluster forest of Wulff-Nilsen,
//! *Faster Deterministic Fully-Dynamic Graph Connectivity* (SODA 2013), §3,
//! with the path compression of De Man et al., *Towards Scalable and
//! Practical Batch-Dynamic Connectivity* (PVLDB 18(3), 2025), §4, and
//! insertion at the lowest common cluster (CF-LCA, same paper §8.1). Levels
//! use the leaf-up orientation of De Man et al.
//!
//! # Definitions and invariants
//!
//! - Every edge has a level `l(e)` in `1..=L_max`, where
//!   `L_max = max(1, ⌈log₂ U⌉)` and `U` is the number of registered
//!   vertices (it only grows: the plain engine never removes vertices).
//!   `G_i` is the graph of edges of level `≤ i`.
//! - **Size invariant** (De Man et al., Invariant 2.1): every component of
//!   `G_i` has at most `2^i` vertices.
//! - A level-i cluster is a component of `G_i`; vertices are the level-0
//!   clusters. The parent of a cluster is the next stored cluster containing
//!   it; roots are the components of the graph.
//! - **Compression** (De Man et al., §4): a cluster of level `j ≥ 1` is stored
//!   iff some level-j edge has both endpoints in it. A stored cluster spans
//!   the levels from its own up to its parent's level minus one, and no edge
//!   touching it has a level strictly inside that span.
//! - **Local trees** (Wulff-Nilsen, §3.4): the children of a cluster are the
//!   leaves of binary *rank trees* (a leaf of size `s` has rank `⌊log₂ s⌋`; an
//!   internal node of rank `r` has two children of rank `r - 1`), with
//!   pairwise distinct root ranks, hung in decreasing rank from a *rank path*
//!   that starts at the cluster node itself. A child of size `s` of a cluster
//!   of size `S` lies at depth at most `log₂(S/s) + 1` (Thorup, cited by
//!   Wulff-Nilsen §3.4), so a vertex-to-root walk telescopes to `O(log n)`
//!   steps. Every node aggregates the number of vertices below it and a
//!   bitmap whose bit `i - 1` says that a vertex below has a level-i edge.
//!
//! # Complexity
//!
//! With `N` the high-water vertex count: `connected` is `O(log N)` worst case
//! plus expected `O(1)` ID lookups. `link` is `O(log N)` actual. Updates are
//! `O(log² N)` amortized in aggregate. That bound is **this project's
//! derivation** for the compressed CF-LCA variant (ADR 0014, section 6),
//! reviewed as a proof sketch; Wulff-Nilsen states it for the uncompressed
//! structure and De Man et al. prove it only for their blocked variant. Vector
//! growth is amortized `O(1)` and can cause latency spikes. Space is
//! `O(V + E)` words.

use crate::{GraphError, NodeId};
use nohash_hasher::IntMap;
use rustc_hash::FxHashMap;

/// Absent handle.
const NONE: u32 = u32::MAX;

/// Most vertices and most edges this backend accepts.
///
/// Node handles are `u32`. Stored clusters are at most `(V - 1) + E` (each
/// unary cluster holds an edge of its level) and local-tree nodes fewer than
/// twice the children, so all nodes stay below `3 · (2V + E)`, which fits in
/// `u32` with both limits at `2^28`.
pub(crate) const MAX_ELEMENTS: usize = 1 << 28;

#[cfg(test)]
thread_local! {
    /// Test-only override of [`MAX_ELEMENTS`] for the current thread, so the
    /// limit paths run with small graphs (`0` means no override).
    static MAX_ELEMENTS_OVERRIDE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The element limit in force: [`MAX_ELEMENTS`], or the current thread's
/// test override.
pub(crate) fn max_elements() -> usize {
    #[cfg(test)]
    {
        let limit = MAX_ELEMENTS_OVERRIDE.with(std::cell::Cell::get);
        if limit != 0 {
            return limit;
        }
    }
    MAX_ELEMENTS
}

/// Runs `body` with the element limit lowered to `limit` on this thread
/// (test-only).
#[cfg(test)]
pub(crate) fn with_max_elements<T>(limit: usize, body: impl FnOnce() -> T) -> T {
    assert!(limit > 0 && limit <= MAX_ELEMENTS);
    let previous = MAX_ELEMENTS_OVERRIDE.with(|cell| cell.replace(limit));
    let result = body();
    MAX_ELEMENTS_OVERRIDE.with(|cell| cell.set(previous));
    result
}

/// Slots of the binary counter used to pair rank trees; ranks stay below
/// 30 under [`MAX_ELEMENTS`].
const RANK_SLOTS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A graph vertex: level 0, a leaf of the forest.
    Vertex,
    /// A stored cluster of level `level ≥ 1`; also the top of its own rank
    /// path.
    Cluster,
    /// An internal node of a rank tree.
    Rank,
    /// A rank-path node below the cluster node.
    Path,
    /// An arena slot on the free list.
    Free,
}

/// One node of the combined forest: vertices, clusters and local-tree nodes.
#[derive(Debug, Clone)]
struct Node {
    parent: u32,
    left: u32,
    right: u32,
    /// Vertices below this node.
    size: u32,
    /// Bit `i - 1` set iff a vertex below has an incident level-i edge.
    bitmap: u64,
    kind: Kind,
    /// Cluster level (0 for vertices, unused for local-tree nodes).
    level: u8,
    /// Rank in the enclosing rank tree (leaves and `Rank` nodes).
    rank: u8,
    /// Search mark: `epoch << 1 | side`.
    mark: u32,
    /// For vertices, the index of their level lists in `vertex_lists`.
    aux: u32,
}

impl Node {
    fn new(kind: Kind) -> Self {
        Self {
            parent: NONE,
            left: NONE,
            right: NONE,
            size: 0,
            bitmap: 0,
            kind,
            level: 0,
            rank: 0,
            mark: 0,
            aux: NONE,
        }
    }
}

/// The edges of one vertex at one level.
#[derive(Debug, Clone, Default)]
struct LevelList {
    level: u8,
    edges: Vec<u32>,
}

/// One edge: endpoints (vertex node handles), level, and its index in each
/// endpoint's level list.
#[derive(Debug, Clone, Copy)]
struct EdgeRecord {
    ends: [u32; 2],
    level: u8,
    position: [u32; 2],
}

/// One side of a deletion search.
#[derive(Debug, Default)]
struct Side {
    /// Reached children of the searched cluster, in discovery order.
    queue: Vec<u32>,
    /// First queue entry that may still have an unexplored edge.
    cursor: usize,
    /// Total vertices of the reached children.
    size: u64,
    /// Edges fetched by this side, detached from their level lists.
    detached: Vec<u32>,
}

impl Side {
    fn reset(&mut self, start: u32, size: u32) {
        self.queue.clear();
        self.queue.push(start);
        self.cursor = 0;
        self.size = u64::from(size);
        self.detached.clear();
    }
}

/// Result of one search step.
enum Step {
    Progress,
    Exhausted,
    Meeting { cluster: u32, edge: u32 },
}

/// Result of a search in one cluster graph.
enum SearchOutcome {
    /// The two start clusters are still connected at this level.
    Connected,
    /// Side `exhausted` reached a whole component; `piece` is that component
    /// as one node (merged or wrapped) when `merged`, otherwise its clusters
    /// are `sides[exhausted].queue`.
    Split {
        exhausted: usize,
        piece: Option<u32>,
    },
}

/// Diagnostic counters (crate-private; used by tests).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClusterForestStats {
    /// Edges moved one level down.
    pub(crate) pushes: u64,
    /// Clusters split by a deletion.
    pub(crate) splits: u64,
    /// Edges fetched by deletion searches.
    pub(crate) fetched_edges: u64,
    /// Cluster graphs searched.
    pub(crate) levels_visited: u64,
    /// Fetched edges internal to the fetching side's reached set
    /// (cluster-graph self-loops and cycles).
    pub(crate) internal_fetches: u64,
    /// Level-(i-1) clusters created over a single reached cluster whose
    /// internal edges were pushed (`wrap`).
    pub(crate) wraps: u64,
}

/// Exact dynamic connectivity with a cluster forest (crate-private backend of
/// [`crate::EngineConfig::ClusterForest`]).
#[derive(Debug, Default)]
pub(crate) struct ClusterForestGraph {
    ids: IntMap<NodeId, u32>,
    nodes: Vec<Node>,
    free_nodes: Vec<u32>,
    vertex_lists: Vec<Vec<LevelList>>,
    edges: Vec<EdgeRecord>,
    free_edges: Vec<u32>,
    edge_index: FxHashMap<(u32, u32), u32>,
    max_level: u8,
    epoch: u32,
    sides: [Side; 2],
    chains: [Vec<u32>; 2],
    trees: Vec<u32>,
    path: Vec<u32>,
    moved: Vec<u32>,
    stats: ClusterForestStats,
}

/// `⌊log₂ value⌋`; panics on zero in every build (a zero size would mean a
/// corrupt forest).
fn floor_log2(value: u32) -> u8 {
    value.ilog2() as u8
}

fn level_bit(level: u8) -> u64 {
    debug_assert!((1..=64).contains(&level));
    1u64 << (level - 1)
}

impl ClusterForestGraph {
    /// Creates an empty graph in O(1).
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether `node` is registered; expected O(1).
    pub(crate) fn contains_node(&self, node: NodeId) -> bool {
        self.ids.contains_key(&node)
    }

    /// Whether the edge `{source, target}` exists; expected O(1).
    pub(crate) fn contains_edge(&self, source: NodeId, target: NodeId) -> bool {
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return false;
        };
        self.edge_index.contains_key(&Self::key(a, b))
    }

    /// Registered vertices, O(1).
    pub(crate) fn node_count(&self) -> usize {
        self.ids.len()
    }

    /// Live edges, O(1).
    pub(crate) fn edge_count(&self) -> usize {
        self.edge_index.len()
    }

    /// Diagnostic counters since creation.
    #[cfg(test)]
    pub(crate) fn stats(&self) -> ClusterForestStats {
        self.stats
    }

    /// Registers an isolated vertex; `Ok(false)` if already present.
    ///
    /// # Errors
    ///
    /// [`GraphError::NodeLimitExceeded`] beyond [`max_elements`] vertices.
    pub(crate) fn add_node(&mut self, node: NodeId) -> Result<bool, GraphError> {
        if self.ids.contains_key(&node) {
            return Ok(false);
        }
        let limit = max_elements();
        if self.ids.len() >= limit {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        self.register(node);
        Ok(true)
    }

    /// Adds the edge `{source, target}`, registering absent endpoints.
    /// Returns whether the edge was absent.
    ///
    /// # Errors
    ///
    /// [`GraphError::SelfLoop`] for `source == target`;
    /// [`GraphError::NodeLimitExceeded`] or [`GraphError::EdgeLimitExceeded`]
    /// beyond [`max_elements`]. Errors leave the graph unchanged.
    pub(crate) fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let limit = max_elements();
        let missing =
            usize::from(!self.contains_node(source)) + usize::from(!self.contains_node(target));
        if missing > limit.saturating_sub(self.ids.len()) {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        if missing == 0 && self.contains_edge(source, target) {
            return Ok(false);
        }
        if self.edge_index.len() >= limit {
            return Err(GraphError::EdgeLimitExceeded { limit });
        }
        let a = self.vertex_or_register(source);
        let b = self.vertex_or_register(target);

        self.fill_chain(0, a);
        self.fill_chain(1, b);
        let root_a = *self.chains[0].last().expect("a chain holds its vertex");
        let root_b = *self.chains[1].last().expect("a chain holds its vertex");
        let level = if root_a == root_b {
            // CF-LCA: both endpoints already share the cluster of their lowest
            // stored common ancestor, so no component of any G_j changes.
            let common = self.lowest_common();
            debug_assert_eq!(self.nodes[common as usize].kind, Kind::Cluster);
            self.nodes[common as usize].level
        } else {
            self.join_roots(root_a, root_b);
            self.max_level
        };

        let edge = self.allocate_edge(a, b, level);
        self.edge_index.insert(Self::key(a, b), edge);
        self.attach_edge(edge);
        self.check_quick();
        Ok(true)
    }

    /// Removes the edge `{source, target}`, keeping both vertices. Returns
    /// whether it existed.
    ///
    /// # Errors
    ///
    /// [`GraphError::SelfLoop`] for `source == target`, without change.
    pub(crate) fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let (Some(&a), Some(&b)) = (self.ids.get(&source), self.ids.get(&target)) else {
            return Ok(false);
        };
        let Some(edge) = self.edge_index.remove(&Self::key(a, b)) else {
            return Ok(false);
        };
        let level = self.edges[edge as usize].level;
        self.detach_edge(edge);
        self.free_edges.push(edge);

        self.fill_chain(0, a);
        self.fill_chain(1, b);
        debug_assert_eq!(self.chains[0].last(), self.chains[1].last());
        let common = self.lowest_common();
        let common_level = self.nodes[common as usize].level;
        debug_assert!(common_level <= level, "an edge never sits below its LCA");
        if level > common_level {
            // ADR 0014, 5.4 step 1: a and b stay connected through edges of
            // level <= common_level < level. The stored cluster that held the
            // edge as one of its own-level edges may now hold none.
            let holder = self.chains[0]
                .iter()
                .rev()
                .copied()
                .find(|&x| self.nodes[x as usize].level <= level)
                .expect("the common ancestor qualifies");
            self.splice_if_redundant(holder);
            self.check_quick();
            return Ok(true);
        }
        let start_a = self.child_on_chain(0, common);
        let start_b = self.child_on_chain(1, common);
        debug_assert_ne!(start_a, start_b, "the LCA separates the endpoints");
        self.repair(common, start_a, start_b);
        self.check_quick();
        Ok(true)
    }

    /// Exact connectivity: both vertices walk to their roots, O(log N).
    ///
    /// # Errors
    ///
    /// [`GraphError::UnknownNode`] for `source`, then `target`, if absent.
    pub(crate) fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        let a = *self
            .ids
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let b = *self
            .ids
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        Ok(a == b || self.root(a) == self.root(b))
    }

    // ----- registration and arenas -----

    fn key(a: u32, b: u32) -> (u32, u32) {
        (a.min(b), a.max(b))
    }

    fn vertex_or_register(&mut self, node: NodeId) -> u32 {
        match self.ids.get(&node) {
            Some(&vertex) => vertex,
            None => self.register(node),
        }
    }

    fn register(&mut self, node: NodeId) -> u32 {
        let mut vertex = Node::new(Kind::Vertex);
        vertex.size = 1;
        vertex.aux = u32::try_from(self.vertex_lists.len()).expect("bounded by MAX_ELEMENTS");
        self.vertex_lists.push(Vec::new());
        let handle = self.allocate(vertex);
        self.ids.insert(node, handle);
        let count = self.ids.len();
        let ceil_log = if count <= 1 {
            0
        } else {
            (usize::BITS - (count - 1).leading_zeros()) as u8
        };
        self.max_level = ceil_log.max(1);
        handle
    }

    fn allocate(&mut self, node: Node) -> u32 {
        if let Some(handle) = self.free_nodes.pop() {
            self.nodes[handle as usize] = node;
            handle
        } else {
            let handle = u32::try_from(self.nodes.len()).expect("bounded by MAX_ELEMENTS");
            assert_ne!(handle, NONE, "node arena exhausted");
            self.nodes.push(node);
            handle
        }
    }

    fn release(&mut self, handle: u32) {
        let node = &mut self.nodes[handle as usize];
        debug_assert!(matches!(node.kind, Kind::Cluster | Kind::Rank | Kind::Path));
        *node = Node::new(Kind::Free);
        self.free_nodes.push(handle);
    }

    fn new_cluster(&mut self, level: u8) -> u32 {
        let mut cluster = Node::new(Kind::Cluster);
        cluster.level = level;
        self.allocate(cluster)
    }

    fn allocate_edge(&mut self, a: u32, b: u32, level: u8) -> u32 {
        let record = EdgeRecord {
            ends: [a, b],
            level,
            position: [0, 0],
        };
        if let Some(edge) = self.free_edges.pop() {
            self.edges[edge as usize] = record;
            edge
        } else {
            let edge = u32::try_from(self.edges.len()).expect("bounded by MAX_ELEMENTS");
            self.edges.push(record);
            edge
        }
    }

    // ----- level lists -----

    /// Index of `level`'s list among a vertex's occupied levels.
    fn list_index(bitmap: u64, level: u8) -> usize {
        (bitmap & (level_bit(level) - 1)).count_ones() as usize
    }

    /// Inserts `edge` into endpoint `end`'s list at the edge's level.
    /// Amortized O(1) (vector growth) plus O(64) for a new level entry.
    fn list_insert(&mut self, edge: u32, end: usize) {
        let EdgeRecord { ends, level, .. } = self.edges[edge as usize];
        let vertex = ends[end] as usize;
        let bit = level_bit(level);
        let bitmap = self.nodes[vertex].bitmap;
        let index = Self::list_index(bitmap, level);
        let lists = &mut self.vertex_lists[self.nodes[vertex].aux as usize];
        if bitmap & bit == 0 {
            lists.insert(
                index,
                LevelList {
                    level,
                    edges: Vec::new(),
                },
            );
            self.nodes[vertex].bitmap |= bit;
        }
        debug_assert_eq!(lists[index].level, level);
        let list = &mut lists[index].edges;
        self.edges[edge as usize].position[end] = list.len() as u32;
        list.push(edge);
    }

    /// Removes `edge` from endpoint `end`'s list; O(1) worst case plus O(64)
    /// when the level entry becomes empty.
    fn list_remove(&mut self, edge: u32, end: usize) {
        let EdgeRecord {
            ends,
            level,
            position,
        } = self.edges[edge as usize];
        let vertex = ends[end];
        let bitmap = self.nodes[vertex as usize].bitmap;
        let index = Self::list_index(bitmap, level);
        let lists = &mut self.vertex_lists[self.nodes[vertex as usize].aux as usize];
        let list = &mut lists[index].edges;
        let at = position[end] as usize;
        debug_assert_eq!(list[at], edge);
        list.swap_remove(at);
        if let Some(&moved) = list.get(at) {
            let record = &mut self.edges[moved as usize];
            let side = usize::from(record.ends[0] != vertex);
            record.position[side] = at as u32;
        }
        if list.is_empty() {
            lists.remove(index);
            self.nodes[vertex as usize].bitmap &= !level_bit(level);
        }
    }

    /// Puts `edge` into both endpoints' lists at its level and refreshes the
    /// bitmaps above them.
    fn attach_edge(&mut self, edge: u32) {
        self.list_insert(edge, 0);
        self.list_insert(edge, 1);
        let [a, b] = self.edges[edge as usize].ends;
        self.refresh(self.nodes[a as usize].parent);
        self.refresh(self.nodes[b as usize].parent);
    }

    /// Takes `edge` out of both endpoints' lists (the record is kept).
    fn detach_edge(&mut self, edge: u32) {
        self.list_remove(edge, 0);
        self.list_remove(edge, 1);
        let [a, b] = self.edges[edge as usize].ends;
        self.refresh(self.nodes[a as usize].parent);
        self.refresh(self.nodes[b as usize].parent);
    }

    // ----- aggregates and navigation -----

    /// Recomputes size and bitmap of a non-vertex node from its children.
    fn recompute(&mut self, handle: u32) -> bool {
        let node = &self.nodes[handle as usize];
        debug_assert_ne!(node.kind, Kind::Vertex);
        let (mut size, mut bitmap) = (0u32, 0u64);
        for child in [node.left, node.right] {
            if child != NONE {
                size += self.nodes[child as usize].size;
                bitmap |= self.nodes[child as usize].bitmap;
            }
        }
        let node = &mut self.nodes[handle as usize];
        let changed = node.size != size || node.bitmap != bitmap;
        node.size = size;
        node.bitmap = bitmap;
        changed
    }

    /// Recomputes aggregates from `start` upward, stopping where nothing
    /// changes (ancestors depend only on their children). O(log N) steps.
    fn refresh(&mut self, start: u32) {
        let mut current = start;
        while current != NONE {
            if !self.recompute(current) {
                return;
            }
            current = self.nodes[current as usize].parent;
        }
    }

    /// The stored cluster whose local tree holds `handle`, or `NONE` for a
    /// root. Climbs local-tree nodes only.
    fn forest_parent(&self, handle: u32) -> u32 {
        let mut current = self.nodes[handle as usize].parent;
        while current != NONE && self.nodes[current as usize].kind != Kind::Cluster {
            current = self.nodes[current as usize].parent;
        }
        current
    }

    fn root(&self, mut handle: u32) -> u32 {
        loop {
            let parent = self.forest_parent(handle);
            if parent == NONE {
                return handle;
            }
            handle = parent;
        }
    }

    /// Fills `chains[which]` with `vertex` and its stored ancestors up to the
    /// root, bottom-up.
    fn fill_chain(&mut self, which: usize, vertex: u32) {
        let mut chain = std::mem::take(&mut self.chains[which]);
        chain.clear();
        let mut current = vertex;
        while current != NONE {
            chain.push(current);
            current = self.forest_parent(current);
        }
        self.chains[which] = chain;
    }

    /// Lowest stored common ancestor of the two filled chains (same root).
    fn lowest_common(&self) -> u32 {
        let [a, b] = &self.chains;
        let mut common = NONE;
        for (x, y) in a.iter().rev().zip(b.iter().rev()) {
            if x != y {
                break;
            }
            common = *x;
        }
        debug_assert_ne!(common, NONE);
        common
    }

    /// The element of `chains[which]` just below `ancestor`.
    fn child_on_chain(&self, which: usize, ancestor: u32) -> u32 {
        let chain = &self.chains[which];
        let at = chain
            .iter()
            .position(|&x| x == ancestor)
            .expect("ancestor is on the chain");
        chain[at - 1]
    }

    /// The child of cluster `parent` that contains vertex `vertex`.
    fn child_of(&self, parent: u32, vertex: u32) -> u32 {
        let mut current = vertex;
        loop {
            let up = self.forest_parent(current);
            debug_assert_ne!(up, NONE, "vertex lies inside the cluster");
            if up == parent {
                return current;
            }
            current = up;
        }
    }

    // ----- local trees (Wulff-Nilsen §3.4, §3.6) -----

    /// Detaches `cluster`'s rank path, freeing its path nodes, and pushes the
    /// rank-tree roots (now parentless) onto `out`. O(log N).
    fn take_rank_trees(&mut self, cluster: u32, out: &mut Vec<u32>) {
        let node = &mut self.nodes[cluster as usize];
        let (left, right) = (node.left, node.right);
        node.left = NONE;
        node.right = NONE;
        if left != NONE {
            self.nodes[left as usize].parent = NONE;
            out.push(left);
        }
        let mut current = right;
        while current != NONE {
            if self.nodes[current as usize].kind == Kind::Path {
                let (tree, next) = (
                    self.nodes[current as usize].left,
                    self.nodes[current as usize].right,
                );
                self.release(current);
                self.nodes[tree as usize].parent = NONE;
                out.push(tree);
                current = next;
            } else {
                self.nodes[current as usize].parent = NONE;
                out.push(current);
                current = NONE;
            }
        }
    }

    /// Adds a parentless rank tree to the binary counter `slots`, pairing
    /// equal ranks. A leaf (vertex or cluster) gets rank `⌊log₂ size⌋` first.
    fn slot_insert(&mut self, slots: &mut [u32; RANK_SLOTS], mut tree: u32) {
        if matches!(self.nodes[tree as usize].kind, Kind::Vertex | Kind::Cluster) {
            let size = self.nodes[tree as usize].size;
            self.nodes[tree as usize].rank = floor_log2(size);
        }
        let mut rank = self.nodes[tree as usize].rank as usize;
        while slots[rank] != NONE {
            let other = slots[rank];
            slots[rank] = NONE;
            let mut pair = Node::new(Kind::Rank);
            pair.left = other;
            pair.right = tree;
            pair.rank = (rank + 1) as u8;
            let handle = self.allocate(pair);
            self.nodes[other as usize].parent = handle;
            self.nodes[tree as usize].parent = handle;
            self.recompute(handle);
            tree = handle;
            rank += 1;
        }
        slots[rank] = tree;
    }

    /// Hangs the trees of `slots` from `cluster`'s rank path in decreasing
    /// rank and recomputes the path and the cluster. The caller refreshes
    /// above the cluster.
    fn hang_rank_trees(&mut self, cluster: u32, slots: &[u32; RANK_SLOTS]) {
        let mut trees = std::mem::take(&mut self.trees);
        trees.clear();
        trees.extend(slots.iter().rev().copied().filter(|&t| t != NONE));
        let mut path = std::mem::take(&mut self.path);
        path.clear();
        match trees.len() {
            0 => {}
            1 => {
                self.nodes[cluster as usize].left = trees[0];
                self.nodes[trees[0] as usize].parent = cluster;
            }
            count => {
                self.nodes[cluster as usize].left = trees[0];
                self.nodes[trees[0] as usize].parent = cluster;
                let mut current = cluster;
                for &tree in &trees[1..count - 1] {
                    let mut node = Node::new(Kind::Path);
                    node.parent = current;
                    node.left = tree;
                    let handle = self.allocate(node);
                    self.nodes[tree as usize].parent = handle;
                    self.nodes[current as usize].right = handle;
                    path.push(handle);
                    current = handle;
                }
                let last = trees[count - 1];
                self.nodes[current as usize].right = last;
                self.nodes[last as usize].parent = current;
            }
        }
        for &node in path.iter().rev() {
            self.recompute(node);
        }
        self.recompute(cluster);
        self.trees = trees;
        self.path = path;
    }

    /// Rebuilds `cluster`'s local tree from `extra` trees plus its own.
    fn rebuild_with(&mut self, cluster: u32, extra: &[u32]) {
        let mut slots = [NONE; RANK_SLOTS];
        let mut own = Vec::new();
        self.take_rank_trees(cluster, &mut own);
        for tree in own.into_iter().chain(extra.iter().copied()) {
            self.slot_insert(&mut slots, tree);
        }
        self.hang_rank_trees(cluster, &slots);
        self.refresh(self.nodes[cluster as usize].parent);
    }

    /// Adds `child` (parentless) to `cluster`'s children. O(log N).
    fn add_child(&mut self, cluster: u32, child: u32) {
        debug_assert_eq!(self.nodes[child as usize].parent, NONE);
        debug_assert!(self.nodes[child as usize].level < self.nodes[cluster as usize].level);
        self.rebuild_with(cluster, &[child]);
    }

    /// Removes `child` from `cluster`'s children, leaving it parentless.
    /// Splits its rank tree along the root path (pieces of distinct ranks)
    /// and re-pairs. O(log N).
    fn remove_child(&mut self, cluster: u32, child: u32) {
        debug_assert_eq!(self.forest_parent(child), cluster);
        let mut own = Vec::new();
        self.take_rank_trees(cluster, &mut own);
        let mut root = child;
        while self.nodes[root as usize].parent != NONE {
            root = self.nodes[root as usize].parent;
        }
        own.retain(|&tree| tree != root);
        let mut current = child;
        let mut parent = self.nodes[child as usize].parent;
        while parent != NONE {
            let node = &self.nodes[parent as usize];
            let sibling = if node.left == current {
                node.right
            } else {
                node.left
            };
            let up = node.parent;
            self.nodes[sibling as usize].parent = NONE;
            own.push(sibling);
            self.release(parent);
            current = parent;
            parent = up;
        }
        self.nodes[child as usize].parent = NONE;
        let mut slots = [NONE; RANK_SLOTS];
        for tree in own {
            self.slot_insert(&mut slots, tree);
        }
        self.hang_rank_trees(cluster, &slots);
        self.refresh(self.nodes[cluster as usize].parent);
    }

    /// Whether `cluster` has exactly one child.
    fn single_child(&self, cluster: u32) -> Option<u32> {
        let node = &self.nodes[cluster as usize];
        if node.right == NONE
            && node.left != NONE
            && matches!(
                self.nodes[node.left as usize].kind,
                Kind::Vertex | Kind::Cluster
            )
        {
            Some(node.left)
        } else {
            None
        }
    }

    /// Puts `replacement` (parentless, same size) where `old` hangs.
    fn replace_in_parent(&mut self, old: u32, replacement: u32) {
        let parent = self.nodes[old as usize].parent;
        self.nodes[replacement as usize].parent = parent;
        self.nodes[replacement as usize].rank = self.nodes[old as usize].rank;
        if parent != NONE {
            let node = &mut self.nodes[parent as usize];
            if node.left == old {
                node.left = replacement;
            } else {
                debug_assert_eq!(node.right, old);
                node.right = replacement;
            }
        }
        self.nodes[old as usize].parent = NONE;
    }

    /// Compression (ADR 0014, 5.7): a cluster with one child and no edge of
    /// its own level is removed, its child taking its place. Returns the
    /// node that now represents it. O(1); aggregates are unchanged.
    fn splice_if_redundant(&mut self, cluster: u32) -> u32 {
        if self.nodes[cluster as usize].kind != Kind::Cluster {
            return cluster;
        }
        let Some(child) = self.single_child(cluster) else {
            return cluster;
        };
        let level = self.nodes[cluster as usize].level;
        if self.nodes[child as usize].bitmap & level_bit(level) != 0 {
            return cluster;
        }
        self.nodes[cluster as usize].left = NONE;
        self.nodes[child as usize].parent = NONE;
        self.replace_in_parent(cluster, child);
        self.release(cluster);
        child
    }

    /// Stores a level-`level` cluster over `child`, in `child`'s place.
    fn wrap(&mut self, child: u32, level: u8) -> u32 {
        debug_assert!(self.nodes[child as usize].level < level);
        let wrapper = self.new_cluster(level);
        self.replace_in_parent(child, wrapper);
        self.nodes[wrapper as usize].left = child;
        self.nodes[child as usize].parent = wrapper;
        self.recompute(wrapper);
        wrapper
    }

    // ----- insertion -----

    /// Joins two roots for a new top-level edge (ADR 0014, 5.3 step 2).
    fn join_roots(&mut self, a: u32, b: u32) {
        let top = self.max_level;
        let level_a = self.nodes[a as usize].level;
        let level_b = self.nodes[b as usize].level;
        match (level_a == top, level_b == top) {
            (true, true) => {
                // Merge b's children into a.
                let mut trees = Vec::new();
                self.take_rank_trees(b, &mut trees);
                self.release(b);
                self.rebuild_with(a, &trees);
            }
            (true, false) => self.add_child(a, b),
            (false, true) => self.add_child(b, a),
            (false, false) => {
                let cluster = self.new_cluster(top);
                self.rebuild_with(cluster, &[a, b]);
            }
        }
    }

    // ----- deletion -----

    /// Repairs connectivity after removing an edge between children `a` and
    /// `b` of `cluster` at the cluster's level (ADR 0014, 5.4 steps 2–5).
    fn repair(&mut self, mut cluster: u32, mut a: u32, mut b: u32) {
        loop {
            let level = self.nodes[cluster as usize].level;
            self.stats.levels_visited += 1;
            let SearchOutcome::Split { exhausted, piece } = self.search(cluster, a, b, level)
            else {
                return;
            };
            self.stats.splits += 1;
            let parent = self.forest_parent(cluster);
            if parent != NONE {
                self.remove_child(parent, cluster);
            }
            let piece = match piece {
                Some(node) => {
                    // The exhausted side was merged into one node with no
                    // level-`level` edge left: it hangs under `parent` itself.
                    self.remove_child(cluster, node);
                    node
                }
                None => self.split_off(cluster, exhausted, level),
            };
            let rest = self.splice_if_redundant(cluster);
            if parent == NONE {
                return;
            }
            self.add_child(parent, rest);
            self.add_child(parent, piece);
            cluster = parent;
            a = piece;
            b = rest;
        }
    }

    /// Moves the (unmerged) reached clusters of side `exhausted` out of
    /// `cluster` into a new level-`level` cluster, spliced if redundant.
    fn split_off(&mut self, cluster: u32, exhausted: usize, level: u8) -> u32 {
        let mut moved = std::mem::take(&mut self.moved);
        moved.clear();
        moved.extend_from_slice(&self.sides[exhausted].queue);
        for &child in &moved {
            self.remove_child(cluster, child);
        }
        let piece = self.new_cluster(level);
        self.rebuild_with(piece, &moved);
        self.moved = moved;
        self.splice_if_redundant(piece)
    }

    fn next_epoch(&mut self) -> u32 {
        self.epoch += 1;
        if self.epoch >= 1 << 31 {
            for node in &mut self.nodes {
                node.mark = 0;
            }
            self.epoch = 1;
        }
        self.epoch
    }

    fn mark(&mut self, handle: u32, side: usize) {
        self.nodes[handle as usize].mark = (self.epoch << 1) | side as u32;
    }

    /// The side that marked `handle` in the current search, if any.
    fn marked_by(&self, handle: u32) -> Option<usize> {
        let mark = self.nodes[handle as usize].mark;
        (mark >> 1 == self.epoch).then_some((mark & 1) as usize)
    }

    /// Two alternating searches in the level-`level` cluster graph of
    /// `cluster` from children `a` and `b`, then the push/merge of the
    /// smaller side and the reattachment of the other (ADR 0014, 5.4 steps
    /// 2–4; Wulff-Nilsen §3.3).
    fn search(&mut self, cluster: u32, a: u32, b: u32, level: u8) -> SearchOutcome {
        self.next_epoch();
        let (size_a, size_b) = (self.nodes[a as usize].size, self.nodes[b as usize].size);
        self.sides[0].reset(a, size_a);
        self.sides[1].reset(b, size_b);
        self.mark(a, 0);
        self.mark(b, 1);
        let bit = level_bit(level);
        let mut turn = 0;
        loop {
            match self.step(cluster, turn, bit) {
                Step::Progress => turn = 1 - turn,
                Step::Meeting {
                    cluster: _meeting,
                    edge,
                } => {
                    // `turn` hit a cluster the other side (the owner) reached
                    // first; the owner's set includes it.
                    let owner = 1 - turn;
                    if self.sides[owner].size <= self.sides[turn].size {
                        self.push_and_merge(cluster, owner, level, None);
                        self.reattach_side(turn);
                    } else {
                        self.push_and_merge(cluster, turn, level, Some(edge));
                        self.reattach_side(owner);
                    }
                    return SearchOutcome::Connected;
                }
                Step::Exhausted => {
                    let other = 1 - turn;
                    if self.sides[turn].size <= self.sides[other].size {
                        let merged = self.push_and_merge(cluster, turn, level, None);
                        self.reattach_side(other);
                        return SearchOutcome::Split {
                            exhausted: turn,
                            piece: Some(merged),
                        };
                    }
                    self.push_and_merge(cluster, other, level, None);
                    self.reattach_side(turn);
                    return SearchOutcome::Split {
                        exhausted: turn,
                        piece: None,
                    };
                }
            }
        }
    }

    /// One step of side `side`: fetch and detach one level edge of a reached
    /// cluster, classify its far end.
    fn step(&mut self, cluster: u32, side: usize, bit: u64) -> Step {
        loop {
            let state = &self.sides[side];
            let Some(&reached) = state.queue.get(state.cursor) else {
                return Step::Exhausted;
            };
            if self.nodes[reached as usize].bitmap & bit == 0 {
                self.sides[side].cursor += 1;
                continue;
            }
            let near = self.descend(reached, bit);
            let level = bit.trailing_zeros() as u8 + 1;
            let lists = &self.vertex_lists[self.nodes[near as usize].aux as usize];
            let index = Self::list_index(self.nodes[near as usize].bitmap, level);
            let edge = *lists[index].edges.last().expect("bit set means non-empty");
            self.detach_edge(edge);
            self.sides[side].detached.push(edge);
            self.stats.fetched_edges += 1;
            let [x, y] = self.edges[edge as usize].ends;
            let far = if x == near { y } else { x };
            let child = self.child_of(cluster, far);
            return match self.marked_by(child) {
                None => {
                    self.mark(child, side);
                    let size = self.nodes[child as usize].size;
                    let state = &mut self.sides[side];
                    state.queue.push(child);
                    state.size += u64::from(size);
                    Step::Progress
                }
                Some(owner) if owner == side => {
                    self.stats.internal_fetches += 1;
                    Step::Progress
                }
                Some(_) => Step::Meeting {
                    cluster: child,
                    edge,
                },
            };
        }
    }

    /// Descends from `start` to a vertex whose level lists hold `bit`.
    fn descend(&self, start: u32, bit: u64) -> u32 {
        let mut current = start;
        debug_assert_ne!(self.nodes[current as usize].bitmap & bit, 0);
        while self.nodes[current as usize].kind != Kind::Vertex {
            let node = &self.nodes[current as usize];
            current = if node.left != NONE && self.nodes[node.left as usize].bitmap & bit != 0 {
                node.left
            } else {
                node.right
            };
        }
        current
    }

    /// Pushes side `side`'s detached edges (except `keep`, reattached at
    /// `level`) to `level - 1` and merges its reached clusters into one
    /// level-(level-1) cluster; returns the node representing them.
    fn push_and_merge(&mut self, cluster: u32, side: usize, level: u8, keep: Option<u32>) -> u32 {
        let target = level - 1;
        let detached = std::mem::take(&mut self.sides[side].detached);
        let pushed = detached.iter().any(|&edge| Some(edge) != keep);
        // ADR 0014, 5.4 step 6: nothing is ever pushed below level 1.
        debug_assert!(target >= 1 || !pushed, "no push below level 1");
        let mut reached = std::mem::take(&mut self.sides[side].queue);
        let merged = self.merge_reached(cluster, &reached, target, pushed);
        for &edge in &detached {
            if Some(edge) != keep {
                self.edges[edge as usize].level = target;
                self.stats.pushes += 1;
            }
            self.attach_edge(edge);
        }
        debug_assert!(
            u64::from(self.nodes[merged as usize].size) <= 1u64 << target,
            "size invariant after a push"
        );
        reached.clear();
        self.sides[side].queue = reached;
        let mut detached = detached;
        detached.clear();
        self.sides[side].detached = detached;
        merged
    }

    /// Reattaches side `side`'s detached edges at their (unchanged) level.
    fn reattach_side(&mut self, side: usize) {
        let detached = std::mem::take(&mut self.sides[side].detached);
        for &edge in &detached {
            self.attach_edge(edge);
        }
        let mut detached = detached;
        detached.clear();
        self.sides[side].detached = detached;
    }

    /// The merge rule of ADR 0014, 5.4: makes `reached` (children of
    /// `cluster`) one level-`target` cluster.
    fn merge_reached(&mut self, cluster: u32, reached: &[u32], target: u8, pushed: bool) -> u32 {
        if let [only] = *reached {
            let level = self.nodes[only as usize].level;
            return if pushed && level < target {
                self.stats.wraps += 1;
                self.wrap(only, target)
            } else {
                only
            };
        }
        for &child in reached {
            self.remove_child(cluster, child);
        }
        let host = reached
            .iter()
            .copied()
            .find(|&child| self.nodes[child as usize].level == target);
        let host = match host {
            Some(host) => host,
            None => self.new_cluster(target),
        };
        let mut trees = Vec::new();
        for &child in reached {
            if child == host {
                continue;
            }
            if self.nodes[child as usize].level == target {
                self.take_rank_trees(child, &mut trees);
                self.release(child);
            } else {
                trees.push(child);
            }
        }
        self.rebuild_with(host, &trees);
        self.add_child(cluster, host);
        host
    }

    /// Cheap structural checks after every operation in debug builds.
    fn check_quick(&self) {
        debug_assert!(self.sides.iter().all(|side| side.detached.is_empty()));
    }
}

#[cfg(test)]
mod tests;
