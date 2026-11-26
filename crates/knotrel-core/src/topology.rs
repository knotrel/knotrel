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
//!    flag `f`; incident vectors are strictly sorted by the neighbors'
//!    external IDs and never contain the owner itself.
//! 2. Every registered node is an engine vertex. Engine vertices that are not
//!    registered are *retired* slots: isolated, with no incident engine edge.
//! 3. The engine's edge set equals the effective edge set.
//! 4. The maintained counters equal the counts they name.
//!
//! With the catalog layout below, also:
//!
//! - **I5 (occupied targets).** Every stored incidence names an occupied
//!   catalog slot, and that slot's `id` is the neighbor's external ID.
//! - **I6 (map).** The ID map maps `x` to `i` iff slot `i` is occupied with
//!   `id == x`.
//! - **I7 (closure).** Catalog slot indices are stored only in incidences, in
//!   ID-map values and in the free list (which names only vacant slots). No
//!   index is returned to callers, stored by the engine, or kept across
//!   calls: the iterator of [`LifecycleGraph::incident_after`] borrows the
//!   graph, so no mutation can happen while it lives.
//!
//! # Catalog layout
//!
//! The catalog (design: `docs/design/compact-node-index.md`, option B64) is
//! an ID map `FxHashMap<NodeId, usize>` into a dense slot vector of 40-byte
//! records (on 64-bit targets), each holding the node's external ID, its
//! incident vector, a construction scratch word and a state (vacant,
//! inactive or active). An incidence is one `usize`: the neighbor's slot
//! index in the low `usize::BITS - 1` bits and the edge's `enabled` flag in
//! the top bit. Incident vectors stay sorted by the neighbor's *external* ID,
//! so every binary search translates the probed slot to its ID, and a
//! pagination cursor (an external ID) is resolved by a lower bound on IDs
//! without being looked up itself.
//!
//! A removed node's slot is vacated only after every incidence naming it was
//! removed (symmetry, invariant 1), then pushed on an intrusive LIFO free
//! list threaded through the vacant slots' `id` fields; the next registration
//! reuses it, and no stored index can still name it (I5, I7). When vacant
//! slots exceed `max(V, 1024)`, the rule of engine compaction below, the slot
//! vector is compacted: occupied slots are renumbered densely in ascending
//! external ID through one bijection that rewrites every incidence and every
//! ID-map value, in `O(V log V + E)`. More than `max(V, 1024)` removals
//! separate two slot compactions, so the amortized cost is
//! `O(log V + E / V)` per removal. Remapping keeps every incidence at its
//! position with the same external ID, so cursors resolve to the same
//! position before and after. Bulk import also assigns slots in ascending
//! external ID. A new slot reserves the slot vector up to the ID map's
//! capacity, so that both grow in step.
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
//! Removing a node from the catalog's ID map can leave a tombstone, which
//! uses insert headroom without holding an entry. Under fresh-ID churn the
//! headroom runs out while live entries stay constant, and the table would
//! double although it is far from full (at 100,000 nodes, from 131,072 to
//! 262,144 buckets within a few percent of churn). Before an insert that would
//! grow the table, the ID map is instead rebuilt into a fresh table sized for
//! its live entries when they are at most 90% of the table's full capacity
//! (its capacity right after its last growth or rebuild): entries move, no
//! slot record or incident vector is touched, and the tombstones are dropped. A table sized
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
                 supported engines: compact-bfs, cluster-forest",
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

/// Bit of a packed [`Incidence`] holding the edge's `enabled` flag: the top
/// bit of `usize`. The slot index occupies the other `usize::BITS - 1` bits.
const ENABLED_BIT: usize = 1 << (usize::BITS - 1);

/// Free-list terminator: stored in [`Catalog::free_head`] when no slot is
/// vacant and in the link of the last vacant slot. Never a valid slot index,
/// since a slot vector holds at most `isize::MAX / size_of::<NodeRecord>()`
/// records.
const NO_SLOT: usize = usize::MAX;

/// One incident registered edge, stored at an endpoint (B64 layout of
/// `docs/design/compact-node-index.md`, section 3.1).
///
/// The neighbor's catalog slot index is packed into the low
/// `usize::BITS - 1` bits and the edge's `enabled` flag into the top bit:
/// 8 bytes on 64-bit targets. A slot index always fits, because a
/// `Vec<NodeRecord>` holds at most `isize::MAX / size_of::<NodeRecord>()`
/// records, which needs at most 58 bits on 64-bit targets and 26 on 32-bit
/// ones. Incident lists stay sorted by the neighbor's **external** ID, read
/// from the neighbor's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Incidence(usize);

impl Incidence {
    fn new(slot: usize, enabled: bool) -> Self {
        debug_assert_eq!(slot & ENABLED_BIT, 0, "slot index below the flag bit");
        Self(slot | if enabled { ENABLED_BIT } else { 0 })
    }

    /// The neighbor's catalog slot.
    fn slot(self) -> usize {
        self.0 & !ENABLED_BIT
    }

    /// The edge's `enabled` flag.
    fn enabled(self) -> bool {
        self.0 & ENABLED_BIT != 0
    }

    fn set_enabled(&mut self, enabled: bool) {
        *self = Self::new(self.slot(), enabled);
    }

    fn set_slot(&mut self, slot: usize) {
        *self = Self::new(slot, self.enabled());
    }
}

/// State of a catalog slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// On the free list; holds no node.
    Vacant,
    /// Holds a registered inactive node.
    Inactive,
    /// Holds a registered active node.
    Active,
}

impl SlotState {
    fn registered(active: bool) -> Self {
        if active { Self::Active } else { Self::Inactive }
    }
}

/// One catalog slot: a registered node, or a vacant entry of the free list.
#[derive(Debug)]
struct NodeRecord {
    /// The external ID of an occupied slot. For a vacant slot, the index of
    /// the next vacant slot (`NO_SLOT` ends the list), stored as a `NodeId`
    /// (lossless: slot indices fit in 58 bits).
    id: NodeId,
    /// Incident registered edges, strictly sorted by the neighbor's external
    /// ID. Empty, without allocation, in a vacant slot.
    incident: Vec<Incidence>,
    /// Construction-only scratch, zero between public calls: the node's
    /// engine index during [`LifecycleGraph::build_engine`]. It occupies the
    /// record's padding, so the record stays 40 bytes on 64-bit targets.
    pending: u32,
    state: SlotState,
}

impl NodeRecord {
    fn registered(id: NodeId, active: bool) -> Self {
        Self {
            id,
            incident: Vec::new(),
            pending: 0,
            state: SlotState::registered(active),
        }
    }

    fn is_active(&self) -> bool {
        self.state == SlotState::Active
    }

