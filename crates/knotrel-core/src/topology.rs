//! Registered topology with node-active and edge-enabled flags (M10).
//!
//! [`LifecycleGraph`] separates the *registered* topology (every node and edge
//! the application declared, with independent flags) from the *effective*
//! graph used for connectivity. An edge is effective exactly when it is
//! registered, enabled, and both endpoints are registered and active.
//!
//! # Design
//!
//! The facade owns two representations:
//!
//! - a **catalog**: for each registered node, its `active` flag and a vector
//!   of incident registered edges sorted by neighbor ID, each carrying the
//!   edge's `enabled` flag. The flag is stored at both endpoints and the two
//!   copies are always updated together;
//! - an inner **engine** ([`ConnectivityGraph`] without limits) holding every
//!   registered node as a vertex and exactly the effective edges.
//!
//! **Invariants,** restored before every public method returns:
//!
//! 1. `v` is in `incident(u)` with flag `f` iff `u` is in `incident(v)` with
//!    flag `f`; incident vectors are strictly sorted and never contain the
//!    owner itself.
//! 2. Every registered node is an engine vertex. Engine vertices that are not
//!    registered are *retired* slots: isolated, with no incident engine edge.
//! 3. The engine's edge set equals the effective edge set.
//! 4. The maintained counters equal the counts they name.
//!
//! Each mutation validates every ordinary error before changing either
//! representation. After validation, engine calls cannot fail (self-loops are
//! excluded and the engine has no limits), so a method that returns `Err`
//! made no change.
//!
//! # Retired engine slots
//!
//! The engines do not delete vertices. Removing a node cuts its effective
//! edges, which leaves its engine vertex isolated; registering the same ID
//! again reuses that vertex, which therefore carries no stale adjacency.
//! A retired slot keeps no adjacency allocation (it is released on removal),
//! only its engine ID-map entry and an empty vector header. When retired
//! slots exceed `max(V, 1024)`, the engine is rebuilt from the catalog.
//! A rebuild costs `O(V log V + E)` (an ID sort plus one scan of the active
//! nodes' incidences, with `V + E_eff` sequential engine insertions); more
//! than `max(V, 1024)` removals separate two rebuilds, so the amortized cost
//! is `O(log V + E / V)` per removal, and engine vertices stay at most
//! `V + max(V, 1024)`, where `V` is the registered node count. (A
//! `max(V / 4, 1024)` threshold was evaluated for v0.6.0 and rejected: it
//! made fresh-ID churn about 90% slower in diagnostics.)
//!
//! # Catalog tombstones
//!
//! Removing a node from the catalog hash table can leave a tombstone, which
//! uses insert headroom without holding an entry. Under fresh-ID churn the
//! headroom runs out while live entries stay constant, and the table would
//! double although it is far from full (at 100,000 nodes, from 131,072 to
//! 262,144 buckets within a few percent of churn). Before an insert that would
//! grow the table, the catalog is instead rebuilt into a fresh table sized for
//! its live entries when they are at most 90% of the table's full capacity
//! (its capacity right after its last growth or rebuild): entries move, no
//! incident vector is copied, and the tombstones are dropped. A table sized
//! for the live entries has the same bucket count whenever they exceed half
//! of the old capacity (the case under churn), and fewer buckets otherwise; it
//! is never larger. In the rare case where the live entries exactly fill the
//! new table's capacity, the pending insert then grows it, which costs a
//! second `O(V)` pass. A rebuild costs
//! `O(V)` and runs only after at least 10% of the full capacity has been
//! consumed by tombstones since the last one, so its amortized cost is `O(1)`
//! per insertion; it holds the old and the new table only while it runs, as a
//! growth does, with the new table no larger than the old one. Otherwise the
//! table grows as usual. (Diagnostic D2c of the v0.7.0 qualification: at
//! 100,000 nodes this removes the growth, lowers peak and steady-state heap by
//! about 5 MiB, and leaves construction unchanged.)

use crate::{ConnectivityGraph, EngineConfig, GraphError, GraphLimits, NodeId};
use rustc_hash::FxHashMap;
use std::{error::Error, fmt};

/// Minimum number of retired engine slots before a compaction is considered.
const MIN_RETIRED_BEFORE_COMPACTION: usize = 1024;

/// Smallest incident-vector capacity that is ever shrunk.
const MIN_SHRINK_CAPACITY: usize = 16;

/// A catalog insert that would grow the table rebuilds it instead when live
/// entries are at most `REBUILD_LIVE_NUMERATOR / REBUILD_LIVE_DENOMINATOR` of
/// the table's full capacity (see the module documentation).
const REBUILD_LIVE_NUMERATOR: usize = 9;
const REBUILD_LIVE_DENOMINATOR: usize = 10;

/// One registered node for [`LifecycleGraph::try_from_topology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeSpec {
    /// The node identifier.
    pub node: NodeId,
    /// Whether the node is active.
    pub active: bool,
}

/// One registered undirected edge for [`LifecycleGraph::try_from_topology`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeSpec {
    /// One endpoint.
    pub source: NodeId,
    /// The other endpoint; orientation is not significant.
    pub target: NodeId,
    /// Whether the edge is enabled.
    pub enabled: bool,
}

/// Flags of one registered edge, returned by [`LifecycleGraph::edge_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EdgeState {
    /// The edge's own flag, independent of its endpoints.
    pub enabled: bool,
    /// Whether the edge participates in connectivity: enabled, with both
    /// endpoints active.
    pub effective: bool,
}

impl EdgeState {
    /// Creates edge flags with the given values.
    ///
    /// [`LifecycleGraph::edge_state`] is the normal source of an `EdgeState`.
    /// This constructor exists because the struct is `#[non_exhaustive]` (a
    /// future release may add a flag), so code outside this crate cannot use a
    /// struct literal; use it to build expected values in comparisons, for
    /// example in a test or a reference model. The values are not validated:
    /// `LifecycleGraph` never reports `effective` without `enabled`.
    ///
    /// ```
    /// use knotrel_core::{EdgeState, EngineConfig, LifecycleGraph};
    /// let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
    /// graph.link(1, 2)?;
    /// graph.set_node_active(2, false)?;
    /// assert_eq!(graph.edge_state(1, 2), Some(EdgeState::new(true, false)));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub const fn new(enabled: bool, effective: bool) -> Self {
        Self { enabled, effective }
    }
}

/// Storage diagnostics of a [`LifecycleGraph`], from
/// [`LifecycleGraph::storage_stats`].
///
/// Logical sizes and retained capacities are reported separately: vectors
/// and hash maps keep capacity after removals, so capacity can exceed the
/// logical size. Capacities count elements, not bytes, and exclude allocator
/// overhead and the inner engine's own capacities.
///
/// The type, its six fields and its trait implementations are stable. It is
/// `#[non_exhaustive]`: a minor release may add fields, but none of these is
/// removed or changes meaning or unit in 1.x.
///
/// # Guaranteed
///
/// For a value `s` returned by `g.storage_stats()`:
///
/// - Every field is a count of elements (entries, vertices or slots), never
///   bytes.
/// - The values are computed at the call; `s` is a copy that does not follow
///   later mutations of `g`.
/// - `s.catalog_nodes == g.node_count()`,
///   `s.engine_vertices == g.engine_vertex_count()`,
///   `s.engine_vertices == s.catalog_nodes + s.retired_vertices` and
///   `s.incident_entries == 2 * g.edge_count()` (registered edges, enabled or
///   not, counted at both endpoints).
/// - `s.catalog_node_capacity >= s.catalog_nodes` and
///   `s.incident_capacity >= s.incident_entries`.
/// - `s.engine_vertices <= 2 * s.catalog_nodes + 1024`, the bound of
///   [`LifecycleGraph::engine_vertex_count`].
/// - On a graph with no registered node and no removal (for example a new
///   one), `engine_vertices`, `retired_vertices`, `catalog_nodes` and
///   `incident_entries` are `0`.
///
/// # Implementation-dependent
///
/// The capacities (initial values, growth, what is retained after removals
/// and catalog rebuilds), and how many retired slots exist below the bound
/// above, that is when retired slots are compacted, depend on the
/// implementation, the operation history and the standard library's growth
/// policy, and may change in any release. None of the values measures heap
/// usage or RSS.
///
/// # Examples
///
/// ```
/// use knotrel_core::{EngineConfig, LifecycleGraph};
///
/// let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
/// graph.link(1, 2)?;
/// graph.link(2, 3)?;
/// assert!(graph.remove_node(3));
/// let storage = graph.storage_stats();
/// assert_eq!(storage.catalog_nodes, graph.node_count());
/// assert_eq!(storage.incident_entries, 2 * graph.edge_count());
/// assert_eq!(
///     storage.engine_vertices,
///     storage.catalog_nodes + storage.retired_vertices
/// );
/// assert!(storage.incident_capacity >= storage.incident_entries);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LifecycleStorage {
    /// Vertices held by the inner engine, including retired slots.
    pub engine_vertices: usize,
    /// Retired engine slots of removed nodes not yet compacted.
    pub retired_vertices: usize,
    /// Registered nodes in the catalog.
    pub catalog_nodes: usize,
    /// Capacity of the catalog's node map (elements).
    pub catalog_node_capacity: usize,
    /// Incidence entries in the catalog: two per registered edge.
    pub incident_entries: usize,
    /// Sum of the incident vectors' capacities (elements).
    pub incident_capacity: usize,
}

