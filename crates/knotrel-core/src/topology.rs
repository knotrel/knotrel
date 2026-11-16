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
//! When retired slots exceed `max(registered nodes, 1024)`, the engine is
//! rebuilt from the catalog. A rebuild performs `O(V + E_eff)` engine
//! insertions; more than `max(V, 1024)` removals separate two rebuilds, so the
//! amortized cost is `O(1 + E_eff / V)` engine insertions per removal, and
//! engine vertices stay below `2 V + 1024`.

use crate::{ConnectivityGraph, EngineConfig, GraphError, GraphLimits, NodeId};
use rustc_hash::FxHashMap;
use std::{error::Error, fmt};

/// Minimum number of retired engine slots before a compaction is considered.
const MIN_RETIRED_BEFORE_COMPACTION: usize = 1024;

/// Smallest incident-vector capacity that is ever shrunk.
const MIN_SHRINK_CAPACITY: usize = 16;

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
pub struct EdgeState {
    /// The edge's own flag, independent of its endpoints.
    pub enabled: bool,
    /// Whether the edge participates in connectivity: enabled, with both
    /// endpoints active.
    pub effective: bool,
}

/// Storage diagnostics of a [`LifecycleGraph`], from
/// [`LifecycleGraph::storage_stats`].
///
/// Logical sizes and retained capacities are reported separately: vectors
/// and hash maps keep capacity after removals, so capacity can exceed the
/// logical size. Capacities count elements, not bytes, and exclude allocator
/// overhead and the inner engine's own capacities.
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
    /// Incident registered edges, strictly sorted by `neighbor`.
    incident: Vec<Incidence>,
}