    fn is_occupied(&self) -> bool {
        self.state != SlotState::Vacant
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

/// The lifecycle catalog: an ID map into a dense slot vector (see the module
/// documentation, "Catalog layout", for the invariants I5 to I7).
#[derive(Debug)]
struct Catalog {
    // `FxHashMap`, not the identity-hashed `IntMap`: with hashbrown, identity
    // hashes of small IDs or IDs sharing their high bits collide in the 7-bit
    // control tag and degrade probing (measured in the v0.5.0 qualification).
    /// External ID → slot index of every registered node (I6).
    ids: FxHashMap<NodeId, usize>,
    /// Slot records, occupied and vacant, indexed by slot.
    slots: Vec<NodeRecord>,
    /// First vacant slot of the intrusive LIFO free list, or `NO_SLOT`.
    free_head: usize,
    /// Number of vacant slots, all of them on the free list.
    vacant: usize,
    /// The ID map's `capacity()` right after its last growth or rebuild;
    /// tombstones lower `capacity()` below it.
    full_capacity: usize,
    /// Slot compactions run so far. Test support.
    #[cfg(test)]
    slot_compactions: usize,
    /// Registrations that reused a vacant slot so far. Test support.
    #[cfg(test)]
    slot_reuses: usize,
}

impl Catalog {
    fn new() -> Self {
        Self::from_parts(FxHashMap::default(), Vec::new())
    }

    /// A catalog with every slot occupied.
    fn from_parts(ids: FxHashMap<NodeId, usize>, slots: Vec<NodeRecord>) -> Self {
        debug_assert_eq!(ids.len(), slots.len());
        Self {
            full_capacity: ids.capacity(),
            ids,
            slots,
            free_head: NO_SLOT,
            vacant: 0,
            #[cfg(test)]
            slot_compactions: 0,
            #[cfg(test)]
            slot_reuses: 0,
        }
    }

    /// Number of registered nodes.
    fn len(&self) -> usize {
        self.ids.len()
    }

    fn slot(&self, node: NodeId) -> Option<usize> {
        self.ids.get(&node).copied()
    }

    fn contains(&self, node: NodeId) -> bool {
        self.ids.contains_key(&node)
    }

    fn record(&self, node: NodeId) -> Option<&NodeRecord> {
        self.slot(node).map(|slot| &self.slots[slot])
    }

    /// Binary search for `neighbor` (an external ID) in the incident list of
    /// slot `owner`, translating each probed entry's slot to its ID:
    /// `O(log d)` probes, each followed by one slot read.
    fn locate(&self, owner: usize, neighbor: NodeId) -> Result<usize, usize> {
        self.slots[owner]
            .incident
            .binary_search_by_key(&neighbor, |incidence| self.slots[incidence.slot()].id)
    }

    /// Sorts every incident list by the neighbors' external IDs and drops
    /// adjacent repeats; returns whether a repeat had a different flag.
    /// `O(E log E)` comparisons, each reading two slots.
    fn sort_incident_lists(&mut self) -> bool {
        let mut conflict = false;
        for slot in 0..self.slots.len() {
            let mut list = std::mem::take(&mut self.slots[slot].incident);
            list.sort_unstable_by_key(|incidence| self.slots[incidence.slot()].id);
            list.dedup_by(|later, kept| {
                let repeat = later.slot() == kept.slot();
                conflict |= repeat && later.enabled() != kept.enabled();
                repeat
            });
            self.slots[slot].incident = list;
        }
        conflict
    }

    /// Registers `node`, known to be absent, and returns its slot: the head
    /// of the free list if any slot is vacant, otherwise a new slot at the
    /// end. A new slot reserves the slot vector up to the ID map's capacity,
    /// so that both grow in step (design section 4.2). Expected `O(1)`,
    /// amortized for growth and for ID-map rebuilds.
    fn insert(&mut self, node: NodeId, active: bool) -> usize {
        self.reserve_id_entry();
        self.ids.reserve(1);
        let record = NodeRecord::registered(node, active);
        let slot = if self.free_head == NO_SLOT {
            if self.slots.len() == self.slots.capacity() {
                let target = self.ids.capacity().max(self.slots.len() + 1);
                self.slots.reserve_exact(target - self.slots.len());
            }
            self.slots.push(record);
            self.slots.len() - 1
        } else {
            let slot = self.free_head;
            debug_assert!(!self.slots[slot].is_occupied());
            self.free_head = self.slots[slot].id as usize;
            self.vacant -= 1;
            self.slots[slot] = record;
            #[cfg(test)]
            {
                self.slot_reuses += 1;
            }
            slot
        };
        self.ids.insert(node, slot);
        slot
    }

    /// Makes room for one new ID-map entry without growing a table whose
    /// headroom was consumed by tombstones (see the module documentation):
    /// when the next insert would grow the table and live entries are at most
    /// 90% of its full capacity, the map is rebuilt into a fresh table sized
    /// for its live entries (never larger than the old one). `O(V)` when it
    /// rebuilds, `O(1)` otherwise.
    fn reserve_id_entry(&mut self) {
        let capacity = self.ids.capacity();
        self.full_capacity = self.full_capacity.max(capacity);
        let live = self.ids.len();
        if capacity == live
            && live * REBUILD_LIVE_DENOMINATOR <= self.full_capacity * REBUILD_LIVE_NUMERATOR
        {
            let mut fresh: FxHashMap<NodeId, usize> =
                FxHashMap::with_capacity_and_hasher(live, Default::default());
            fresh.extend(self.ids.drain());
            self.ids = fresh;
            self.full_capacity = self.ids.capacity();
        }
    }

    /// Vacates `slot` after every incidence naming it was removed: unmaps its
    /// ID, releases its incident vector and pushes it on the free list.
    /// Expected `O(1)`.
    fn release(&mut self, slot: usize) {
        let record = &mut self.slots[slot];
        debug_assert!(record.is_occupied());
        self.ids.remove(&record.id);
        record.incident = Vec::new();
        record.pending = 0;
        record.state = SlotState::Vacant;
        record.id = self.free_head as NodeId;
        self.free_head = slot;
        self.vacant += 1;
    }

    /// Compacts the slot vector when vacant slots exceed
    /// `max(registered nodes, MIN_VACANT_BEFORE_SLOT_COMPACTION)`, the rule
    /// of engine compaction. More than `max(V, 1024)` removals separate two
    /// compactions, so the `O(V log V + E)` cost of
    /// [`Self::renumber_by_id`] is amortized to `O(log V + E / V)` per
    /// removal, and slots stay at most `V + max(V, 1024)`.
    fn compact_if_needed(&mut self) {
        if self.vacant <= self.len().max(MIN_VACANT_BEFORE_SLOT_COMPACTION) {
            return;
        }
        self.renumber_by_id();
        self.slots.shrink_to(self.ids.capacity());
        #[cfg(test)]
        {
            self.slot_compactions += 1;
        }
    }

    /// Renumbers the occupied slots densely in ascending external ID and
    /// drops the vacant ones (design section 5.4).
    ///
    /// One bijection, old occupied slot → new dense slot, is built from an ID
    /// sort; every incidence and every ID-map value is rewritten through it
    /// (they are the only stored slot indices, I7), then the records are
    /// permuted in place by cycle-following swaps and the vector is truncated.
    /// Each incidence keeps its position, so incident lists stay sorted by
    /// external ID (and are now sorted by slot too). `O(V log V + E)` time;
    /// scratch: the sorted slot list (`V` words, dropped before the rewrite)
    /// and the bijection (`V + F` words).
    fn renumber_by_id(&mut self) {
        let len = self.slots.len();
        let mut order: Vec<usize> = (0..len)
            .filter(|&slot| self.slots[slot].is_occupied())
            .collect();
        order.sort_unstable_by_key(|&slot| self.slots[slot].id);
        let mut renumbered = vec![NO_SLOT; len];
        for (new, &old) in order.iter().enumerate() {
            renumbered[old] = new;
        }
        drop(order);
        for record in &mut self.slots {
            for incidence in &mut record.incident {
                incidence.set_slot(renumbered[incidence.slot()]);
            }
        }
        for slot in self.ids.values_mut() {
            *slot = renumbered[*slot];
        }
        // Each swap moves one record to its final position, where it stays;
        // the record displaced from there lands at `position` and is handled
        // next. Vacant records (`NO_SLOT`) end up beyond the occupied prefix.
        for position in 0..len {
            loop {
                let target = renumbered[position];
                if target == position || target == NO_SLOT {
                    break;
                }
                self.slots.swap(position, target);
                renumbered.swap(position, target);
            }
        }
        self.slots.truncate(self.ids.len());
        self.free_head = NO_SLOT;
        self.vacant = 0;
    }
}

/// Minimum number of vacant catalog slots before a slot compaction is
/// considered: the engine-compaction threshold, so both follow one rule.
const MIN_VACANT_BEFORE_SLOT_COMPACTION: usize = MIN_RETIRED_BEFORE_COMPACTION;

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
    catalog: Catalog,
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
            catalog: Catalog::new(),
            engine: ConnectivityGraph::new(engine),
            limits,
            checks: Self::bounded(engine, limits),
            active_nodes: 0,
            registered_edges: 0,
            enabled_edges: 0,
            effective_edges: 0,
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
            Some(catalog) => Ok(Self::finish(engine, limits, catalog)),
            None => Self::catalog_exact(checks, nodes, edges)
                .map(|catalog| Self::finish(engine, limits, catalog)),
        }
    }

    /// Catalog assembly for inputs that cannot fail: returns `None`, without
    /// side effects, whenever an error could arise (a self-loop, a conflicting
    /// repeated node or edge, a node limit reached, or more edge entries than
    /// the edge limit), and [`Self::catalog_exact`] then reproduces the exact
    /// first error in input order.
    ///
    /// Slots are assigned in first-registration order (`nodes`, then new edge
    /// endpoints), so slot order is the registration order;
    /// [`Self::finish`] renumbers them by ID. A first pass registers every ID
    /// in the ID map and counts degrees in a 4-byte-per-node scratch vector;
    /// the slot vector is then allocated exactly once with its final length,
    /// each incident vector likewise (exactly sized for duplicate-free
    /// input; repeated entries are counted, so such vectors are shrunk by
    /// `finish`). Edges are appended without a per-edge duplicate lookup:
    /// incident lists are sorted once and adjacent repeats are dropped (a
    /// repeated edge with a different flag returns `None`). Without a node
    /// limit each endpoint costs two expected O(1) ID-map probes (counting
    /// pass and fill pass; two more per edge with a node limit) instead of
    /// the exact path's duplicate-map lookup and insertion plus endpoint
    /// probes. `O(V + E log E)` time overall, the same bound as the exact
    /// path. An input that fails is detected here first, so error paths pay
    /// up to about twice the work.
    fn catalog_fast(
        limits: GraphLimits,
        nodes: &[NodeSpec],
        edges: &[EdgeSpec],
    ) -> Option<Catalog> {
        if limits.max_edges.is_some_and(|limit| edges.len() > limit) {
            return None;
        }
        let mut ids: FxHashMap<NodeId, usize> = FxHashMap::default();
        ids.reserve(nodes.len());
        // Flags of the nodes listed in `nodes`, which take the first slots.
        let mut flags: Vec<bool> = Vec::with_capacity(nodes.len());
        for spec in nodes {
            match ids.entry(spec.node) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    if flags[*entry.get()] != spec.active {
                        return None;
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    if limits.max_nodes.is_some_and(|limit| flags.len() >= limit) {
                        return None;
                    }
                    entry.insert(flags.len());
                    flags.push(spec.active);
                }
            }
        }
        // Saturating degree counts (only a capacity hint), indexed by slot.
        let mut degrees: Vec<u32> = vec![0; flags.len()];
        for spec in edges {
            if spec.source == spec.target {
                return None;
            }
            if let Some(limit) = limits.max_nodes {
                let missing = usize::from(!ids.contains_key(&spec.source))
                    + usize::from(!ids.contains_key(&spec.target));
                if missing > limit.saturating_sub(ids.len()) {
                    return None;
                }
            }
            for endpoint in [spec.source, spec.target] {
                let next = degrees.len();
                let slot = *ids.entry(endpoint).or_insert(next);
                if slot == next {
                    degrees.push(0);
                }
                degrees[slot] = degrees[slot].saturating_add(1);
            }
        }
        let mut slots: Vec<NodeRecord> = Vec::with_capacity(degrees.len());
        for (slot, &degree) in degrees.iter().enumerate() {
            slots.push(NodeRecord {
                id: 0, // set from the ID map below
                incident: Vec::with_capacity(degree as usize),
                pending: 0,
                state: SlotState::registered(flags.get(slot).copied().unwrap_or(true)),
            });
        }
        drop(degrees);
        drop(flags);
        for (&node, &slot) in &ids {
            slots[slot].id = node;
        }
        for spec in edges {
            let (source, target) = (ids[&spec.source], ids[&spec.target]);
            slots[source]
                .incident
                .push(Incidence::new(target, spec.enabled));
            slots[target]
                .incident
                .push(Incidence::new(source, spec.enabled));
        }
        let mut catalog = Catalog::from_parts(ids, slots);
        if catalog.sort_incident_lists() {
            return None;
        }
        Some(catalog)
    }

    /// Exact catalog assembly: processes entries in input order and stops at
    /// the first failing one (see [`Self::try_from_topology`]). Slots are
    /// assigned in first-registration order, as in [`Self::catalog_fast`].
    /// Incident lists are sorted here. `O((V + E) log E)` time.
    fn catalog_exact(
        limits: GraphLimits,
        nodes: &[NodeSpec],
        edges: &[EdgeSpec],
    ) -> Result<Catalog, LifecycleError> {
        let mut ids: FxHashMap<NodeId, usize> = FxHashMap::default();
        ids.reserve(nodes.len());
        let mut slots: Vec<NodeRecord> = Vec::with_capacity(nodes.len());
        let node_limit = limits.max_nodes;
        for spec in nodes {
            if let Some(&slot) = ids.get(&spec.node) {
                if slots[slot].is_active() != spec.active {
                    return Err(LifecycleError::ConflictingNode { node: spec.node });
                }
                continue;
            }
            if let Some(limit) = node_limit
                && slots.len() >= limit
            {
                return Err(GraphError::NodeLimitExceeded { limit }.into());
            }
            ids.insert(spec.node, slots.len());
            slots.push(NodeRecord::registered(spec.node, spec.active));
        }

        let mut seen: FxHashMap<(NodeId, NodeId), bool> = FxHashMap::default();
        seen.reserve(edges.len());
        for spec in edges {
            GraphError::check_distinct(spec.source, spec.target)?;
            let missing = usize::from(!ids.contains_key(&spec.source))
                + usize::from(!ids.contains_key(&spec.target));
            if let Some(limit) = node_limit
                && missing > limit.saturating_sub(slots.len())
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
            let mut endpoints = [0; 2];
            for (endpoint_slot, endpoint) in endpoints.iter_mut().zip([spec.source, spec.target]) {
                *endpoint_slot = *ids.entry(endpoint).or_insert_with(|| {
                    slots.push(NodeRecord::registered(endpoint, true));
                    slots.len() - 1
                });
            }
            seen.insert(key, spec.enabled);
            let [source, target] = endpoints;
            slots[source]
                .incident
                .push(Incidence::new(target, spec.enabled));
            slots[target]
                .incident
                .push(Incidence::new(source, spec.enabled));
        }
        drop(seen);

        let mut catalog = Catalog::from_parts(ids, slots);
        let conflict = catalog.sort_incident_lists();
        debug_assert!(!conflict, "repeated edges were skipped above");
        Ok(catalog)
    }

    /// Shrinks the slot vector and the incident lists, computes counts,
    /// builds the engine from the effective edges of a validated catalog
    /// with sorted, duplicate-free incident lists, and renumbers the slots in
    /// ascending ID order (design section 5.1). `O(V log V + E)` (see
    /// [`Self::build_engine`] and [`Catalog::renumber_by_id`]).
    ///
    /// For the one-pass `compact-bfs` build the slots are renumbered first,
    /// so that the build visits them in address order. The incremental build
    /// runs before renumbering, so it inserts vertices in registration order
    /// (the slot order of the assembled catalog), as before the slot layout.
    fn finish(engine: EngineConfig, limits: GraphLimits, mut catalog: Catalog) -> Self {
        // A no-op after `catalog_fast`, which allocates the slot vector with
        // its final length; `catalog_exact` grows it.
        catalog.slots.shrink_to_fit();
        let mut active_nodes = 0;
        let mut registered_edges = 0;
        let mut enabled_incidences = 0;
        for record in &mut catalog.slots {
            record.incident.shrink_to_fit();
            registered_edges += record.incident.len();
            enabled_incidences += record.incident.iter().filter(|i| i.enabled()).count();
            active_nodes += usize::from(record.is_active());
        }
        let (engine_graph, effective_edges) = if Self::builds_in_one_pass(engine, catalog.len()) {
            catalog.renumber_by_id();
            Self::build_engine(engine, &mut catalog)
        } else {
            let built = Self::build_engine_incremental(engine, &catalog);
            catalog.renumber_by_id();
            built
        };
        catalog.full_capacity = catalog.ids.capacity();
        Self {
            catalog,
            engine: engine_graph,
            limits,
            checks: Self::bounded(engine, limits),
            active_nodes,
            registered_edges: registered_edges / 2,
            enabled_edges: enabled_incidences / 2,
            effective_edges,
        }
    }

    /// Whether [`Self::build_engine`] uses the one-pass `compact-bfs` build:
    /// only for that engine and for catalogs indexable by `u32`.
    fn builds_in_one_pass(engine: EngineConfig, nodes: usize) -> bool {
        engine == EngineConfig::CompactBfs && u32::try_from(nodes).is_ok()
    }

    /// Builds an unlimited engine holding every catalog node as a vertex and
    /// exactly the effective edges; returns it with the effective edge count.
    ///
    /// For `compact-bfs` the engine is built in one pass
    /// ([`Graph::from_sorted_adjacency`](crate::Graph::from_sorted_adjacency)):
    /// the occupied slots are sorted by node ID and receive engine indices in
    /// ascending ID order (the index is parked in the construction-only
    /// `pending` field, reset to zero afterwards). Each effective edge is
    /// found once, from its smaller endpoint, whose incident list is entered
    /// by a binary search past its smaller neighbors, and is appended to both
    /// adjacency lists; visiting nodes in index order keeps every list
    /// strictly sorted. Each adjacency vector is allocated once with capacity
    /// equal to the node's enabled registered degree (an upper bound of its
    /// effective degree), so no vector grows. `O(V log V + E)` time with
    /// `O(1)` slot reads; scratch memory is the `O(V)` sorted slot vector,
    /// converted in place into the engine's sorted ID vector.
    ///
    /// `cluster-forest` (the other lifecycle engine), and catalogs too large
    /// for 32-bit indices, use the incremental
    /// [`Self::build_engine_incremental`], which inserts vertices in slot
    /// order.
    fn build_engine(engine: EngineConfig, catalog: &mut Catalog) -> (ConnectivityGraph, usize) {
        if !Self::builds_in_one_pass(engine, catalog.len()) {
            return Self::build_engine_incremental(engine, catalog);
        }
        let slots = &mut catalog.slots;
        let mut order: Vec<usize> = (0..slots.len())
            .filter(|&slot| slots[slot].is_occupied())
            .collect();
        order.sort_unstable_by_key(|&slot| slots[slot].id);
        for (index, &slot) in order.iter().enumerate() {
            slots[slot].pending = u32::try_from(index).expect("node count checked above");
        }
        let slots = &catalog.slots;
        let mut adjacency: Vec<Vec<usize>> = order
            .iter()
            .map(|&slot| {
                let record = &slots[slot];
                if record.is_active() {
                    Vec::with_capacity(record.incident.iter().filter(|i| i.enabled()).count())
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
        for (index, &slot) in order.iter().enumerate() {
            let record = &slots[slot];
            if !record.is_active() {
                continue;
            }
            let larger = record
                .incident
                .partition_point(|incidence| slots[incidence.slot()].id < record.id);
            for incidence in &record.incident[larger..] {
                if !incidence.enabled() {
                    continue;
                }
                let neighbor = &slots[incidence.slot()];
                if neighbor.is_active() {
                    let other = neighbor.pending as usize;
                    adjacency[index].push(other);
                    adjacency[other].push(index);
                    edges += 1;
                }
            }
        }
        for &slot in &order {
            catalog.slots[slot].pending = 0;
        }
        let slots = &catalog.slots;
        let ids: Vec<NodeId> = order.into_iter().map(|slot| slots[slot].id).collect();
        let graph = crate::Graph::from_sorted_adjacency(ids, adjacency);
        (ConnectivityGraph::from(graph), edges)
    }

    /// Incremental engine construction through
    /// [`ConnectivityGraph::try_from_edges`]: the occupied slots' nodes in
    /// slot order, then the effective edges. `O(V + E)` catalog work plus one
    /// engine insertion per effective edge. Used for engines other than
    /// `compact-bfs` and as the differential reference in tests.
    fn build_engine_incremental(
        engine: EngineConfig,
        catalog: &Catalog,
    ) -> (ConnectivityGraph, usize) {
        let slots = &catalog.slots;
        let order: Vec<NodeId> = slots
            .iter()
            .filter(|record| record.is_occupied())
            .map(|record| record.id)
            .collect();
        let mut effective: Vec<(NodeId, NodeId)> = Vec::new();
        for record in slots.iter().filter(|record| record.is_active()) {
            for incidence in &record.incident {
                if !incidence.enabled() {
                    continue;
                }
                let neighbor = &slots[incidence.slot()];
                if record.id < neighbor.id && neighbor.is_active() {
                    effective.push((record.id, neighbor.id));
                }
            }
        }
        let graph =
            ConnectivityGraph::try_from_edges(engine, GraphLimits::default(), &order, &effective)
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
        if self.catalog.contains(node) {
            return Ok(false);
        }
        if let Some(limit) = self.checks.max_nodes
            && self.catalog.len() >= limit
        {
            return Err(GraphError::NodeLimitExceeded { limit });
        }
        self.register(node);
        Ok(true)
    }

    /// Registers a node known to be absent and within limits; returns its
    /// catalog slot.
    fn register(&mut self, node: NodeId) -> usize {
        let slot = self.catalog.insert(node, true);
        // A retired engine slot for `node` is isolated and is reused as is.
        self.engine
            .add_node(node)
            .expect("an unlimited engine accepts any vertex");
        self.active_nodes += 1;
        slot
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
        let source_slot = self.catalog.slot(source);
        let target_slot = self.catalog.slot(target);
        if let Some(limit) = self.checks.max_nodes {
            let missing = usize::from(source_slot.is_none()) + usize::from(target_slot.is_none());
            if missing > limit.saturating_sub(self.catalog.len()) {
                return Err(GraphError::NodeLimitExceeded { limit });
            }
        }
        if let (Some(slot), Some(_)) = (source_slot, target_slot)
            && self.catalog.locate(slot, target).is_ok()
        {
            return Ok(false);
        }
        if let Some(limit) = self.checks.max_edges
            && self.registered_edges >= limit
        {
            return Err(GraphError::EdgeLimitExceeded { limit });
        }
        let source_slot = source_slot.unwrap_or_else(|| self.register(source));
        let target_slot = target_slot.unwrap_or_else(|| self.register(target));
        self.insert_edge(source_slot, target_slot, true);
        Ok(true)
    }

    /// Inserts a validated, absent edge between the registered distinct
    /// nodes in slots `source` and `target`.
    fn insert_edge(&mut self, source: usize, target: usize, enabled: bool) {
        for (owner, neighbor) in [(source, target), (target, source)] {
            let index = self
                .catalog
                .locate(owner, self.catalog.slots[neighbor].id)
                .expect_err("edge is absent at both endpoints");
            self.catalog.slots[owner]
                .incident
                .insert(index, Incidence::new(neighbor, enabled));
        }
        self.registered_edges += 1;
        if enabled {
            self.enabled_edges += 1;
            let (source, target) = (&self.catalog.slots[source], &self.catalog.slots[target]);
            if source.is_active() && target.is_active() {
                let (source, target) = (source.id, target.id);
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
        let (Some(source_slot), Some(target_slot)) =
            (self.catalog.slot(source), self.catalog.slot(target))
        else {
            return Ok(false);
        };
        let Ok(index) = self.catalog.locate(source_slot, target) else {
            return Ok(false);
        };
        let reverse = self
            .catalog
            .locate(target_slot, source)
            .expect("incidence is symmetric");
        let slots = &mut self.catalog.slots;
        let both_active = slots[source_slot].is_active() && slots[target_slot].is_active();
        let removed = slots[source_slot].remove_at(index);
        slots[target_slot].remove_at(reverse);
        self.registered_edges -= 1;
        if removed.enabled() {
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
        let slot = self
            .catalog
            .slot(node)
            .ok_or(GraphError::UnknownNode { node })?;
        let slots = &self.catalog.slots;
        let record = &slots[slot];
        if record.is_active() == active {
            return Ok(false);
        }
        // Effective incident edges are visited in descending neighbor order
        // (see `remove_node` for why).
        let mut affected = Vec::new();
        for incidence in record.incident.iter().rev() {
            if incidence.enabled() {
                let neighbor = &slots[incidence.slot()];
                if neighbor.is_active() {
                    affected.push(neighbor.id);
                }
            }
        }
        for neighbor in affected {
            if active {
                self.engine_link(node, neighbor);
            } else {
                self.engine_cut(node, neighbor);
            }
        }
        self.catalog.slots[slot].state = SlotState::registered(active);
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
        let source_slot = self
            .catalog
            .slot(source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let target_slot = self
            .catalog
            .slot(target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        let index = self
            .catalog
            .locate(source_slot, target)
            .map_err(|_| LifecycleError::UnknownEdge { source, target })?;
        if self.catalog.slots[source_slot].incident[index].enabled() == enabled {
            return Ok(false);
        }
        let reverse = self
            .catalog
            .locate(target_slot, source)
            .expect("incidence is symmetric");
        let slots = &mut self.catalog.slots;
        let both_active = slots[source_slot].is_active() && slots[target_slot].is_active();
        slots[source_slot].incident[index].set_enabled(enabled);
        slots[target_slot].incident[reverse].set_enabled(enabled);
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
    /// amortized share of engine and catalog-slot compaction.
    pub fn remove_node(&mut self, node: NodeId) -> bool {
        let Some(slot) = self.catalog.slot(node) else {
            return false;
        };
        let active = self.catalog.slots[slot].is_active();
        let incident = std::mem::take(&mut self.catalog.slots[slot].incident);
        // Descending neighbor order. For `compact-bfs`, the only engine with
        // lifecycle support, whose adjacency vectors are sorted, this removes
        // from the end of the node's vector, so each removal shifts no elements
        // instead of up to `d`. (Earlier candidates also measured this order on
        // the now-deferred `ett` and `hdt` lifecycle support.)
        for incidence in incident.iter().rev() {
            let neighbor_slot = incidence.slot();
            // The search translates slot `slot` to `node`: the slot is still
            // occupied until `release` below.
            let reverse = self
                .catalog
                .locate(neighbor_slot, node)
                .expect("incidence is symmetric");
            let neighbor = &mut self.catalog.slots[neighbor_slot];
            let neighbor_active = neighbor.is_active();
            let neighbor_id = neighbor.id;
            neighbor.remove_at(reverse);
            self.registered_edges -= 1;
            if incidence.enabled() {
                self.enabled_edges -= 1;
                if active && neighbor_active {
                    self.engine_cut(node, neighbor_id);
                }
            }
        }
        drop(incident);
        // No incidence names `slot` any more (symmetry), so it can be reused.
        self.catalog.release(slot);
        if active {
            self.active_nodes -= 1;
        }
        // The retired slot is isolated now; release its adjacency allocation
        // so it retains only its ID-map entry and an empty vector header.
        self.engine.release_isolated_storage(node);
        self.catalog.compact_if_needed();
        self.compact_if_needed();
        true
    }

    /// Rebuilds the engine when retired slots exceed
    /// `max(registered nodes, MIN_RETIRED_BEFORE_COMPACTION)`, through
    /// [`Self::build_engine`]. The old engine is dropped after the new one is
    /// built, so both are live at the peak.
    fn compact_if_needed(&mut self) {
        let retired = self.engine.node_count() - self.catalog.len();
        if retired <= self.catalog.len().max(MIN_RETIRED_BEFORE_COMPACTION) {
            return;
        }
        let (engine, effective) = Self::build_engine(self.engine.engine(), &mut self.catalog);
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
            .catalog
            .record(source)
            .ok_or(GraphError::UnknownNode { node: source })?;
        let target_record = self
            .catalog
            .record(target)
            .ok_or(GraphError::UnknownNode { node: target })?;
        if !source_record.is_active() || !target_record.is_active() {
            return Ok(false);
        }
        self.engine.connected(source, target)
    }

    /// Returns `Some(active)` for a registered node, `None` otherwise.
    #[must_use]
    pub fn node_active(&self, node: NodeId) -> Option<bool> {
        self.catalog.record(node).map(NodeRecord::is_active)
    }

    /// Returns the number of registered edges incident to `node` (enabled or
    /// not), or `None` if it is not registered. Expected O(1).
    #[must_use]
    pub fn degree(&self, node: NodeId) -> Option<usize> {
        self.catalog
            .record(node)
            .map(|record| record.incident.len())
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
    /// Positioning is O(log deg(node)); each step reads the neighbor's
    /// catalog entry once (O(1)), for its ID and for `effective`.
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
        let record = self.catalog.record(node)?;
        let slots = &self.catalog.slots;
        // A lower bound by external ID: the cursor itself is never looked up,
        // so it may name a removed or never-registered node (invariants 1, I5).
        let start = after.map_or(0, |cursor| {
            record
                .incident
                .partition_point(|incidence| slots[incidence.slot()].id <= cursor)
        });
        Some(record.incident[start..].iter().map(move |incidence| {
            let neighbor = &slots[incidence.slot()];
            let enabled = incidence.enabled();
            (
                neighbor.id,
                EdgeState {
                    enabled,
                    effective: enabled && record.is_active() && neighbor.is_active(),
                },
            )
        }))
    }

    /// Returns the flags of the registered edge `{source, target}`, or `None`
    /// if it is not registered. A self-loop pair is never registered.
    #[must_use]
    pub fn edge_state(&self, source: NodeId, target: NodeId) -> Option<EdgeState> {
        let source_slot = self.catalog.slot(source)?;
        let index = self.catalog.locate(source_slot, target).ok()?;
        let source_record = &self.catalog.slots[source_slot];
        let incidence = source_record.incident[index];
        let enabled = incidence.enabled();
        let effective = enabled
            && source_record.is_active()
            && self.catalog.slots[incidence.slot()].is_active();
        Some(EdgeState { enabled, effective })
    }

    /// Returns the number of registered nodes, active or inactive.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.catalog.len()
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
        // Vacant slots hold empty, unallocated incident vectors.
        let (incident_entries, incident_capacity) =
            self.catalog
                .slots
                .iter()
                .fold((0, 0), |(len, cap), record| {
                    (
                        len + record.incident.len(),
                        cap + record.incident.capacity(),
                    )
                });
        let engine_vertices = self.engine.node_count();
        LifecycleStorage {
            engine_vertices,
            retired_vertices: engine_vertices - self.catalog.len(),
            catalog_nodes: self.catalog.len(),
            catalog_node_capacity: self.catalog.ids.capacity(),
            incident_entries,
            incident_capacity,
        }
    }

    /// Verifies every internal invariant (1 to 4 and I5 to I7 of the module
    /// documentation); panics on violation. Test support.
    #[cfg(test)]
    pub(crate) fn validate(&self) {
        let catalog = &self.catalog;
        let slots = &catalog.slots;
        // I6: the ID map and the occupied slots are in bijection.
        for (&node, &slot) in &catalog.ids {
            assert!(slots[slot].is_occupied(), "map names a vacant slot");
            assert_eq!(slots[slot].id, node, "map and slot disagree");
        }
        let occupied = slots.iter().filter(|record| record.is_occupied()).count();
        assert_eq!(occupied, catalog.ids.len());
        // Free list: exactly the vacant slots, each once, LIFO-linked.
        let mut on_list = vec![false; slots.len()];
        let mut cursor = catalog.free_head;
        let mut listed = 0;
        while cursor != NO_SLOT {
            assert!(
                !slots[cursor].is_occupied(),
                "free list names an occupied slot"
            );
            assert!(!on_list[cursor], "free list has a cycle");
            on_list[cursor] = true;
            listed += 1;
            cursor = slots[cursor].id as usize;
        }
        assert_eq!(listed, catalog.vacant);
        assert_eq!(listed, slots.len() - occupied);
        assert!(catalog.vacant <= catalog.len().max(MIN_VACANT_BEFORE_SLOT_COMPACTION));

        let mut registered = 0;
        let mut enabled = 0;
        let mut effective = 0;
        let mut active = 0;
        for (slot, record) in slots.iter().enumerate() {
            assert_eq!(record.pending, 0, "construction scratch is reset");
            if !record.is_occupied() {
                assert_eq!(record.incident.capacity(), 0, "vacant slot holds no list");
                continue;
            }
            let node = record.id;
            active += usize::from(record.is_active());
            assert!(self.engine.contains_node(node));
            for incidence in &record.incident {
                // I5: every stored index names an occupied slot.
                let other = &slots[incidence.slot()];
                assert!(other.is_occupied(), "incidence names a vacant slot");
                assert_ne!(incidence.slot(), slot);
                assert_ne!(other.id, node);
            }
            for pair in record.incident.windows(2) {
                assert!(slots[pair[0].slot()].id < slots[pair[1].slot()].id);
            }
            for incidence in &record.incident {
                let other = &slots[incidence.slot()];
                let back = catalog.locate(incidence.slot(), node).expect("symmetric");
                assert_eq!(other.incident[back].slot(), slot);
                assert_eq!(other.incident[back].enabled(), incidence.enabled());
                if node < other.id {
                    registered += 1;
                    enabled += usize::from(incidence.enabled());
                    if incidence.enabled() && record.is_active() && other.is_active() {
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
        assert!(self.engine.node_count() <= 2 * catalog.len() + MIN_RETIRED_BEFORE_COMPACTION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// One slot as compared by tests: external ID, state, and the incident
    /// list translated to `(neighbor ID, enabled)` pairs.
    type CatalogRow = (NodeId, SlotState, Vec<(NodeId, bool)>);

    /// A catalog's slots in slot order (see [`CatalogRow`]).
    fn catalog_rows(catalog: &Catalog) -> Vec<CatalogRow> {
        catalog
            .slots
            .iter()
            .map(|record| {
                let incident = record
                    .incident
                    .iter()
                    .map(|i| (catalog.slots[i.slot()].id, i.enabled()))
                    .collect();
                (record.id, record.state, incident)
            })
            .collect()
    }

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
        let before = graph.catalog.record(0).unwrap().incident.capacity();
        for neighbor in 1..=60 {
            graph.cut(0, neighbor).unwrap();
        }
        assert!(graph.catalog.record(0).unwrap().incident.capacity() < before);
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

    /// The B64 layout of `docs/design/compact-node-index.md`, section 3:
    /// 8-byte incidences, 40-byte slot records (the scratch word and the
    /// state share the padding) and 16-byte ID-map entries.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn catalog_layout_sizes() {
        assert_eq!(size_of::<Incidence>(), 8);
        assert_eq!(size_of::<NodeRecord>(), 40);
        assert_eq!(size_of::<(NodeId, usize)>(), 16);
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
                (Ok(exact_catalog), Some(fast_catalog)) => {
                    fast_successes += 1;
                    // Same slots in the same (registration) order, same IDs,
                    // flags and incident lists.
                    assert_eq!(catalog_rows(exact_catalog), catalog_rows(&fast_catalog));
                    for catalog in [exact_catalog, &fast_catalog] {
                        for (&node, &slot) in &catalog.ids {
                            assert_eq!(catalog.slots[slot].id, node, "case {case}");
                        }
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
            let Ok(mut catalog) =
                LifecycleGraph::catalog_exact(GraphLimits::default(), &nodes, &edges)
            else {
                continue; // conflicting repeated flags
            };
            // Registration order: the slot order of the assembled catalog.
            let order: Vec<NodeId> = catalog.slots.iter().map(|record| record.id).collect();
            let (incremental, incremental_edges) =
                LifecycleGraph::build_engine_incremental(EngineConfig::CompactBfs, &catalog);
            let (bulk, bulk_edges) =
                LifecycleGraph::build_engine(EngineConfig::CompactBfs, &mut catalog);
            assert_eq!(bulk_edges, incremental_edges, "case {case}");
            assert_eq!(bulk.edge_count(), incremental.edge_count(), "case {case}");
            assert_eq!(bulk.node_count(), incremental.node_count(), "case {case}");
            assert!(catalog.slots.iter().all(|record| record.pending == 0));
            for &a in &order {
                for &b in &order {
                    assert_eq!(
                        bulk.connected(a, b),
                        incremental.connected(a, b),
                        "case {case}: {a}-{b}"
                    );
                }
            }
            let graph =
                LifecycleGraph::finish(EngineConfig::CompactBfs, GraphLimits::default(), catalog);
            graph.validate();
            // Bulk import leaves the slots dense, in ascending ID order, and
            // the slot vector exactly reserved.
            let slots = &graph.catalog.slots;
            assert!(slots.windows(2).all(|pair| pair[0].id < pair[1].id));
            assert_eq!(slots.len(), graph.node_count());
            assert_eq!(slots.capacity(), slots.len(), "case {case}");
        }
    }

    /// The incremental fallback of `build_engine` (non-`compact-bfs`
    /// engines) registers every catalog node, on an assembled catalog and on
    /// one with vacant slots (as compaction sees it), and agrees with the
    /// one-pass `compact-bfs` build on edges and connectivity.
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
        let assembled =
            LifecycleGraph::catalog_exact(GraphLimits::default(), &nodes, &edges).unwrap();
        let mut graph = LifecycleGraph::try_from_topology(
            EngineConfig::CompactBfs,
            GraphLimits::default(),
            &nodes,
            &edges,
        )
        .unwrap();
        graph.link(9, 11).unwrap();
        graph.link(11, 1).unwrap();
        assert!(graph.remove_node(7));
        assert!(graph.add_node(13).unwrap());
        assert!(graph.remove_node(11));
        assert!(graph.catalog.vacant > 0);
        let mut churned = graph.catalog;
        for catalog in [&mut { assembled }, &mut churned] {
            let order: Vec<NodeId> = catalog.ids.keys().copied().collect();
            let (compact, compact_edges) =
                LifecycleGraph::build_engine(EngineConfig::CompactBfs, catalog);
            let (fallback, fallback_edges) =
                LifecycleGraph::build_engine(EngineConfig::EulerTour, catalog);
            assert_eq!(fallback.engine(), EngineConfig::EulerTour);
            assert_eq!(fallback.node_count(), catalog.len());
            assert_eq!(compact.node_count(), catalog.len());
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
        let full = graph.catalog.ids.capacity();
        assert_eq!(graph.catalog.full_capacity, full);
        // Node 0 (the hub) and node N - 1 (inactive) stay; the others are churned.
        let mut live: std::collections::VecDeque<u64> = (1..N - 1).collect();
        let mut rebuilds = 0;
        for round in 0..(8 * N) {
            let before = graph.catalog.ids.capacity();
            let old = live.pop_front().unwrap();
            assert!(graph.remove_node(old));
            let fresh = 10 * N + round;
            graph.link(0, fresh).unwrap();
            live.push_back(fresh);
            let after = graph.catalog.ids.capacity();
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
        assert_eq!(graph.catalog.full_capacity, full);
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
        let first = graph.catalog.ids.capacity();
        for v in 1_000..10_000 {
            graph.add_node(v).unwrap();
        }
        assert!(graph.catalog.ids.capacity() > first);
        assert!(graph.catalog.full_capacity >= graph.catalog.ids.capacity());
        graph.validate();
    }

    /// The packed incidence keeps slot and flag independent, up to the
    /// largest slot index below the flag bit.
    #[test]
    fn incidence_packs_slot_and_enabled_flag() {
        for slot in [0, 1, 12_345, ENABLED_BIT - 1] {
            for enabled in [false, true] {
                let mut incidence = Incidence::new(slot, enabled);
                assert_eq!(incidence.slot(), slot);
                assert_eq!(incidence.enabled(), enabled);
                incidence.set_enabled(!enabled);
                assert_eq!((incidence.slot(), incidence.enabled()), (slot, !enabled));
                incidence.set_slot(7);
                assert_eq!((incidence.slot(), incidence.enabled()), (7, !enabled));
            }
        }
    }

    /// Vacant slots are reused last-in first-out; new slots grow the slot
    /// vector in step with the ID map's capacity.
    #[test]
    fn slots_are_reused_lifo_and_grow_with_the_id_map() {
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        for node in 0..10_000 {
            graph.add_node(node).unwrap();
            assert_eq!(
                graph.catalog.slots.capacity(),
                graph.catalog.ids.capacity(),
                "node {node}"
            );
        }
        let slot_of = |graph: &LifecycleGraph, node| graph.catalog.slot(node).unwrap();
        let (two, four) = (slot_of(&graph, 2), slot_of(&graph, 4));
        graph.link(2, 4).unwrap();
        graph.link(4, 5).unwrap();
        assert!(graph.remove_node(2));
        assert!(graph.remove_node(4));
        assert_eq!(graph.catalog.vacant, 2);
        let length = graph.catalog.slots.len();
        graph.link(20_000, 5).unwrap(); // reuses the slot of 4, the last freed
        graph.add_node(20_001).unwrap(); // then the slot of 2
        assert_eq!(slot_of(&graph, 20_000), four);
        assert_eq!(slot_of(&graph, 20_001), two);
        assert_eq!(graph.catalog.slots.len(), length);
        assert_eq!(graph.catalog.slot_reuses, 2);
        // The reused slots carry no stale incidence.
        assert_eq!(graph.degree(20_001), Some(0));
        assert_eq!(
            graph
                .incident_after(5, None)
                .unwrap()
                .map(|e| e.0)
                .collect::<Vec<_>>(),
            [20_000]
        );
        graph.validate();
    }

    /// A slot compaction renumbers slots densely in ascending ID while every
    /// incident list keeps its order, so a cursor captured before it resolves
    /// to the same position after it; cursors naming a removed node or a
    /// never-registered ID work on both sides of the compaction.
    #[test]
    fn cursors_survive_slot_compaction() {
        const SPOKES: u64 = 3_000;
        let hub = 1 << 40;
        let spoke = |k: u64| (k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 8) | 1;
        let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs).unwrap();
        // Spokes in scrambled ID order, so slot order differs from ID order.
        for k in 0..SPOKES {
            graph.link(hub, spoke(k)).unwrap();
        }
        let filler: Vec<NodeId> = (0..SPOKES).map(|k| 2 * k + (1 << 62)).collect();
        for &node in &filler {
            graph.add_node(node).unwrap();
        }
        let mut sorted: Vec<NodeId> = (0..SPOKES).map(spoke).collect();
        sorted.sort_unstable();
        let page = |graph: &LifecycleGraph, cursor| -> Vec<NodeId> {
            graph
                .incident_after(hub, cursor)
                .unwrap()
                .take(100)
                .map(|e| e.0)
                .collect()
        };
        assert_eq!(page(&graph, None), sorted[..100]);
        let removed = sorted[99];
        assert!(graph.remove_node(removed));
        let unregistered = sorted[500] + 1; // even: never a spoke
        assert!(graph.node_active(unregistered).is_none());
        let cursors = [Some(removed), Some(unregistered), Some(sorted[1_234]), None];
        let before: Vec<Vec<NodeId>> = cursors.iter().map(|&c| page(&graph, c)).collect();
        assert_eq!(before[0], sorted[100..200]);
        assert_eq!(before[1], sorted[501..601]);
        assert_eq!(graph.catalog.slot_compactions, 0);
        for &node in &filler {
            assert!(graph.remove_node(node));
        }
        assert_eq!(graph.catalog.slot_compactions, 1, "the removals compacted");
        assert_eq!(graph.catalog.vacant, 0);
        assert_eq!(graph.catalog.slots.len(), graph.node_count());
        let ids: Vec<NodeId> = graph.catalog.slots.iter().map(|r| r.id).collect();
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "slots in ID order"
        );
        let after: Vec<Vec<NodeId>> = cursors.iter().map(|&c| page(&graph, c)).collect();
        assert_eq!(before, after);
        graph.validate();
    }

    /// Reference model for the differential test: ordered maps, a BFS for
    /// connectivity, no shared code with `LifecycleGraph`.
    #[derive(Default)]
    struct Model {
        nodes: BTreeMap<NodeId, bool>,
        /// Edge flags keyed by the ordered endpoint pair.
        edges: BTreeMap<(NodeId, NodeId), bool>,
        adjacency: BTreeMap<NodeId, BTreeSet<NodeId>>,
    }

    impl Model {
        fn key(a: NodeId, b: NodeId) -> (NodeId, NodeId) {
            (a.min(b), a.max(b))
        }

        fn add_node(&mut self, node: NodeId) -> Result<bool, GraphError> {
            if self.nodes.contains_key(&node) {
                return Ok(false);
            }
            self.nodes.insert(node, true);
            self.adjacency.insert(node, BTreeSet::new());
            Ok(true)
        }

        fn link(&mut self, a: NodeId, b: NodeId) -> Result<bool, GraphError> {
            GraphError::check_distinct(a, b)?;
            if self.edges.contains_key(&Self::key(a, b)) {
                return Ok(false);
            }
            self.add_node(a)?;
            self.add_node(b)?;
            self.edges.insert(Self::key(a, b), true);
            self.adjacency.get_mut(&a).unwrap().insert(b);
            self.adjacency.get_mut(&b).unwrap().insert(a);
            Ok(true)
        }

        fn cut(&mut self, a: NodeId, b: NodeId) -> Result<bool, GraphError> {
            GraphError::check_distinct(a, b)?;
            if self.edges.remove(&Self::key(a, b)).is_none() {
                return Ok(false);
            }
            self.adjacency.get_mut(&a).unwrap().remove(&b);
            self.adjacency.get_mut(&b).unwrap().remove(&a);
            Ok(true)
        }

        fn set_node_active(&mut self, node: NodeId, active: bool) -> Result<bool, LifecycleError> {
            let flag = self
                .nodes
                .get_mut(&node)
                .ok_or(GraphError::UnknownNode { node })?;
            Ok(std::mem::replace(flag, active) != active)
        }

        fn set_edge_enabled(
            &mut self,
            a: NodeId,
            b: NodeId,
            enabled: bool,
        ) -> Result<bool, LifecycleError> {
            GraphError::check_distinct(a, b)?;
            for node in [a, b] {
                if !self.nodes.contains_key(&node) {
                    return Err(GraphError::UnknownNode { node }.into());
                }
            }
            let flag = self
                .edges
                .get_mut(&Self::key(a, b))
                .ok_or(LifecycleError::UnknownEdge {
                    source: a,
                    target: b,
                })?;
            Ok(std::mem::replace(flag, enabled) != enabled)
        }

        fn remove_node(&mut self, node: NodeId) -> bool {
            if self.nodes.remove(&node).is_none() {
                return false;
            }
            for neighbor in self.adjacency.remove(&node).unwrap() {
                self.adjacency.get_mut(&neighbor).unwrap().remove(&node);
                self.edges.remove(&Self::key(node, neighbor));
            }
            true
        }

        fn state(&self, a: NodeId, b: NodeId) -> EdgeState {
            let enabled = self.edges[&Self::key(a, b)];
            EdgeState::new(enabled, enabled && self.nodes[&a] && self.nodes[&b])
        }

        fn connected(&self, a: NodeId, b: NodeId) -> Result<bool, GraphError> {
            for node in [a, b] {
                if !self.nodes.contains_key(&node) {
                    return Err(GraphError::UnknownNode { node });
                }
            }
            if !self.nodes[&a] || !self.nodes[&b] {
                return Ok(false);
            }
            let mut seen = BTreeSet::from([a]);
            let mut stack = vec![a];
            while let Some(node) = stack.pop() {
                if node == b {
                    return Ok(true);
                }
                for &next in &self.adjacency[&node] {
                    if self.state(node, next).effective && seen.insert(next) {
                        stack.push(next);
                    }
                }
            }
            Ok(false)
        }

        fn incident_after(
            &self,
            node: NodeId,
            after: Option<NodeId>,
        ) -> Option<Vec<(NodeId, EdgeState)>> {
            let neighbors = self.adjacency.get(&node)?;
            let start = after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
            Some(
                neighbors
                    .range((start, std::ops::Bound::Unbounded))
                    .map(|&n| (n, self.state(node, n)))
                    .collect(),
            )
        }

        fn counts(&self) -> [usize; 5] {
            let effective = self
                .edges
                .iter()
                .filter(|&(&(a, b), &enabled)| enabled && self.nodes[&a] && self.nodes[&b])
                .count();
            [
                self.nodes.len(),
                self.edges.len(),
                self.nodes.values().filter(|&&active| active).count(),
                self.edges.values().filter(|&&enabled| enabled).count(),
                effective,
            ]
        }
    }

    /// Deterministic SplitMix64 generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    /// Compares counts, every registered node's flag and degree, full
    /// paginated traversals of sampled nodes (with random page sizes) and
    /// single pages at random cursors (registered, removed or
    /// never-registered IDs), and sampled connectivity queries.
    fn assert_same(graph: &LifecycleGraph, model: &Model, universe: &[NodeId], rng: &mut Rng) {
        let counts = [
            graph.node_count(),
            graph.edge_count(),
            graph.active_node_count(),
            graph.enabled_edge_count(),
            graph.effective_edge_count(),
        ];
        assert_eq!(counts, model.counts());
        for (&node, &active) in &model.nodes {
            assert_eq!(graph.node_active(node), Some(active));
            assert_eq!(graph.degree(node), Some(model.adjacency[&node].len()));
        }
        let pick = |rng: &mut Rng| universe[rng.below(universe.len())];
        for round in 0..24 {
            let node = if round == 0 { universe[0] } else { pick(rng) };
            let Some(expected) = model.incident_after(node, None) else {
                assert!(graph.incident_after(node, None).is_none());
                continue;
            };
            let limit = 1 + rng.below(64);
            let mut paged = Vec::new();
            let mut cursor = None;
            loop {
                let page: Vec<_> = graph
                    .incident_after(node, cursor)
                    .unwrap()
                    .take(limit)
                    .collect();
                let Some(last) = page.last() else { break };
                cursor = Some(last.0);
                paged.extend(page);
            }
            assert_eq!(paged, expected, "node {node}");
            for _ in 0..4 {
                let cursor = Some(pick(rng));
                let got: Vec<_> = graph.incident_after(node, cursor).unwrap().collect();
                assert_eq!(Some(got), model.incident_after(node, cursor));
            }
        }
        for _ in 0..64 {
            let (a, b) = (pick(rng), pick(rng));
            assert_eq!(graph.connected(a, b), model.connected(a, b), "{a}-{b}");
        }
    }

    /// One random operation on both, comparing the results. `hub` takes a
    /// quarter of the first endpoints, so it collects a high degree.
    fn random_step(
        graph: &mut LifecycleGraph,
        model: &mut Model,
        universe: &[NodeId],
        rng: &mut Rng,
    ) {
        let pick = |rng: &mut Rng| universe[rng.below(universe.len())];
        let a = if rng.below(4) == 0 {
            universe[0]
        } else {
            pick(rng)
        };
        let b = pick(rng);
        let flag = rng.below(2) == 0;
        match rng.below(100) {
            0..=29 => assert_eq!(graph.link(a, b), model.link(a, b)),
            30..=39 => assert_eq!(graph.cut(a, b), model.cut(a, b)),
            40..=49 => assert_eq!(graph.add_node(b), model.add_node(b)),
            50..=61 => assert_eq!(
                graph.set_node_active(b, flag),
                model.set_node_active(b, flag)
            ),
            62..=79 => assert_eq!(
                graph.set_edge_enabled(a, b, flag),
                model.set_edge_enabled(a, b, flag)
            ),
            80..=84 => assert_eq!(graph.remove_node(b), model.remove_node(b)),
            85..=89 => assert_eq!(
                graph.edge_state(a, b),
                model
                    .edges
                    .get(&Model::key(a, b))
                    .map(|_| model.state(a, b))
            ),
            90..=94 => {
                let cursor = Some(pick(rng));
                let got = graph
                    .incident_after(a, cursor)
                    .map(|edges| edges.take(32).collect::<Vec<_>>());
                let mut expected = model.incident_after(a, cursor);
                if let Some(edges) = expected.as_mut() {
                    edges.truncate(32);
                }
                assert_eq!(got, expected);
            }
            _ => assert_eq!(graph.connected(a, b), model.connected(a, b)),
        }
    }

    /// Long seeded sequences against the reference model, alternating growth
    /// phases with heavy removal phases (85% of the registered nodes, each
    /// removal followed by a random operation), so that vacant slots are
    /// reused and slot compactions run; both are asserted.
    #[test]
    fn randomized_sequences_match_a_reference_model_through_slot_reuse_and_compaction() {
        // IDs are scrambled over the whole u64 range, so slot order and ID
        // order are unrelated; universe[0] is the hub.
        let universe: Vec<NodeId> = (0..4_096_u64)
            .map(|k| k.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17))
            .collect();
        for (engine, seeds, cycles) in [
            (EngineConfig::CompactBfs, 0..3_u64, 3),
            (EngineConfig::ClusterForest, 3..4_u64, 2),
        ] {
            for seed in seeds {
                let mut rng = Rng(seed.wrapping_mul(0x5851_f42d_4c95_7f2d) ^ 0x151);
                let mut graph = LifecycleGraph::new(engine).unwrap();
                let mut model = Model::default();
                for _ in 0..cycles {
                    for _ in 0..6_000 {
                        random_step(&mut graph, &mut model, &universe, &mut rng);
                    }
                    graph.validate();
                    assert_same(&graph, &model, &universe, &mut rng);
                    let mut victims: Vec<NodeId> = model.nodes.keys().copied().collect();
                    for index in (1..victims.len()).rev() {
                        victims.swap(index, rng.below(index + 1));
                    }
                    victims.truncate(victims.len() * 85 / 100);
                    for (index, &node) in victims.iter().enumerate() {
                        assert_eq!(graph.remove_node(node), model.remove_node(node));
                        random_step(&mut graph, &mut model, &universe, &mut rng);
                        if index % 512 == 0 {
                            graph.validate();
                        }
                    }
                    graph.validate();
                    assert_same(&graph, &model, &universe, &mut rng);
                }
                let catalog = &graph.catalog;
                assert!(catalog.slot_reuses > 0, "seed {seed}: no slot was reused");
                assert!(
                    catalog.slot_compactions > 0,
                    "seed {seed}: no slot compaction"
                );
            }
        }
    }
}
