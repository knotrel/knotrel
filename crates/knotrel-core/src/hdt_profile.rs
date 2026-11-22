//! Read-only structural storage accounting for HDT.
//!
//! Vector capacities measure requested buffer storage, not allocator usable size.
//! Ordered payloads exclude B-tree spare slots and node metadata. Neither value
//! is total heap usage or RSS; allocator overhead and the snapshot are excluded.

/// Structural storage snapshot of an [`crate::HdtGraph`], from
/// [`crate::HdtGraph::storage_stats`], collected outside timed graph
/// operations.
///
/// A stable interface of the experimental HDT engine
/// ([`crate::EngineConfig::Hdt`]). The struct is exhaustive: its eight public
/// fields are frozen for 1.x, and a new field requires a major release.
///
/// # Guaranteed
///
/// For a snapshot `s` of a graph `g`, computed at the call:
///
/// - `s.vertex_count == g.node_count()` and `s.edge_count == g.edge_count()`.
/// - Every `*_bytes` field is in bytes; `vertex_count`, `edge_count`,
///   `available_levels` and the per-level counts are plain counts.
/// - `s.vector_capacity_bytes` is the sum of `s.level_capacity_bytes`,
///   `s.edge_arc_capacity_bytes` and, for every level, its
///   `token_capacity_bytes`, `vertex_index_capacity_bytes`,
///   `free_list_capacity_bytes`, `adjacency_capacity_bytes` and
///   `mapping_capacity_bytes`.
/// - `s.levels` is empty exactly when `s.vertex_count == 0`, and
///   `s.levels[i].level == i` for every index `i` (ascending, from level 0).
/// - `s.available_levels == 0` exactly when `s.vertex_count == 0`, and
///   `s.levels.len() <= s.available_levels`. Since vertices are never
///   removed, `available_levels` never decreases for one graph.
/// - Level 0 holds a spanning forest of the whole graph:
///   `s.levels[0].tree_edges` equals `vertex_count` minus the number of
///   connected components, and `tree_edges` does not increase from one level
///   to the next (each level's forest contains the next one's).
/// - Each edge has exactly one level, where it is counted at both endpoints:
///   the sum over all levels of `tree_incidences + non_tree_incidences` is
///   `2 * s.edge_count`, and the sum of `tree_incidences` is
///   `2 * s.levels[0].tree_edges`.
///
/// # Implementation-dependent
///
/// Every capacity and payload byte value, the number of materialized levels,
/// the per-level `vertices` record slots, which edges are tree edges at which
/// level, and the exact `available_levels` beyond the properties above depend
/// on the implementation, the operation history and the standard library's
/// growth policy, and may change in any release. Capacities retain their
/// peaks after removals. None of the values is total heap usage or RSS:
/// allocator overhead, hash table and B-tree overhead and the snapshot itself
/// are excluded.
///
/// # Examples
///
/// ```
/// use knotrel_core::HdtGraph;
///
/// let mut graph = HdtGraph::new();
/// graph.link(1, 2)?;
/// graph.link(2, 3)?;
/// graph.link(3, 1)?;
/// graph.add_node(4);
/// let storage = graph.storage_stats();
/// assert_eq!(storage.vertex_count, 4);
/// assert_eq!(storage.edge_count, 3);
/// // Four vertices in two components: a spanning forest of two edges.
/// assert_eq!(storage.levels[0].tree_edges, 2);
/// let incidences: usize = storage
///     .levels
///     .iter()
///     .map(|level| level.tree_incidences + level.non_tree_incidences)
///     .sum();
/// assert_eq!(incidences, 2 * storage.edge_count);
/// # Ok::<(), knotrel_core::GraphError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdtStorageStats {
    /// Registered graph vertices.
    pub vertex_count: usize,
    /// Live graph edges.
    pub edge_count: usize,
    /// Number of levels allowed by the current vertex universe.
    pub available_levels: usize,
    /// Owned vector capacities in bytes, excluding this snapshot.
    pub vector_capacity_bytes: usize,
    /// Live ordered-container key/value payload bytes; excludes B-tree overhead.
    pub ordered_payload_bytes: usize,
    /// Capacity bytes of per-edge tour handle vectors.
    pub edge_arc_capacity_bytes: usize,
    /// Capacity bytes of the level vector, including inline container headers.
    pub level_capacity_bytes: usize,
    /// Materialized levels, in ascending order.
    pub levels: Vec<HdtLevelStorage>,
}

/// Storage and live incidences of one materialized HDT level, an element of
/// [`HdtStorageStats::levels`].
///
/// A stable interface of the experimental HDT engine; the struct is
/// exhaustive, so its eleven public fields are frozen for 1.x. The properties
/// that relate these fields to each other and to the graph are guaranteed as
/// listed on [`HdtStorageStats`]; every byte value and `vertices` are
/// implementation-dependent. `Default` gives an all-zero value.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HdtLevelStorage {
    /// Level index.
    pub level: usize,
    /// Vertex record slots: live records plus released slots awaiting reuse.
    pub vertices: usize,
    /// Live spanning edges in this forest, including higher-level edges.
    pub tree_edges: usize,
    /// Exact-level tree incidences, counting both endpoints.
    pub tree_incidences: usize,
    /// Exact-level non-tree incidences, counting both endpoints.
    pub non_tree_incidences: usize,
    /// Euler-tour token arena capacity bytes.
    pub token_capacity_bytes: usize,
    /// Vertex-to-token vector capacity bytes.
    pub vertex_index_capacity_bytes: usize,
    /// Recycled-token handle vector capacity bytes.
    pub free_list_capacity_bytes: usize,
    /// Capacity bytes of both vectors of adjacency set headers.
    pub adjacency_capacity_bytes: usize,
    /// Capacity bytes of the local-to-global mapping and released-slot vectors.
    pub mapping_capacity_bytes: usize,
    /// Live global-to-local mapping payload, excluding B-tree overhead.
    pub mapping_payload_bytes: usize,
}