/// Errors returned by lifecycle-specific [`LifecycleGraph`] operations.
///
/// The enum is non-exhaustive so that later lifecycle errors can be added
/// without breaking matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LifecycleError {
    /// A graph error shared with the plain engines: a limit, a self-loop or
    /// an unknown node.
    Graph(GraphError),
    /// Both endpoints are registered but the edge between them is not.
    UnknownEdge {
        /// The first endpoint, as supplied.
        source: NodeId,
        /// The second endpoint, as supplied.
        target: NodeId,
    },
    /// [`LifecycleGraph::try_from_topology`] received the same node twice
    /// with different `active` flags.
    ConflictingNode {
        /// The repeated node.
        node: NodeId,
    },
    /// [`LifecycleGraph::try_from_topology`] received the same unordered
    /// edge twice with different `enabled` flags.
    ConflictingEdge {
        /// The first endpoint of the repeated entry, as supplied.
        source: NodeId,
        /// The second endpoint of the repeated entry, as supplied.
        target: NodeId,
    },
    /// Lifecycle support is not available for this engine. In v0.5.0 only
    /// [`EngineConfig::CompactBfs`] is supported; `ett` and `hdt` lifecycle
    /// support is deferred. No graph is created and no engine is substituted.
    UnsupportedEngine {
        /// The requested engine.
        engine: EngineConfig,
    },
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => error.fmt(formatter),
            Self::UnknownEdge { source, target } => {
                write!(formatter, "unknown edge: {source}-{target}")
            }
            Self::ConflictingNode { node } => {
                write!(
                    formatter,
                    "node {node} is repeated with a different active flag"
                )
            }
            Self::ConflictingEdge { source, target } => write!(
                formatter,
                "edge {source}-{target} is repeated with a different enabled flag"
            ),
            Self::UnsupportedEngine { engine } => write!(
                formatter,
                "node and edge lifecycle support is not available for engine {}; \
                 only compact-bfs is supported",
                engine.as_str()
            ),
        }
    }
}

impl Error for LifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GraphError> for LifecycleError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

/// One incident registered edge, stored at an endpoint.
#[derive(Debug, Clone, Copy)]
struct Incidence {
    neighbor: NodeId,
    enabled: bool,
}

/// Catalog entry of one registered node.
#[derive(Debug, Default)]
struct NodeRecord {
    active: bool,
    /// Construction-only scratch, zero between public calls: the saturating
    /// degree count that sizes `incident` in bulk import (only a capacity
    /// hint), then the node's engine index during
    /// [`LifecycleGraph::build_engine`]. It occupies the record's padding, so
    /// the record stays 32 bytes on 64-bit targets.
    pending: u32,
    /// Incident registered edges, strictly sorted by `neighbor`.
    incident: Vec<Incidence>,
}

impl NodeRecord {
    fn active() -> Self {
        Self {
            active: true,
            pending: 0,
            incident: Vec::new(),
        }
    }

    fn position(&self, neighbor: NodeId) -> Result<usize, usize> {
        self.incident
            .binary_search_by_key(&neighbor, |incidence| incidence.neighbor)
    }

    /// Removes the incidence at `index`, shrinking a mostly empty vector.
    fn remove_at(&mut self, index: usize) -> Incidence {
        let removed = self.incident.remove(index);
        let capacity = self.incident.capacity();
        if capacity > MIN_SHRINK_CAPACITY && self.incident.len() * 4 < capacity {
            self.incident.shrink_to(self.incident.len() * 2);
        }
        removed
    }
}

/// A graph with reversible node activation, reversible edge enabling and
/// permanent node removal.
///
/// Connectivity is answered over the **effective graph**: active nodes and
/// the registered edges that are enabled and have two active endpoints. The
/// plain operations [`add_node`](Self::add_node), [`link`](Self::link) and
/// [`cut`](Self::cut) keep their [`ConnectivityGraph`] meaning on the
/// registered topology: `add_node` never reactivates a node, `link` never
/// re-enables an edge and `cut` removes a registered edge whether or not it is
/// effective.
///
/// [`GraphLimits`] bound **registered** nodes and edges, including inactive
/// nodes and disabled edges.
///
/// # Costs
///
/// Let `d` be the registered degree of the node concerned and `U` the cost of
/// one effective edge update in the selected engine. Deactivation and
/// activation cost `O(d)` plus one engine update per effective incident edge.
/// Removal also edits every neighbor's incident vector, `O(d + sum of
/// neighbor degrees)`, and pays an amortized share of engine compaction (see
/// the module documentation). None of these bounds is logarithmic.
///
/// Memory holds the registered topology in a catalog and the effective graph
/// in the engine, roughly twice the plain engine for an all-active graph.
///
/// # Failure
///
/// A method that returns `Err` made no change. A panic during a mutation
/// leaves the graph unspecified; discard it.
///
/// # Examples
///
/// ```rust
/// use knotrel_core::{EngineConfig, LifecycleGraph};
///
/// let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
/// graph.link(1, 2)?;
/// graph.link(2, 3)?;
///
/// // Deactivate node 2: both incident edges stop being effective.
/// assert!(graph.set_node_active(2, false)?);
/// assert!(!graph.connected(1, 3)?);
///
/// // Disable one incident edge while node 2 is inactive.
/// assert!(graph.set_edge_enabled(2, 3, false)?);
///
/// // Reactivate: only the still-enabled edge returns.
/// assert!(graph.set_node_active(2, true)?);
/// assert!(graph.connected(1, 2)?);
/// assert!(!graph.connected(1, 3)?);
///
/// // Removal is permanent; re-adding the ID creates an isolated node.
/// assert!(graph.remove_node(2));
/// assert!(graph.add_node(2)?);
/// assert!(!graph.connected(1, 2)?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct LifecycleGraph {
    // `FxHashMap`, not the identity-hashed `IntMap`: with hashbrown, identity
    // hashes of small IDs or IDs sharing their high bits collide in the 7-bit
    // control tag and degrade probing (measured in the v0.5.0 qualification).
    nodes: FxHashMap<NodeId, NodeRecord>,
    engine: ConnectivityGraph,
    /// Configured limits, as returned by [`Self::limits`].
    limits: GraphLimits,
    /// Limits actually enforced: the configured ones tightened by the
    /// engine's own capacity ([`Self::bounded`]).
    checks: GraphLimits,
    active_nodes: usize,
    registered_edges: usize,
    enabled_edges: usize,
    effective_edges: usize,
    /// The catalog's `capacity()` right after its last growth or rebuild;
    /// tombstones lower `capacity()` below it.
    catalog_full_capacity: usize,
}

impl LifecycleGraph {
    /// Reports whether lifecycle support is available for `engine`.
    ///
    /// [`EngineConfig::CompactBfs`] and, from 1.1,
    /// [`EngineConfig::ClusterForest`] are supported. Lifecycle support for
    /// the experimental `ett` and `hdt` engines is deferred; their plain
    /// [`ConnectivityGraph`] use is unaffected.
    #[must_use]
    pub const fn supports_engine(engine: EngineConfig) -> bool {
        matches!(
            engine,
            EngineConfig::CompactBfs | EngineConfig::ClusterForest
        )
    }