impl NodeRecord {
    fn active() -> Self {
        Self {
            active: true,
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
/// let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs);
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
    limits: GraphLimits,
    active_nodes: usize,
    registered_edges: usize,
    enabled_edges: usize,
    effective_edges: usize,
}

impl LifecycleGraph {
    /// Creates an empty unlimited graph using `engine` for connectivity.
    #[must_use]
    pub fn new(engine: EngineConfig) -> Self {
        Self::with_limits(engine, GraphLimits::default())
    }

    /// Creates an empty graph with cardinality limits on registered topology.
    #[must_use]
    pub fn with_limits(engine: EngineConfig, limits: GraphLimits) -> Self {
        Self {
            nodes: FxHashMap::default(),
            engine: ConnectivityGraph::new(engine),
            limits,
            active_nodes: 0,
            registered_edges: 0,
            enabled_edges: 0,
            effective_edges: 0,
        }
    }

    /// Builds a graph from complete registered topology.
    ///
    /// Registers `nodes` in order, then `edges` in order, creating missing
    /// edge endpoints as active nodes. Repeated entries with identical flags
    /// are accepted once; edges compare as unordered pairs.
    ///
    /// The catalog is assembled first and incident vectors are sorted once,
    /// `O((V + E) log E)`; the engine then receives the active nodes'
    /// effective edges through [`ConnectivityGraph::try_from_edges`].
    ///
    /// # Errors
    ///
    /// Processing stops at the first failing entry, in input order:
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
                    incident: Vec::new(),
                },
            );
            order.push(spec.node);
        }

        let mut seen: FxHashMap<(NodeId, NodeId), bool> = FxHashMap::default();
        seen.reserve(edges.len());
        let mut enabled_edges = 0;
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
            enabled_edges += usize::from(spec.enabled);
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

        let mut effective: Vec<(NodeId, NodeId)> = Vec::new();
        let mut active_nodes = 0;
        let mut registered_edges = 0;
        for &node in &order {
            let record = catalog.get_mut(&node).expect("ordered node is registered");
            record
                .incident
                .sort_unstable_by_key(|incidence| incidence.neighbor);
            record.incident.shrink_to_fit();
            registered_edges += record.incident.len();
            active_nodes += usize::from(record.active);
        }
        for &node in &order {
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
        let engine_graph =
            ConnectivityGraph::try_from_edges(engine, GraphLimits::default(), &order, &effective)
                .expect("an unlimited engine accepts a validated simple graph");
        Ok(Self {
            nodes: catalog,
            engine: engine_graph,
            limits,
            active_nodes,
            registered_edges: registered_edges / 2,
            enabled_edges,
            effective_edges: effective.len(),
        })
    }

    /// Returns the configured limits on registered topology.
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
    /// exceed `max_nodes`.
    pub fn add_node(&mut self, node: NodeId) -> Result<bool, GraphError> {
        if self.nodes.contains_key(&node) {
            return Ok(false);
        }
        if let Some(limit) = self.limits.max_nodes
            && self.nodes.len() >= limit
        {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        self.register(node);
        Ok(true)
    }

    /// Registers a node known to be absent and within limits.
    fn register(&mut self, node: NodeId) {
        self.nodes.insert(node, NodeRecord::active());
        // A retired engine slot for `node` is isolated and is reused as is.
        self.engine
            .add_node(node)
            .expect("an unlimited engine accepts any vertex");
        self.active_nodes += 1;
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
    /// fit; [`GraphError::EdgeLimitExceeded`] for a new edge at `max_edges`.
    pub fn link(&mut self, source: NodeId, target: NodeId) -> Result<bool, GraphError> {
        GraphError::check_distinct(source, target)?;
        let source_known = self.nodes.contains_key(&source);
        let target_known = self.nodes.contains_key(&target);
        if let Some(limit) = self.limits.max_nodes {
            let missing = usize::from(!source_known) + usize::from(!target_known);
            if missing > limit.saturating_sub(self.nodes.len()) {
                return Err(GraphError::NodeLimitExceeded { limit });
            }
        }
        let exists = source_known && target_known && self.nodes[&source].position(target).is_ok();
        if exists {
            return Ok(false);
        }
        if let Some(limit) = self.limits.max_edges
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
        // Descending neighbor order. For `compact-bfs`, whose adjacency vectors
        // are sorted, this removes from the end of the node's vector, so each
        // removal shifts no elements instead of up to `d`. For `ett` and `hdt`
        // the v0.5.0 qualification measured large reductions with this order
        // on high-degree nodes (for example 7.6x slower than an unordered
        // application-side equivalent before, 0.7x after, ETT degree 4,096);
        // the order is a measured choice there, not a proven bound.
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
        self.compact_if_needed();
        true
    }

    /// Rebuilds the engine when retired slots exceed
    /// `max(registered nodes, MIN_RETIRED_BEFORE_COMPACTION)`.
    fn compact_if_needed(&mut self) {
        let retired = self.engine.node_count() - self.nodes.len();
        if retired <= self.nodes.len().max(MIN_RETIRED_BEFORE_COMPACTION) {
            return;
        }
        let order: Vec<NodeId> = self.nodes.keys().copied().collect();
        let mut effective = Vec::with_capacity(self.effective_edges);
        for (&node, record) in &self.nodes {
            if !record.active {
                continue;
            }
            for incidence in &record.incident {
                if node < incidence.neighbor
                    && incidence.enabled
                    && self.nodes[&incidence.neighbor].active
                {
                    effective.push((node, incidence.neighbor));
                }
            }
        }
        debug_assert_eq!(effective.len(), self.effective_edges);
        self.engine = ConnectivityGraph::try_from_edges(
            self.engine.engine(),
            GraphLimits::default(),
            &order,
            &effective,
        )
        .expect("an unlimited engine accepts a validated simple graph");
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
    /// Costs `O(V)` time (one pass over the catalog) and no allocation.
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

    /// Random mixed operations, including high-degree hubs, keep every
    /// internal invariant (catalog symmetry, engine edge set, counters).
    #[test]
    fn random_operations_preserve_internal_invariants() {
        for engine in [
            EngineConfig::CompactBfs,
            EngineConfig::EulerTour,
            EngineConfig::Hdt,
        ] {
            let mut state = 0x1234_5678_u64;
            let mut next = move |bound: u64| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % bound
            };
            let mut graph = LifecycleGraph::new(engine);
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
        for engine in [
            EngineConfig::CompactBfs,
            EngineConfig::EulerTour,
            EngineConfig::Hdt,
        ] {
            let mut graph = LifecycleGraph::new(engine);
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
}
