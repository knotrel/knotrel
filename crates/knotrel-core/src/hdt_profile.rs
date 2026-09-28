//! Read-only structural storage accounting for HDT.
//!
//! Vector capacities measure requested buffer storage, not allocator usable size.
//! Ordered payloads exclude B-tree spare slots and node metadata. Neither value
//! is total heap usage or RSS; allocator overhead and the snapshot are excluded.

/// Structural storage snapshot, collected outside timed graph operations.
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

/// Storage and live incidences of one materialized HDT level.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HdtLevelStorage {
    /// Level index.
    pub level: usize,
    /// Materialized vertex records, including retained isolated records.
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
    /// Local-to-global vertex mapping vector capacity bytes.
    pub mapping_capacity_bytes: usize,
    /// Live global-to-local mapping payload, excluding B-tree overhead.
    pub mapping_payload_bytes: usize,
}