    /// Tightens `limits` by the engine's capacity, so the engine never
    /// rejects (or panics on) a mutation the catalog accepted.
    ///
    /// `cluster-forest` holds at most `max_elements()` (2²⁸) vertices and
    /// edges. Engine vertices include retired slots, at most
    /// `max(catalog, 1024)` of them between compactions (see
    /// [`Self::compact_if_needed`]), so at most `2 × catalog + 1024`: the
    /// catalog is capped at `(2²⁸ − 1024) / 2` nodes and registered edges
    /// (an upper bound of effective edges) at 2²⁸. Other engines are
    /// unlimited.
    fn bounded(engine: EngineConfig, limits: GraphLimits) -> GraphLimits {
        if engine != EngineConfig::ClusterForest {
            return limits;
        }
        let capacity = crate::cluster_forest::max_elements();
        let nodes = capacity.saturating_sub(MIN_RETIRED_BEFORE_COMPACTION) / 2;
        let tighten = |configured: Option<usize>, cap: usize| {
            Some(configured.map_or(cap, |limit| limit.min(cap)))
        };
        GraphLimits {
            max_nodes: tighten(limits.max_nodes, nodes),
            max_edges: tighten(limits.max_edges, capacity),
        }
    }

    fn check_engine(engine: EngineConfig) -> Result<(), LifecycleError> {
        if Self::supports_engine(engine) {
            Ok(())
        } else {
            Err(LifecycleError::UnsupportedEngine { engine })
        }
    }

    /// Creates an empty unlimited graph using `engine` for connectivity.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::UnsupportedEngine`] unless
    /// [`supports_engine`](Self::supports_engine) holds for `engine`.
    pub fn new(engine: EngineConfig) -> Result<Self, LifecycleError> {
        Self::with_limits(engine, GraphLimits::default())
    }

    /// Creates an empty graph with cardinality limits on registered topology.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::UnsupportedEngine`] unless
    /// [`supports_engine`](Self::supports_engine) holds for `engine`.
    pub fn with_limits(engine: EngineConfig, limits: GraphLimits) -> Result<Self, LifecycleError> {
        Self::check_engine(engine)?;
        Ok(Self {
            nodes: FxHashMap::default(),
            engine: ConnectivityGraph::new(engine),
            limits,
            checks: Self::bounded(engine, limits),
            active_nodes: 0,
            registered_edges: 0,
            enabled_edges: 0,
            effective_edges: 0,
            catalog_full_capacity: 0,
        })
    }

    /// Builds a graph from complete registered topology.
    ///
    /// Registers `nodes` in order, then `edges` in order, creating missing
    /// edge endpoints as active nodes. Repeated entries with identical flags
    /// are accepted once; edges compare as unordered pairs.
    ///
    /// The catalog is assembled first and incident vectors are sorted once,
    /// `O((V + E) log E)`; the engine is then built in one pass from the
    /// catalog with exactly the effective edges, `O(V log V + E)`.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::UnsupportedEngine`] first, before any entry is
    /// examined, unless [`supports_engine`](Self::supports_engine) holds for
    /// `engine`. Otherwise processing stops at the first failing entry, in
    /// input order:
    /// [`LifecycleError::ConflictingNode`] or
    /// [`LifecycleError::ConflictingEdge`] for a repeated entry with a
    /// different flag; [`GraphError::SelfLoop`], then
    /// [`GraphError::NodeLimitExceeded`], then
    /// [`GraphError::EdgeLimitExceeded`] (for a new edge) wrapped in
    /// [`LifecycleError::Graph`].
    pub fn try_from_topology(
        engine: EngineConfig,
        limits: GraphLimits,
        nodes: &[NodeSpec],
        edges: &[EdgeSpec],
    ) -> Result<Self, LifecycleError> {
        Self::check_engine(engine)?;
        let checks = Self::bounded(engine, limits);
        match Self::catalog_fast(checks, nodes, edges) {
            Some((catalog, order)) => Ok(Self::finish(engine, limits, catalog, order)),
            None => Self::catalog_exact(checks, nodes, edges)
                .map(|(catalog, order)| Self::finish(engine, limits, catalog, order)),
        }
    }

    /// Catalog assembly for inputs that cannot fail: returns `None`, without
    /// side effects, whenever an error could arise (a self-loop, a conflicting
    /// repeated node or edge, a node limit reached, or more edge entries than
    /// the edge limit), and [`Self::catalog_exact`] then reproduces the exact
    /// first error in input order.
    ///
    /// Edges are appended without a per-edge duplicate lookup: incident lists
    /// are sorted once and adjacent repeats are dropped (a repeated edge with a
    /// different flag returns `None`). Without a node limit each endpoint costs
    /// two expected O(1) catalog probes (counting pass and fill pass; two more
    /// per edge with a node limit) instead of the exact path's duplicate-map
    /// lookup and insertion plus endpoint probes. `O(V + E log E)` time
    /// overall, the same bound as the exact path. An input that fails is
    /// detected here first, so error paths pay up to about twice the work.
    fn catalog_fast(
        limits: GraphLimits,
        nodes: &[NodeSpec],
        edges: &[EdgeSpec],
    ) -> Option<(FxHashMap<NodeId, NodeRecord>, Vec<NodeId>)> {
        if limits.max_edges.is_some_and(|limit| edges.len() > limit) {
            return None;
        }
        let mut catalog: FxHashMap<NodeId, NodeRecord> = FxHashMap::default();
        catalog.reserve(nodes.len());
        let mut order: Vec<NodeId> = Vec::with_capacity(nodes.len());
        for spec in nodes {
            match catalog.entry(spec.node) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    if entry.get().active != spec.active {
                        return None;
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    if limits.max_nodes.is_some_and(|limit| order.len() >= limit) {
                        return None;
                    }
                    entry.insert(NodeRecord {
                        active: spec.active,
                        pending: 0,
                        incident: Vec::new(),
                    });
                    order.push(spec.node);
                }
            }
        }
        for spec in edges {
            if spec.source == spec.target {
                return None;
            }
            if let Some(limit) = limits.max_nodes {
                let missing = usize::from(!catalog.contains_key(&spec.source))
                    + usize::from(!catalog.contains_key(&spec.target));
                if missing > limit.saturating_sub(order.len()) {
                    return None;
                }
            }
            // Count degrees first so each incident vector is allocated once:
            // exactly sized for duplicate-free input (repeated entries are
            // counted, so such vectors are shrunk by `finish`).
            for endpoint in [spec.source, spec.target] {
                let record = catalog.entry(endpoint).or_insert_with(|| {
                    order.push(endpoint);
                    NodeRecord::active()
                });
                record.pending = record.pending.saturating_add(1);
            }
        }
        for record in catalog.values_mut() {
            record.incident = Vec::with_capacity(record.pending as usize);
            record.pending = 0;
        }
        for spec in edges {
            for (endpoint, neighbor) in [(spec.source, spec.target), (spec.target, spec.source)] {
                catalog
                    .get_mut(&endpoint)
                    .expect("endpoint registered by the counting pass")
                    .incident
                    .push(Incidence {
                        neighbor,
                        enabled: spec.enabled,
                    });
            }
        }
        for record in catalog.values_mut() {
            record
                .incident
                .sort_unstable_by_key(|incidence| incidence.neighbor);
            let mut conflict = false;
            record.incident.dedup_by(|later, kept| {
                let repeat = later.neighbor == kept.neighbor;
                conflict |= repeat && later.enabled != kept.enabled;
                repeat
            });
            if conflict {
                return None;
            }
        }
        Some((catalog, order))
    }

    /// Exact catalog assembly: processes entries in input order and stops at
    /// the first failing one (see [`Self::try_from_topology`]). Incident lists
    /// are sorted here. `O((V + E) log E)` time.
    fn catalog_exact(
        limits: GraphLimits,
        nodes: &[NodeSpec],
        edges: &[EdgeSpec],
    ) -> Result<(FxHashMap<NodeId, NodeRecord>, Vec<NodeId>), LifecycleError> {
        let mut catalog: FxHashMap<NodeId, NodeRecord> = FxHashMap::default();
        catalog.reserve(nodes.len());
        let mut order: Vec<NodeId> = Vec::with_capacity(nodes.len());
        let node_limit = limits.max_nodes;
        for spec in nodes {
            if let Some(record) = catalog.get(&spec.node) {
                if record.active != spec.active {
                    return Err(LifecycleError::ConflictingNode { node: spec.node });
                }
                continue;
            }
            if let Some(limit) = node_limit
                && order.len() >= limit
            {
                return Err(GraphError::NodeLimitExceeded { limit }.into());
            }
            catalog.insert(
                spec.node,
                NodeRecord {
                    active: spec.active,
                    pending: 0,
                    incident: Vec::new(),
                },
            );
            order.push(spec.node);
        }

        let mut seen: FxHashMap<(NodeId, NodeId), bool> = FxHashMap::default();
        seen.reserve(edges.len());
        for spec in edges {
            GraphError::check_distinct(spec.source, spec.target)?;
            let missing = usize::from(!catalog.contains_key(&spec.source))
                + usize::from(!catalog.contains_key(&spec.target));
            if let Some(limit) = node_limit
                && missing > limit.saturating_sub(order.len())
            {
                return Err(GraphError::NodeLimitExceeded { limit }.into());
            }
            let key = (spec.source.min(spec.target), spec.source.max(spec.target));
            if let Some(&enabled) = seen.get(&key) {
                if enabled != spec.enabled {
                    return Err(LifecycleError::ConflictingEdge {
                        source: spec.source,
                        target: spec.target,
                    });
                }
                continue;
            }
            if let Some(limit) = limits.max_edges
                && seen.len() >= limit
            {
                return Err(GraphError::EdgeLimitExceeded { limit }.into());
            }
            for endpoint in [spec.source, spec.target] {
                catalog.entry(endpoint).or_insert_with(|| {
                    order.push(endpoint);
                    NodeRecord::active()
                });
            }
            seen.insert(key, spec.enabled);
            let incidence = |neighbor| Incidence {
                neighbor,
                enabled: spec.enabled,
            };
            catalog
                .get_mut(&spec.source)
                .expect("endpoint registered above")
                .incident
                .push(incidence(spec.target));
            catalog
                .get_mut(&spec.target)
                .expect("endpoint registered above")
                .incident
                .push(incidence(spec.source));
        }
        drop(seen);

        for record in catalog.values_mut() {
            record
                .incident
                .sort_unstable_by_key(|incidence| incidence.neighbor);
        }
        Ok((catalog, order))
    }

    /// Shrinks incident lists, computes counts and builds the engine from the
    /// effective edges of a validated catalog with sorted, duplicate-free
    /// incident lists. `O(V log V + E)` (see [`Self::build_engine`]).
    fn finish(
        engine: EngineConfig,
        limits: GraphLimits,
        mut catalog: FxHashMap<NodeId, NodeRecord>,
        order: Vec<NodeId>,
    ) -> Self {
        let mut active_nodes = 0;
        let mut registered_edges = 0;
        let mut enabled_incidences = 0;
        for record in catalog.values_mut() {
            record.incident.shrink_to_fit();
            registered_edges += record.incident.len();
            enabled_incidences += record.incident.iter().filter(|i| i.enabled).count();
            active_nodes += usize::from(record.active);
        }
        let (engine_graph, effective_edges) = Self::build_engine(engine, &mut catalog, &order);
        let catalog_full_capacity = catalog.capacity();
        Self {
            nodes: catalog,
            engine: engine_graph,
            limits,
            checks: Self::bounded(engine, limits),
            active_nodes,
            registered_edges: registered_edges / 2,
            enabled_edges: enabled_incidences / 2,
            effective_edges,
            catalog_full_capacity,
        }
    }

    /// Builds an unlimited engine holding every catalog node as a vertex and
    /// exactly the effective edges; returns it with the effective edge count.
    ///
    /// For `compact-bfs` (the only engine with lifecycle support) the engine
    /// is built in one pass ([`Graph::from_sorted_adjacency`](crate::Graph::from_sorted_adjacency)): node IDs are
    /// sorted and receive engine indices in ascending ID order (the index is
    /// parked in the construction-only `pending` field, reset to zero
    /// afterwards). Each effective edge costs one catalog probe, from its
    /// smaller endpoint, and is appended to both adjacency lists; visiting
    /// nodes in index order keeps every list strictly sorted. Each
    /// adjacency vector is allocated once with capacity equal to the node's
    /// enabled registered degree (an upper bound of its effective degree), so
    /// no vector grows. `O(V log V + E)` time with expected `O(1)` probes;
    /// scratch memory is the `O(V)` sorted ID vector.
    ///
    /// Other engines, and catalogs too large for 32-bit indices, use the
    /// incremental [`Self::build_engine_incremental`]. `order` is used only
    /// by that path, as the vertex insertion order when it lists every catalog
    /// node; otherwise (compaction passes an empty slice) the catalog's keys
    /// are used, so no registered node is ever left out of the engine.
    fn build_engine(
        engine: EngineConfig,
        catalog: &mut FxHashMap<NodeId, NodeRecord>,
        order: &[NodeId],
    ) -> (ConnectivityGraph, usize) {
        if engine != EngineConfig::CompactBfs || u32::try_from(catalog.len()).is_err() {
            // `order` may be empty (compaction passes none): every catalog node
            // must still become a vertex, so fall back to the catalog's keys.
            if order.len() == catalog.len() {
                return Self::build_engine_incremental(engine, catalog, order);
            }
            let keys: Vec<NodeId> = catalog.keys().copied().collect();
            return Self::build_engine_incremental(engine, catalog, &keys);
        }
        let mut ids: Vec<NodeId> = catalog.keys().copied().collect();
        ids.sort_unstable();
        for (index, node) in ids.iter().enumerate() {
            catalog
                .get_mut(node)
                .expect("listed node is registered")
                .pending = u32::try_from(index).expect("node count checked above");
        }
        let records: Vec<&NodeRecord> = ids.iter().map(|node| &catalog[node]).collect();
        let mut adjacency: Vec<Vec<usize>> = records
            .iter()
            .map(|record| {
                if record.active {
                    Vec::with_capacity(record.incident.iter().filter(|i| i.enabled).count())
                } else {
                    Vec::new()
                }
            })
            .collect();
        // Each effective edge {u, v} with u < v is found once, from u, and
        // pushed to both lists. Nodes are visited in ascending index order, so
        // `adjacency[x]` first receives its smaller neighbors (pushed while
        // visiting them, in ascending order) and then its larger ones (from
        // its own sorted incident list): every list ends strictly sorted.
        let mut edges = 0;
        for (index, record) in records.iter().enumerate() {
            if !record.active {
                continue;
            }
            let node = ids[index];
            for incidence in &record.incident {
                if incidence.neighbor < node || !incidence.enabled {
                    continue;
                }
                let neighbor = &catalog[&incidence.neighbor];
                if neighbor.active {
                    let other = neighbor.pending as usize;
                    adjacency[index].push(other);
                    adjacency[other].push(index);
                    edges += 1;
                }
            }
        }
        drop(records);
        for record in catalog.values_mut() {
            record.pending = 0;
        }
        let graph = crate::Graph::from_sorted_adjacency(ids, adjacency);
        (ConnectivityGraph::from(graph), edges)
    }

    /// Incremental engine construction through
    /// [`ConnectivityGraph::try_from_edges`]: vertices in `order`, then the
    /// effective edges. `O(V + E)` catalog work plus one engine insertion per
    /// effective edge. Used for engines other than `compact-bfs` and as the
    /// differential reference in tests.
    fn build_engine_incremental(
        engine: EngineConfig,
        catalog: &FxHashMap<NodeId, NodeRecord>,
        order: &[NodeId],
    ) -> (ConnectivityGraph, usize) {
        let mut effective: Vec<(NodeId, NodeId)> = Vec::new();
        for &node in order {
            let record = &catalog[&node];
            if !record.active {
                continue;
            }
            for incidence in &record.incident {
                if node < incidence.neighbor
                    && incidence.enabled
                    && catalog[&incidence.neighbor].active
                {
                    effective.push((node, incidence.neighbor));
                }
            }
        }
        let graph =
            ConnectivityGraph::try_from_edges(engine, GraphLimits::default(), order, &effective)
                .expect("an unlimited engine accepts a validated simple graph");
        (graph, effective.len())
    }

    /// Returns the **configured** limits on registered topology, exactly as
    /// passed at construction; they are not adjusted for the engine.
    ///
    /// The **enforced** limits are the configured ones, except with
    /// [`EngineConfig::ClusterForest`], where they can be lower:
    /// at most `2²⁷ − 512` registered nodes and `2²⁸` registered edges,
    /// whatever is configured (the engine's capacity, with room for retired
    /// slots); mutations beyond them return [`GraphError::NodeLimitExceeded`]
    /// or [`GraphError::EdgeLimitExceeded`] carrying the enforced limit.
    #[must_use]
    pub fn limits(&self) -> GraphLimits {
        self.limits
    }

    /// Returns the engine that answers connectivity.
    #[must_use]
    pub fn engine(&self) -> EngineConfig {
        self.engine.engine()
    }

    /// Registers `node` as an active isolated node.
    ///
    /// Returns `Ok(false)` if the node is already registered; an inactive
    /// node stays inactive. Use [`set_node_active`](Self::set_node_active) to
    /// reactivate it. A previously removed ID is registered as a fresh node
    /// without its former edges.
    ///
    /// # Errors
    ///
    /// [`GraphError::NodeLimitExceeded`] when registering a new node would
    /// exceed the enforced node limit: `max_nodes`, or the engine's lower
    /// capacity (see [`Self::limits`]). The error carries the enforced value.
    pub fn add_node(&mut self, node: NodeId) -> Result<bool, GraphError> {
        if self.nodes.contains_key(&node) {
            return Ok(false);
        }
        if let Some(limit) = self.checks.max_nodes
            && self.nodes.len() >= limit
        {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        self.register(node);
        Ok(true)
    }

    /// Registers a node known to be absent and within limits.
    fn register(&mut self, node: NodeId) {
        self.reserve_catalog_slot();
        self.nodes.insert(node, NodeRecord::active());
        // A retired engine slot for `node` is isolated and is reused as is.
        self.engine
            .add_node(node)
            .expect("an unlimited engine accepts any vertex");
        self.active_nodes += 1;
    }

    /// Makes room for one new catalog entry without growing a table whose
    /// headroom was consumed by tombstones (see the module documentation):
    /// when the next insert would grow the table and live entries are at most
    /// 90% of its full capacity, the catalog is rebuilt into a fresh table
    /// sized for its live entries (never larger than the old one). `O(V)` when
    /// it rebuilds, `O(1)` otherwise.
    fn reserve_catalog_slot(&mut self) {
        let capacity = self.nodes.capacity();
        self.catalog_full_capacity = self.catalog_full_capacity.max(capacity);
        let live = self.nodes.len();
        if capacity == live
            && live * REBUILD_LIVE_DENOMINATOR
                <= self.catalog_full_capacity * REBUILD_LIVE_NUMERATOR
        {
            let mut fresh: FxHashMap<NodeId, NodeRecord> =
                FxHashMap::with_capacity_and_hasher(live, Default::default());
            fresh.extend(self.nodes.drain());
            self.nodes = fresh;
            self.catalog_full_capacity = self.nodes.capacity();
        }
    }

    /// Registers the undirected edge `{source, target}` as enabled.
    ///
    /// Missing endpoints are registered as active nodes. The edge is
    /// effective only if both endpoints are active. Returns `Ok(false)` if
    /// the edge is already registered, enabled or disabled; a disabled edge
    /// stays disabled.
    ///
    /// # Errors
    ///
    /// In precedence order: [`GraphError::SelfLoop`];
    /// [`GraphError::NodeLimitExceeded`] when the missing endpoints do not
    /// fit; [`GraphError::EdgeLimitExceeded`] for a new edge at the enforced
    /// edge limit. Both carry the enforced value, which can be lower than the
    /// configured one (see [`Self::limits`]).
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let source_known = self.nodes.contains_key(&source);
        let target_known = self.nodes.contains_key(&target);
        if let Some(limit) = self.checks.max_nodes {
            let missing = usize::from(!source_known) + usize::from(!target_known);
            if missing > limit.saturating_sub(self.nodes.len()) {
                return Err(GraphError::NodeLimitExceeded { limit });
            }
        }
        let exists = source_known && target_known && self.nodes[&source].position(target).is_ok();
        if exists {
            return Ok(false);
        }
        if let Some(limit) = self.checks.max_edges
            && self.registered_edges >= limit
        {
            return Err(GraphError::EdgeLimitExceeded { limit });
        }
        if !source_known {
            self.register(source);
        }
        if !target_known {
            self.register(target);
        }
        self.insert_edge(source, target, true);
        Ok(true)
    }

    /// Inserts a validated, absent edge between registered distinct nodes.
    fn insert_edge(&mut self, source: NodeId, target: NodeId, enabled: bool) {
        for (owner, neighbor) in [(source, target), (target, source)] {
            let record = self.nodes.get_mut(&owner).expect("endpoint is registered");
            let index = record
                .position(neighbor)
                .expect_err("edge is absent at both endpoints");
            record
                .incident
                .insert(index, Incidence { neighbor, enabled });
        }
        self.registered_edges += 1;
        if enabled {
            self.enabled_edges += 1;
            if self.nodes[&source].active && self.nodes[&target].active {
                self.engine_link(source, target);
            }
        }
    }

    fn engine_link(&mut self, source: NodeId, target: NodeId) {
        let changed = self
            .engine
            .link(source, target)
            .expect("an unlimited engine links distinct vertices");
        debug_assert!(changed, "engine edge set equals the effective edge set");
        self.effective_edges += 1;
    }

    fn engine_cut(&mut self, source: NodeId, target: NodeId) {
        let changed = self
            .engine
            .cut(source, target)
            .expect("the engine cuts distinct vertices");
        debug_assert!(changed, "engine edge set equals the effective edge set");
        self.effective_edges -= 1;
    }

    /// Removes the registered edge `{source, target}`, whether or not it is
    /// enabled or effective. Both endpoints stay registered.
    ///
    /// Returns `Ok(false)` if the edge or an endpoint is not registered.
    ///
    /// # Errors
    ///
    /// [`GraphError::SelfLoop`] if `source == target`.
    pub fn cut(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let (Some(source_record), Some(target_record)) =
            (self.nodes.get(&source), self.nodes.get(&target))
        else {
            return Ok(false);
        };
        let Ok(index) = source_record.position(target) else {
            return Ok(false);
        };
        let both_active = source_record.active && target_record.active;
        let removed = self
            .nodes
            .get_mut(&source)
            .expect("source is registered")
            .remove_at(index);
        let target_record = self.nodes.get_mut(&target).expect("target is registered");
        let reverse = target_record
            .position(source)
            .expect("incidence is symmetric");
        target_record.remove_at(reverse);
        self.registered_edges -= 1;
        if removed.enabled {
            self.enabled_edges -= 1;
            if both_active {
                self.engine_cut(source, target);
            }
        }
        Ok(true)
    }

    /// Sets the `active` flag of a registered node.
    ///
    /// Incident registered edges and their `enabled` flags are preserved.
    /// Activation makes effective exactly the incident edges that are enabled
    /// and whose other endpoint is active. Returns `Ok(false)` if the flag
    /// already has this value.
    ///
    /// Costs `O(d)` for registered degree `d`, plus one engine update for
    /// each incident edge whose effectiveness changes.
    ///
    /// # Errors
    ///
    /// [`GraphError::UnknownNode`] (wrapped) if `node` is not registered.
    pub fn set_node_active(&mut self, node: NodeId, active: bool) -> Result<bool, LifecycleError> {
        let record = self
            .nodes
            .get(&node)
            .ok_or(GraphError::UnknownNode { node })?;
        if record.active == active {
            return Ok(false);
        }
        // Effective incident edges are visited in descending neighbor order
        // (see `remove_node` for why).
        let mut affected = Vec::new();
        for incidence in record.incident.iter().rev() {
            if incidence.enabled && self.nodes[&incidence.neighbor].active {
                affected.push(incidence.neighbor);
            }
        }
        for neighbor in affected {
            if active {
                self.engine_link(node, neighbor);
            } else {
                self.engine_cut(node, neighbor);
            }
        }
        self.nodes
            .get_mut(&node)
            .expect("node is registered")
            .active = active;
        if active {
            self.active_nodes += 1;
        } else {
            self.active_nodes -= 1;
        }
        Ok(true)
    }

    /// Sets the `enabled` flag of a registered edge, including while an
    /// endpoint is inactive; the flag then takes effect when both endpoints
    /// are active. Returns `Ok(false)` if the flag already has this value.
    ///
    /// # Errors
    ///
    /// In precedence order: [`GraphError::SelfLoop`];
    /// [`GraphError::UnknownNode`] for `source`, then for `target`;
    /// [`LifecycleError::UnknownEdge`] if both nodes are registered but the
    /// edge is not.
    pub fn set_edge_enabled(
        &mut self,
        source: NodeId,
        target: NodeId,
        enabled: bool,
    ) -> Result<bool, LifecycleError> {
        GraphError::check_distinct(source, target)?;
        let source_record = self
            .nodes
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let target_record = self
            .nodes
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        let index = source_record
            .position(target)
            .map_err(|_| LifecycleError::UnknownEdge { source, target })?;
        if source_record.incident[index].enabled == enabled {
            return Ok(false);
        }
        let both_active = source_record.active && target_record.active;
        let reverse = target_record
            .position(source)
            .expect("incidence is symmetric");
        self.nodes
            .get_mut(&source)
            .expect("source is registered")
            .incident[index]
            .enabled = enabled;
        self.nodes
            .get_mut(&target)
            .expect("target is registered")
            .incident[reverse]
            .enabled = enabled;
        if enabled {
            self.enabled_edges += 1;
            if both_active {
                self.engine_link(source, target);
            }
        } else {
            self.enabled_edges -= 1;
            if both_active {
                self.engine_cut(source, target);
            }
        }
        Ok(true)
    }

    /// Permanently removes `node` and every incident registered edge,
    /// including disabled edges and edges to inactive neighbors.
    ///
    /// Returns `false` if `node` is not registered. Registering the same ID
    /// later creates a fresh active isolated node.
    ///
    /// Costs `O(d + sum of neighbor degrees)` catalog work for registered
    /// degree `d`, one engine update per effective incident edge, and an
    /// amortized share of engine compaction.
    pub fn remove_node(&mut self, node: NodeId) -> bool {
        let Some(record) = self.nodes.remove(&node) else {
            return false;
        };
        // Descending neighbor order. For `compact-bfs`, the only engine with
        // lifecycle support, whose adjacency vectors are sorted, this removes
        // from the end of the node's vector, so each removal shifts no elements
        // instead of up to `d`. (Earlier candidates also measured this order on
        // the now-deferred `ett` and `hdt` lifecycle support.)
        for incidence in record.incident.iter().rev() {
            let neighbor = self
                .nodes
                .get_mut(&incidence.neighbor)
                .expect("neighbor is registered");
            let neighbor_active = neighbor.active;
            let reverse = neighbor.position(node).expect("incidence is symmetric");
            neighbor.remove_at(reverse);
            self.registered_edges -= 1;
            if incidence.enabled {
                self.enabled_edges -= 1;
                if record.active && neighbor_active {
                    self.engine_cut(node, incidence.neighbor);
                }
            }
        }
        if record.active {
            self.active_nodes -= 1;
        }
        // The retired slot is isolated now; release its adjacency allocation
        // so it retains only its ID-map entry and an empty vector header.
        self.engine.release_isolated_storage(node);
        self.compact_if_needed();
        true
    }

    /// Rebuilds the engine when retired slots exceed
    /// `max(registered nodes, MIN_RETIRED_BEFORE_COMPACTION)`, through
    /// [`Self::build_engine`]. The old engine is dropped after the new one is
    /// built, so both are live at the peak.
    fn compact_if_needed(&mut self) {
        let retired = self.engine.node_count() - self.nodes.len();
        if retired <= self.nodes.len().max(MIN_RETIRED_BEFORE_COMPACTION) {
            return;
        }
        let (engine, effective) = Self::build_engine(self.engine.engine(), &mut self.nodes, &[]);
        debug_assert_eq!(effective, self.effective_edges);
        self.engine = engine;
    }

    /// Returns whether `source` and `target` are connected in the effective
    /// graph.
    ///
    /// An inactive endpoint returns `Ok(false)`, including
    /// `connected(n, n)`. An active node is connected to itself.
    ///
    /// # Errors
    ///
    /// [`GraphError::UnknownNode`] for `source`, then for `target`, if not
    /// registered.
    pub fn connected(&self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        let source_record = self
            .nodes
            .get(&source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let target_record = self
            .nodes
            .get(&target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        if !source_record.active || !target_record.active {
            return Ok(false);
        }
        self.engine.connected(source, target)
    }

    /// Returns `Some(active)` for a registered node, `None` otherwise.
    #[must_use]
    pub fn node_active(&self, node: NodeId) -> Option<bool> {
        self.nodes.get(&node).map(|record| record.active)
    }

    /// Returns the number of registered edges incident to `node` (enabled or
    /// not), or `None` if it is not registered. Expected O(1).
    #[must_use]
    pub fn degree(&self, node: NodeId) -> Option<usize> {
        self.nodes.get(&node).map(|record| record.incident.len())
    }

    /// Iterates the registered edges incident to `node` in ascending neighbor
    /// ID, starting strictly after the neighbor `after` (or from the start),
    /// with each edge's flags. Returns `None` if `node` is not registered.
    ///
    /// `after` is a pagination cursor: it need not be registered or still
    /// linked, so every edge that stays registered while a caller pages through
    /// the list is visited exactly once; edges registered or removed meanwhile
    /// may or may not be visited.
    ///
    /// Positioning is O(log deg(node)); each step costs one expected-O(1)
    /// catalog lookup of the neighbor (for `effective`).
    ///
    /// ```rust
    /// use knotrel_core::{EngineConfig, LifecycleGraph};
    ///
    /// let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
    /// graph.link(5, 9)?;
    /// graph.link(5, 1)?;
    /// graph.set_node_active(9, false)?;
    /// let edges: Vec<_> = graph.incident_after(5, None).unwrap().collect();
    /// assert_eq!(edges.len(), 2);
    /// assert_eq!(edges[0].0, 1); // ascending neighbor ID
    /// assert!(edges[0].1.effective);
    /// assert!(edges[1].1.enabled && !edges[1].1.effective); // 9 is inactive
    /// assert_eq!(graph.incident_after(5, Some(1)).unwrap().count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn incident_after(
        &self,
        node: NodeId,
        after: Option<NodeId>,
    ) -> Option<impl Iterator<Item = (NodeId, EdgeState)> + '_> {
        let record = self.nodes.get(&node)?;
        let start = after.map_or(0, |cursor| {
            record
                .incident
                .partition_point(|incidence| incidence.neighbor <= cursor)
        });
        Some(record.incident[start..].iter().map(move |incidence| {
            let neighbor_active = self.nodes[&incidence.neighbor].active;
            (
                incidence.neighbor,
                EdgeState {
                    enabled: incidence.enabled,
                    effective: incidence.enabled && record.active && neighbor_active,
                },
            )
        }))
    }

    /// Returns the flags of the registered edge `{source, target}`, or `None`
    /// if it is not registered. A self-loop pair is never registered.
    #[must_use]
    pub fn edge_state(&self, source: NodeId, target: NodeId) -> Option<EdgeState> {
        let source_record = self.nodes.get(&source)?;
        let index = source_record.position(target).ok()?;
        let enabled = source_record.incident[index].enabled;
        let effective = enabled && source_record.active && self.nodes[&target].active;
        Some(EdgeState { enabled, effective })
    }

    /// Returns the number of registered nodes, active or inactive.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the number of registered edges, enabled or disabled.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.registered_edges
    }

    /// Returns the number of registered nodes whose flag is active.
    #[must_use]
    pub fn active_node_count(&self) -> usize {
        self.active_nodes
    }

    /// Returns the number of registered edges whose flag is enabled.
    #[must_use]
    pub fn enabled_edge_count(&self) -> usize {
        self.enabled_edges
    }

    /// Returns the number of effective edges: enabled, with both endpoints
    /// active.
    #[must_use]
    pub fn effective_edge_count(&self) -> usize {
        self.effective_edges
    }

    /// Returns the number of vertices held by the inner engine: every
    /// registered node plus retired slots of removed nodes not yet compacted.
    ///
    /// It is a storage diagnostic, bounded by `2 × node_count() + 1024`.
    #[must_use]
    pub fn engine_vertex_count(&self) -> usize {
        self.engine.node_count()
    }

    /// Returns storage diagnostics: logical sizes and retained capacities.
    ///
    /// Costs `O(V)` time (one pass over the catalog) and no allocation, and
    /// changes no state. [`LifecycleStorage`] lists which properties of the
    /// values are guaranteed and which are implementation-dependent.
    #[must_use]
    pub fn storage_stats(&self) -> LifecycleStorage {
        let (incident_entries, incident_capacity) =
            self.nodes.values().fold((0, 0), |(len, cap), record| {
                (
                    len + record.incident.len(),
                    cap + record.incident.capacity(),
                )
            });
        let engine_vertices = self.engine.node_count();
        LifecycleStorage {
            engine_vertices,
            retired_vertices: engine_vertices - self.nodes.len(),
            catalog_nodes: self.nodes.len(),
            catalog_node_capacity: self.nodes.capacity(),
            incident_entries,
            incident_capacity,
        }
    }

    /// Verifies every internal invariant; panics on violation. Test support.
    #[cfg(test)]
    pub(crate) fn validate(&self) {
        let mut registered = 0;
        let mut enabled = 0;
        let mut effective = 0;
        let mut active = 0;
        for (&node, record) in &self.nodes {
            active += usize::from(record.active);
            assert!(self.engine.contains_node(node));
            for pair in record.incident.windows(2) {
                assert!(pair[0].neighbor < pair[1].neighbor);
            }
            for incidence in &record.incident {
                assert_ne!(incidence.neighbor, node);
                let other = &self.nodes[&incidence.neighbor];
                let back = other.position(node).expect("symmetric");
                assert_eq!(other.incident[back].enabled, incidence.enabled);
                if node < incidence.neighbor {
                    registered += 1;
                    enabled += usize::from(incidence.enabled);
                    if incidence.enabled && record.active && other.active {
                        effective += 1;
                    }
                }
            }
        }
        assert_eq!(registered, self.registered_edges);
        assert_eq!(enabled, self.enabled_edges);
        assert_eq!(effective, self.effective_edges);
        assert_eq!(active, self.active_nodes);
        assert_eq!(effective, self.engine.edge_count());
        assert!(self.engine.node_count() <= 2 * self.nodes.len() + MIN_RETIRED_BEFORE_COMPACTION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `incident_after` pages in ascending neighbor ID, reports flags, and keeps
    /// working with a cursor whose neighbor was removed.
    #[test]
    fn incident_pages_follow_neighbor_ids_and_survive_removal() {
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for neighbor in [30, 10, 20, u64::MAX] {
            graph.link(5, neighbor).unwrap();
        }
        graph.set_edge_enabled(5, 20, false).unwrap();
        graph.set_node_active(u64::MAX, false).unwrap();
        let first: Vec<_> = graph.incident_after(5, None).unwrap().take(2).collect();
        assert_eq!(first.iter().map(|e| e.0).collect::<Vec<_>>(), [10, 20]);
        assert!(first[0].1.effective);
        assert!(!first[1].1.enabled && !first[1].1.effective);
        assert!(graph.remove_node(20));
        let rest: Vec<_> = graph.incident_after(5, Some(20)).unwrap().collect();
        assert_eq!(rest.iter().map(|e| e.0).collect::<Vec<_>>(), [30, u64::MAX]);
        assert!(rest[1].1.enabled && !rest[1].1.effective);
        assert_eq!(graph.degree(5), Some(3));
        assert_eq!(graph.degree(20), None);
        assert!(graph.incident_after(20, None).is_none());
    }

    /// `Error::source` exposes only wrapped graph errors.
    #[test]
    fn lifecycle_error_source_wraps_only_graph_errors() {
        let graph = LifecycleError::Graph(GraphError::SelfLoop { node: 1 });
        assert!(graph.source().is_some());
        for other in [
            LifecycleError::UnknownEdge {
                source: 1,
                target: 2,
            },
            LifecycleError::ConflictingNode { node: 1 },
            LifecycleError::UnsupportedEngine {
                engine: EngineConfig::Hdt,
            },
        ] {
            assert!(other.source().is_none(), "{other}");
        }
    }

    /// Incident vectors shrink after most incidences are cut; a link to an
    /// inactive endpoint is registered without an engine edge; compaction
    /// skips inactive nodes' incidences.
    #[test]
    fn shrink_inactive_links_and_compaction_with_inactive_nodes() {
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for neighbor in 1..=64 {
            graph.link(0, neighbor).unwrap();
        }
        let before = graph.nodes[&0].incident.capacity();
        for neighbor in 1..=60 {
            graph.cut(0, neighbor).unwrap();
        }
        assert!(graph.nodes[&0].incident.capacity() < before);
        graph.validate();

        graph.add_node(200).unwrap();
        graph.set_node_active(200, false).unwrap();
        assert_eq!(graph.link(0, 200), Ok(true));
        assert_eq!(graph.edge_state(0, 200).map(|s| s.effective), Some(false));
        graph.validate();

        // More than 1,024 removals while node 200 (with an incident edge)
        // stays inactive trigger a compaction that must skip it.
        for node in 1_000..2_200 {
            graph.link(node, node + 10_000).unwrap();
        }
        let vertices = graph.engine_vertex_count();
        for node in 1_000..2_200 {
            graph.remove_node(node);
            graph.remove_node(node + 10_000);
        }
        assert!(graph.engine_vertex_count() < vertices, "a compaction ran");
        assert_eq!(graph.node_active(200), Some(false));
        assert_eq!(graph.edge_state(0, 200).map(|s| s.effective), Some(false));
        graph.validate();
    }

    /// A removed node's retired engine slot keeps no adjacency allocation,
    /// whatever its former degree, and its same-ID reuse starts empty.
    #[test]
    fn removal_releases_retired_slot_adjacency() {
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for leaf in 1..=300 {
            graph.link(0, leaf).unwrap();
        }
        assert!(graph.engine.adjacency_capacity(0).unwrap() >= 300);
        assert!(graph.remove_node(0));
        assert_eq!(graph.engine.adjacency_capacity(0), Some(0), "retired slot");
        assert_eq!(graph.storage_stats().retired_vertices, 1);
        assert!(graph.add_node(0).unwrap());
        assert_eq!(graph.engine.adjacency_capacity(0), Some(0));
        assert!(!graph.connected(0, 1).unwrap());
        graph.validate();
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn node_record_degree_hint_fits_in_padding() {
        assert_eq!(size_of::<NodeRecord>(), 32);
    }

    /// The fast catalog path agrees with the exact path: identical catalogs
    /// and registration order whenever it succeeds, and `None` on every input
    /// the exact path rejects. Inputs mix duplicates (same and reversed
    /// endpoints, same and different flags), self-loops and tight limits.
    #[test]
    fn fast_catalog_matches_exact_catalog() {
        let mut seed = 0x51_u64;
        let mut next = move |bound: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % bound
        };
        let mut fast_successes = 0;
        let mut exact_errors = 0;
        let mut fallback_successes = 0;
        for case in 0..3_000 {
            let n = 2 + next(60);
            let nodes: Vec<NodeSpec> = (0..next(10))
                .map(|_| NodeSpec {
                    node: next(n),
                    active: next(12) != 0,
                })
                .collect();
            let mut edges: Vec<EdgeSpec> = (0..next(25))
                .map(|_| EdgeSpec {
                    source: next(n),
                    target: next(n),
                    enabled: next(12) != 0,
                })
                .collect();
            for _ in 0..next(4) {
                // Repeated edges, often reversed, sometimes with a flipped flag.
                if let Some(&e) = edges.get(next(edges.len().max(1) as u64) as usize) {
                    let flip = next(5) == 0;
                    edges.push(EdgeSpec {
                        source: e.target,
                        target: e.source,
                        enabled: e.enabled != flip,
                    });
                }
            }
            let limits = GraphLimits {
                max_nodes: (case % 3 == 0).then(|| next(14) as usize),
                max_edges: (case % 4 == 0).then(|| next(26) as usize),
            };
            let exact = LifecycleGraph::catalog_exact(limits, &nodes, &edges);
            let fast = LifecycleGraph::catalog_fast(limits, &nodes, &edges);
            match (&exact, fast) {
                (Ok((exact_catalog, exact_order)), Some((fast_catalog, fast_order))) => {
                    fast_successes += 1;
                    assert_eq!(exact_order, &fast_order, "case {case}");
                    assert_eq!(exact_catalog.len(), fast_catalog.len());
                    for (node, record) in exact_catalog {
                        let other = &fast_catalog[node];
                        assert_eq!(record.active, other.active);
                        let pairs = |r: &NodeRecord| {
                            r.incident
                                .iter()
                                .map(|i| (i.neighbor, i.enabled))
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(pairs(record), pairs(other), "case {case}");
                    }
                }
                (Err(_), None) => exact_errors += 1,
                (Ok(_), None) => fallback_successes += 1,
                (Err(error), Some(_)) => panic!("case {case}: fast path accepted {error:?}"),
            }
        }
        assert!(fast_successes > 600, "{fast_successes}");
        assert!(exact_errors > 300, "{exact_errors}");
        // Only an edge-limit pre-check with duplicates may defer a success.
        assert!(
            fallback_successes < fast_successes / 10,
            "{fallback_successes}"
        );
    }

    /// Bulk engine construction agrees with incremental construction on
    /// random flagged catalogs: same vertex and edge counts, the same
    /// connectivity on every pair, and every internal invariant. Compaction
    /// uses the same builder, so a compacted graph is checked as well.
    #[test]
    fn bulk_engine_build_matches_incremental_build() {
        let mut seed = 0x9e37_79b9_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for case in 0..400 {
            // IDs are spread over a wide range and inserted out of order, so
            // ascending-ID indices differ from registration order.
            let n = 2 + next(40);
            let id = |k: u64| k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (case % 40);
            let nodes: Vec<NodeSpec> = (0..next(n))
                .map(|_| NodeSpec {
                    node: id(next(n)),
                    active: next(3) != 0,
                })
                .collect();
            let mut edges = Vec::new();
            for _ in 0..next(3 * n) {
                let (source, target) = (id(next(n)), id(next(n)));
                let enabled = next(4) != 0;
                if source != target {
                    edges.push(EdgeSpec {
                        source,
                        target,
                        enabled,
                    });
                }
            }
            let Ok((mut catalog, order)) =
                LifecycleGraph::catalog_exact(GraphLimits::default(), &nodes, &edges)
            else {
                continue; // conflicting repeated flags
            };
            let (incremental, incremental_edges) = LifecycleGraph::build_engine_incremental(
                EngineConfig::CompactBfs,
                &catalog,
                &order,
            );
            let (bulk, bulk_edges) =
                LifecycleGraph::build_engine(EngineConfig::CompactBfs, &mut catalog, &order);
            assert_eq!(bulk_edges, incremental_edges, "case {case}");
            assert_eq!(bulk.edge_count(), incremental.edge_count(), "case {case}");
            assert_eq!(bulk.node_count(), incremental.node_count(), "case {case}");
            assert!(catalog.values().all(|record| record.pending == 0));
            for &a in &order {
                for &b in &order {
                    assert_eq!(
                        bulk.connected(a, b),
                        incremental.connected(a, b),
                        "case {case}: {a}-{b}"
                    );
                }
            }
            let graph = LifecycleGraph::finish(
                EngineConfig::CompactBfs,
                GraphLimits::default(),
                catalog,
                order,
            );
            graph.validate();
        }
    }

    /// The incremental fallback of `build_engine` (non-`compact-bfs`
    /// engines) registers every catalog node, whether it receives the full
    /// insertion order or the empty slice that compaction passes, and agrees
    /// with the one-pass `compact-bfs` build on edges and connectivity.
    #[test]
    fn fallback_engine_build_registers_every_catalog_node() {
        let nodes: Vec<NodeSpec> = [(9, true), (3, false), (7, true), (1, true), (5, true)]
            .into_iter()
            .map(|(node, active)| NodeSpec { node, active })
            .collect();
        let edges: Vec<EdgeSpec> = [(9, 7, true), (7, 1, true), (3, 9, true), (5, 1, false)]
            .into_iter()
            .map(|(source, target, enabled)| EdgeSpec {
                source,
                target,
                enabled,
            })
            .collect();
        let (mut catalog, order) =
            LifecycleGraph::catalog_exact(GraphLimits::default(), &nodes, &edges).unwrap();
        let (compact, compact_edges) =
            LifecycleGraph::build_engine(EngineConfig::CompactBfs, &mut catalog, &order);
        for given in [&order[..], &[][..]] {
            let (fallback, fallback_edges) =
                LifecycleGraph::build_engine(EngineConfig::EulerTour, &mut catalog, given);
            assert_eq!(fallback.engine(), EngineConfig::EulerTour);
            assert_eq!(fallback.node_count(), catalog.len());
            assert_eq!(fallback_edges, compact_edges);
            assert_eq!(fallback.edge_count(), compact.edge_count());
            for &a in &order {
                for &b in &order {
                    assert_eq!(fallback.connected(a, b), compact.connected(a, b), "{a}-{b}");
                }
            }
        }
    }

    /// Random mixed operations, including high-degree hubs, keep every
    /// internal invariant (catalog symmetry, engine edge set, counters).
    #[test]
    fn random_operations_preserve_internal_invariants() {
        // `ett` and `hdt` lifecycle support is deferred (rejected by the constructors).
        for engine in [EngineConfig::CompactBfs] {
            let mut state = 0x1234_5678_u64;
            let mut next = move |bound: u64| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % bound
            };
            let mut graph = LifecycleGraph::new(engine).unwrap();
            for _ in 0..4_000 {
                // Node 0 is a hub that most edges touch.
                let a = if next(3) == 0 { 0 } else { next(40) };
                let b = next(40);
                let flag = next(2) == 0;
                match next(9) {
                    0..=2 => drop(graph.link(a, b)),
                    3 => drop(graph.cut(a, b)),
                    4 => drop(graph.set_node_active(a, flag)),
                    5 | 6 => drop(graph.set_edge_enabled(a, b, flag)),
                    7 => drop(graph.remove_node(a)),
                    _ => drop(graph.add_node(a)),
                }
                graph.validate();
            }
        }
    }

    #[test]
    fn compaction_bounds_retired_slots_and_preserves_connectivity() {
        // `ett` and `hdt` lifecycle support is deferred (rejected by the constructors).
        for engine in [EngineConfig::CompactBfs] {
            let mut graph = LifecycleGraph::new(engine).unwrap();
            graph.link(0, 1).unwrap();
            graph.link(1, 2).unwrap();
            graph.set_edge_enabled(1, 2, false).unwrap();
            for round in 0..3_000_u64 {
                let node = 10 + round;
                graph.link(2, node).unwrap();
                assert!(graph.remove_node(node));
                graph.validate();
            }
            assert!(graph.engine_vertex_count() <= 2 * graph.node_count() + 1024);
            let storage = graph.storage_stats();
            assert_eq!(storage.catalog_nodes, graph.node_count());
            assert_eq!(storage.incident_entries, 2 * graph.edge_count());
            assert!(storage.incident_capacity >= storage.incident_entries);
            assert_eq!(
                storage.engine_vertices,
                storage.catalog_nodes + storage.retired_vertices
            );
            assert!(graph.connected(0, 1).unwrap());
            assert!(!graph.connected(0, 2).unwrap());
            assert_eq!(
                graph.edge_state(1, 2).map(|state| state.enabled),
                Some(false)
            );
        }
    }

    /// Fresh-ID churn at constant size: tombstones never make the catalog grow;
    /// rebuilds happen and keep every entry, flag, incidence and connectivity.
    #[test]
    fn fresh_id_churn_rebuilds_the_catalog_instead_of_growing_it() {
        // 3,200 entries fill a 4,096-bucket table (capacity 3,584) to 78% of its buckets
        // and 89% of its capacity, as at 100,000 nodes (131,072 buckets: 76%, 87%):
        // removals leave tombstones, and live entries are within the 90% rebuild bound. A
        // lightly loaded table mostly frees slots outright and never needs a rebuild; above
        // 90% of capacity the table grows, as before.
        const N: u64 = 3_200;
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for v in 0..N {
            graph.add_node(v).unwrap();
        }
        for v in 1..N {
            graph.link(0, v).unwrap();
        }
        graph.set_node_active(N - 1, false).unwrap();
        let full = graph.nodes.capacity();
        assert_eq!(graph.catalog_full_capacity, full);
        // Node 0 (the hub) and node N - 1 (inactive) stay; the others are churned.
        let mut live: std::collections::VecDeque<u64> = (1..N - 1).collect();
        let mut rebuilds = 0;
        for round in 0..(8 * N) {
            let before = graph.nodes.capacity();
            let old = live.pop_front().unwrap();
            assert!(graph.remove_node(old));
            let fresh = 10 * N + round;
            graph.link(0, fresh).unwrap();
            live.push_back(fresh);
            let after = graph.nodes.capacity();
            assert!(after <= full, "round {round}: the catalog grew to {after}");
            // A rebuild restores the headroom that tombstones had consumed: a jump
            // of more than one (a plain insert into a tombstone slot adds at most one).
            if after > before + 1 {
                rebuilds += 1;
            }
            if round % 1_024 == 0 {
                graph.validate();
            }
        }
        graph.validate();
        assert!(rebuilds > 0, "the churn produced no tombstone rebuild");
        assert_eq!(graph.catalog_full_capacity, full);
        assert_eq!(graph.node_count(), N as usize);
        assert_eq!(graph.node_active(N - 1), Some(false));
        assert!(graph.connected(0, *live.back().unwrap()).unwrap());
        assert!(!graph.connected(0, N - 1).unwrap());
    }

    /// Growing the number of live entries still grows the catalog.
    #[test]
    fn live_growth_still_grows_the_catalog() {
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for v in 0..1_000 {
            graph.add_node(v).unwrap();
        }
        let first = graph.nodes.capacity();
        for v in 1_000..10_000 {
            graph.add_node(v).unwrap();
        }
        assert!(graph.nodes.capacity() > first);
        assert!(graph.catalog_full_capacity >= graph.nodes.capacity());
        graph.validate();
    }
}
