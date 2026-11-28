//! The service's graph: the plain engine, or the lifecycle facade when
//! `KNOTREL_TOPOLOGY_LIFECYCLE` is enabled (M10), plus an optional ordered node
//! index for the read-only inspection routes (`KNOTREL_INSPECTION`).
//!
//! The choice is fixed at startup. In plain mode the service holds the same
//! [`ConnectivityGraph`] as before, with no topology catalog; every legacy
//! operation dispatches through one enum match. Every mutation goes through a
//! [`ServiceGraph`] method, which is also where the node index is maintained.

use std::collections::BTreeSet;
use std::ops::Bound;

use knotrel_core::{
    ConnectivityGraph, EngineConfig, GraphError, GraphLimits, IncidenceError, LifecycleError,
    LifecycleGraph,
};
/// Lifecycle counts, reported only by a lifecycle-enabled graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct LifecycleCounts {
    pub(crate) active_node_count: usize,
    pub(crate) enabled_edge_count: usize,
    pub(crate) effective_edge_count: usize,
}

/// The graph owned by the service, under the graph lock.
#[derive(Debug, Clone)]
pub(crate) struct ServiceGraph {
    topology: Topology,
    /// Registered node IDs in ascending order, kept only when inspection is
    /// enabled. Invariant: equal to the topology's registered node set after
    /// every public method returns.
    order: Option<BTreeSet<u64>>,
}

/// The engine or lifecycle facade.
#[derive(Debug, Clone)]
pub(crate) enum Topology {
    /// Lifecycle support disabled: the plain engine.
    Plain(ConnectivityGraph),
    /// Lifecycle support enabled: registered topology with flags.
    Lifecycle(LifecycleGraph),
}

/// Most entries (nodes or incidences) one inspection page may examine.
///
/// Bounds the time a page holds the shared graph lock regardless of graph
/// size or filter selectivity; a page that hits it ends early with a cursor.
pub(crate) const INSPECTION_SCAN_BUDGET: usize = 10_000;

/// Largest page size accepted by the inspection routes.
pub(crate) const INSPECTION_MAX_LIMIT: usize = 1000;

impl ServiceGraph {
    /// Creates an empty graph for the startup settings.
    ///
    /// `lifecycle` with an unsupported engine is unreachable:
    /// [`ServerConfig::with_topology_lifecycle`](crate::ServerConfig::with_topology_lifecycle)
    /// rejects it and is the only way to enable lifecycle support.
    pub(crate) fn empty(
        engine: EngineConfig,
        limits: GraphLimits,
        lifecycle: bool,
        inspection: bool,
    ) -> Self {
        let topology = if lifecycle {
            Topology::Lifecycle(
                LifecycleGraph::with_limits(engine, limits)
                    .expect("ServerConfig admits lifecycle support only for supported engines"),
            )
        } else {
            Topology::Plain(ConnectivityGraph::with_limits(engine, limits))
        };
        Self::from_parts(topology, inspection.then(BTreeSet::new))
    }

    /// Wraps an import candidate built off-lock. `order` must hold exactly
    /// the candidate's registered node IDs, or be `None` without inspection.
    ///
    /// With inspection, a plain `compact-bfs` engine also starts keeping the
    /// ID of every internal index (8 bytes per node) for neighbor listing.
    /// Without inspection nothing extra is kept. The lifecycle facade lists
    /// neighbors from its own catalog and needs nothing extra.
    pub(crate) fn from_parts(mut topology: Topology, order: Option<BTreeSet<u64>>) -> Self {
        debug_assert!(order.as_ref().is_none_or(|order| order.len()
            == match &topology {
                Topology::Plain(graph) => graph.node_count(),
                Topology::Lifecycle(graph) => graph.node_count(),
            }));
        if order.is_some()
            && let Topology::Plain(graph) = &mut topology
        {
            graph.enable_neighbor_listing();
        }
        Self { topology, order }
    }

    /// Whether lifecycle operations are supported.
    pub(crate) fn is_lifecycle(&self) -> bool {
        matches!(self.topology, Topology::Lifecycle(_))
    }

    /// Whether the inspection routes are enabled (the node index is kept).
    pub(crate) fn is_inspectable(&self) -> bool {
        self.order.is_some()
    }

    pub(crate) fn engine(&self) -> EngineConfig {
        match &self.topology {
            Topology::Plain(graph) => graph.engine(),
            Topology::Lifecycle(graph) => graph.engine(),
        }
    }

    pub(crate) fn limits(&self) -> GraphLimits {
        match &self.topology {
            Topology::Plain(graph) => graph.limits(),
            Topology::Lifecycle(graph) => graph.limits(),
        }
    }

    /// Registered nodes (all nodes in plain mode).
    pub(crate) fn node_count(&self) -> usize {
        match &self.topology {
            Topology::Plain(graph) => graph.node_count(),
            Topology::Lifecycle(graph) => graph.node_count(),
        }
    }

    /// Registered edges (all edges in plain mode).
    pub(crate) fn edge_count(&self) -> usize {
        match &self.topology {
            Topology::Plain(graph) => graph.edge_count(),
            Topology::Lifecycle(graph) => graph.edge_count(),
        }
    }

    /// Lifecycle counts; `None` in plain mode. `O(1)`.
    pub(crate) fn lifecycle_counts(&self) -> Option<LifecycleCounts> {
        match &self.topology {
            Topology::Plain(_) => None,
            Topology::Lifecycle(graph) => Some(LifecycleCounts {
                active_node_count: graph.active_node_count(),
                enabled_edge_count: graph.enabled_edge_count(),
                effective_edge_count: graph.effective_edge_count(),
            }),
        }
    }

    fn contains_node(&self, node: u64) -> bool {
        match &self.topology {
            Topology::Plain(graph) => graph.contains_node(node),
            Topology::Lifecycle(graph) => graph.node_active(node).is_some(),
        }
    }

    pub(crate) fn add_node(&mut self, node: u64) -> Result<bool, GraphError> {
        let changed = match &mut self.topology {
            Topology::Plain(graph) => graph.add_node(node),
            Topology::Lifecycle(graph) => graph.add_node(node),
        }?;
        if changed && let Some(order) = &mut self.order {
            order.insert(node);
        }
        Ok(changed)
    }

    pub(crate) fn link(&mut self, source: u64, target: u64) -> Result<bool, GraphError> {
        // An O(1) membership probe per endpoint keeps the O(log V) index update
        // off links between existing nodes. A rejected link registers nothing.
        let new_endpoints = self
            .order
            .is_some()
            .then(|| (!self.contains_node(source), !self.contains_node(target)));
        let changed = match &mut self.topology {
            Topology::Plain(graph) => graph.link(source, target),
            Topology::Lifecycle(graph) => graph.link(source, target),
        }?;
        if let (Some(order), Some((new_source, new_target))) = (&mut self.order, new_endpoints) {
            if new_source {
                order.insert(source);
            }
            if new_target {
                order.insert(target);
            }
        }
        Ok(changed)
    }

    pub(crate) fn cut(&mut self, source: u64, target: u64) -> Result<bool, GraphError> {
        match &mut self.topology {
            Topology::Plain(graph) => graph.cut(source, target),
            Topology::Lifecycle(graph) => graph.cut(source, target),
        }
    }

    pub(crate) fn connected(&self, source: u64, target: u64) -> Result<bool, GraphError> {
        match &self.topology {
            Topology::Plain(graph) => graph.connected(source, target),
            Topology::Lifecycle(graph) => graph.connected(source, target),
        }
    }

    fn lifecycle_mut(&mut self) -> Result<&mut LifecycleGraph, LifecycleUnsupported> {
        match &mut self.topology {
            Topology::Plain(_) => Err(LifecycleUnsupported),
            Topology::Lifecycle(graph) => Ok(graph),
        }
    }

    /// The lifecycle facade, for read-only queries. Callers reject lifecycle
    /// requests for a plain graph before execution, so `Err` is a
    /// request-validation bug that surfaces as an error, never as a silent no-op.
    pub(crate) fn lifecycle(&self) -> Result<&LifecycleGraph, LifecycleUnsupported> {
        match &self.topology {
            Topology::Plain(_) => Err(LifecycleUnsupported),
            Topology::Lifecycle(graph) => Ok(graph),
        }
    }

    /// Sets a node's `active` flag (lifecycle support only).
    pub(crate) fn set_node_active(&mut self, node: u64, active: bool) -> LifecycleResult {
        Ok(self.lifecycle_mut()?.set_node_active(node, active)?)
    }

    /// Sets a registered edge's `enabled` flag (lifecycle support only).
    pub(crate) fn set_edge_enabled(
        &mut self,
        source: u64,
        target: u64,
        enabled: bool,
    ) -> LifecycleResult {
        Ok(self
            .lifecycle_mut()?
            .set_edge_enabled(source, target, enabled)?)
    }

    /// Removes a node and its incident edges (lifecycle support only).
    pub(crate) fn remove_node(&mut self, node: u64) -> LifecycleResult {
        let removed = self.lifecycle_mut()?.remove_node(node);
        if removed && let Some(order) = &mut self.order {
            order.remove(&node);
        }
        Ok(removed)
    }

    // ----- Inspection (read-only; see docs/http-api.md "Inspection") -----

    fn order(&self) -> Result<&BTreeSet<u64>, InspectError> {
        self.order.as_ref().ok_or(InspectError::Disabled)
    }

    /// One node's inspection record, or `None` if it is not registered.
    fn node_record(&self, node: u64) -> Option<NodeRecord> {
        match &self.topology {
            Topology::Lifecycle(graph) => Some(NodeRecord {
                node,
                active: graph.node_active(node)?,
                degree: graph.degree(node),
            }),
            Topology::Plain(graph) => graph.contains_node(node).then(|| NodeRecord {
                node,
                active: true,
                degree: graph.as_compact().and_then(|compact| compact.degree(node)),
            }),
        }
    }

    /// Looks up one node. `O(1)` expected.
    pub(crate) fn inspect_node(&self, node: u64) -> Result<NodeRecord, InspectError> {
        self.order()?;
        self.node_record(node).ok_or(InspectError::UnknownNode)
    }

    /// Lists nodes in ascending ID after `after`.
    ///
    /// Examines at most [`INSPECTION_SCAN_BUDGET`] nodes: `O(log V)` to seek
    /// plus `O(budget)` expected lookups, independent of `V`.
    pub(crate) fn list_nodes(
        &self,
        after: Option<u64>,
        limit: usize,
        filter: NodeFilter,
    ) -> Result<NodePage, InspectError> {
        let order = self.order()?;
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut nodes = Vec::with_capacity(limit.min(INSPECTION_MAX_LIMIT));
        let mut last = None;
        for (visited, &node) in order.range((lower, Bound::Unbounded)).enumerate() {
            if nodes.len() == limit || visited == INSPECTION_SCAN_BUDGET {
                return Ok(NodePage {
                    nodes,
                    next_after: last,
                });
            }
            last = Some(node);
            let record = self
                .node_record(node)
                .expect("the node index matches the registered topology");
            if filter.matches(record.active) {
                nodes.push(record);
            }
        }
        Ok(NodePage {
            nodes,
            next_after: None,
        })
    }

    /// Visits `node`'s incident edges after neighbor `after`, in the
    /// engine's incidence order, calling `visit` until it returns `false`.
    fn for_each_incident(
        &self,
        node: u64,
        after: Option<u64>,
        mut visit: impl FnMut(EdgeRecord) -> bool,
    ) -> Result<(), InspectError> {
        match &self.topology {
            Topology::Lifecycle(graph) => {
                let edges = graph
                    .incident_after(node, after)
                    .ok_or(InspectError::UnknownNode)?;
                for (neighbor, state) in edges {
                    if !visit(EdgeRecord {
                        neighbor,
                        enabled: state.enabled,
                        effective: state.effective,
                    }) {
                        break;
                    }
                }
            }
            Topology::Plain(graph) => {
                let compact = graph.as_compact().ok_or(InspectError::Unsupported)?;
                let neighbors = compact
                    .neighbors_after(node, after)
                    .map_err(inspect_incidence_error)?;
                for neighbor in neighbors {
                    if !visit(EdgeRecord {
                        neighbor,
                        enabled: true,
                        effective: true,
                    }) {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Lists `node`'s incident edges after neighbor `after`.
    ///
    /// `O(log deg)` to position plus at most [`INSPECTION_SCAN_BUDGET`]
    /// examined incidences.
    pub(crate) fn list_incident(
        &self,
        node: u64,
        after: Option<u64>,
        limit: usize,
        filter: EdgeFilter,
    ) -> Result<IncidentPage, InspectError> {
        self.order()?;
        let record = self.node_record(node).ok_or(InspectError::UnknownNode)?;
        let mut edges = Vec::with_capacity(limit.min(INSPECTION_MAX_LIMIT));
        let mut last = None;
        let mut visited = 0;
        let mut exhausted = true;
        self.for_each_incident(node, after, |edge| {
            if edges.len() == limit || visited == INSPECTION_SCAN_BUDGET {
                exhausted = false;
                return false;
            }
            visited += 1;
            last = Some(edge.neighbor);
            if filter.matches(edge) {
                edges.push(edge);
            }
            true
        })?;
        Ok(IncidentPage {
            node: record,
            edges,
            next_after: if exhausted { None } else { last },
        })
    }

    /// Lists edges once each, as `(source, target)` with `source < target`,
    /// ordered by `source` and then by `source`'s incidence order.
    ///
    /// `after` is the resume position: `(source, Some(target))` continues
    /// inside `source`'s incidences after `target`; `(source, None)` continues
    /// with the first node after `source`. Examines at most
    /// [`INSPECTION_SCAN_BUDGET`] nodes plus incidences: `O(log V)` to seek
    /// plus `O(budget)`, independent of `V`.
    ///
    /// Invariant for the returned `next`: every entry before it has been
    /// examined. A node is entered only with at least two budget units left,
    /// so a page cut short inside a node has examined at least one of its
    /// incidences and can resume after it.
    pub(crate) fn list_edges(
        &self,
        after: Option<(u64, Option<u64>)>,
        limit: usize,
        filter: EdgeFilter,
    ) -> Result<EdgePage, InspectError> {
        let order = self.order()?;
        if let Topology::Plain(graph) = &self.topology
            && graph.as_compact().is_none()
        {
            return Err(InspectError::Unsupported);
        }
        let lower = match after {
            None => Bound::Unbounded,
            Some((source, Some(_))) => Bound::Included(source),
            Some((source, None)) => Bound::Excluded(source),
        };
        let mut edges = Vec::with_capacity(limit.min(INSPECTION_MAX_LIMIT));
        let mut budget = INSPECTION_SCAN_BUDGET;
        let mut resume = None;
        for &source in order.range((lower, Bound::Unbounded)) {
            if edges.len() == limit || budget < 2 {
                return Ok(EdgePage {
                    edges,
                    next: resume,
                });
            }
            budget -= 1;
            // The incidence cursor applies only to the node it was issued for.
            let incident_after = match after {
                Some((cursor_source, Some(target))) if cursor_source == source => Some(target),
                _ => None,
            };
            let mut last_neighbor = None;
            let mut cut_short = false;
            self.for_each_incident(source, incident_after, |edge| {
                if edges.len() == limit || budget == 0 {
                    cut_short = true;
                    return false;
                }
                budget -= 1;
                last_neighbor = Some(edge.neighbor);
                if edge.neighbor > source && filter.matches(edge) {
                    edges.push(PairRecord {
                        source,
                        target: edge.neighbor,
                        enabled: edge.enabled,
                        effective: edge.effective,
                    });
                }
                true
            })?;
            if cut_short {
                return Ok(EdgePage {
                    edges,
                    next: Some((source, last_neighbor)),
                });
            }
            resume = Some((source, None));
        }
        Ok(EdgePage { edges, next: None })
    }
}

impl From<ConnectivityGraph> for ServiceGraph {
    /// Wraps an existing graph (the legacy `router(Graph)`), without inspection.
    fn from(graph: ConnectivityGraph) -> Self {
        Self {
            topology: Topology::Plain(graph),
            order: None,
        }
    }
}

/// Node state filter of `GET /v1/nodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeFilter {
    All,
    Active,
    Inactive,
}

impl NodeFilter {
    fn matches(self, active: bool) -> bool {
        match self {
            Self::All => true,
            Self::Active => active,
            Self::Inactive => !active,
        }
    }
}

/// Edge state filter of the edge listing routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeFilter {
    All,
    Enabled,
    Disabled,
    Effective,
    Ineffective,
}

impl EdgeFilter {
    fn matches(self, edge: EdgeRecord) -> bool {
        match self {
            Self::All => true,
            Self::Enabled => edge.enabled,
            Self::Disabled => !edge.enabled,
            Self::Effective => edge.effective,
            Self::Ineffective => !edge.effective,
        }
    }
}

/// One registered node as reported by inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NodeRecord {
    pub(crate) node: u64,
    /// Always `true` without lifecycle support.
    pub(crate) active: bool,
    /// Registered degree; `None` for engines without per-node adjacency.
    pub(crate) degree: Option<usize>,
}

/// One incident edge, seen from the node being inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeRecord {
    pub(crate) neighbor: u64,
    pub(crate) enabled: bool,
    pub(crate) effective: bool,
}

/// One edge of the global listing (`source < target`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PairRecord {
    pub(crate) source: u64,
    pub(crate) target: u64,
    pub(crate) enabled: bool,
    pub(crate) effective: bool,
}

/// A page of `GET /v1/nodes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodePage {
    pub(crate) nodes: Vec<NodeRecord>,
    /// Cursor of the next page; `None` when the traversal is complete.
    pub(crate) next_after: Option<u64>,
}

/// A page of `GET /v1/nodes/{node}/edges`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncidentPage {
    pub(crate) node: NodeRecord,
    pub(crate) edges: Vec<EdgeRecord>,
    pub(crate) next_after: Option<u64>,
}

/// A page of `GET /v1/edges`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgePage {
    pub(crate) edges: Vec<PairRecord>,
    /// Resume position: `(source, Some(target))` or `(source, None)`.
    pub(crate) next: Option<(u64, Option<u64>)>,
}

/// Why an inspection request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectError {
    /// `KNOTREL_INSPECTION` is not enabled.
    Disabled,
    /// The engine keeps no per-node adjacency (`ett`, `hdt`, `cluster-forest`).
    Unsupported,
    /// The inspected node is not registered.
    UnknownNode,
    /// An incidence cursor names a node that was never registered.
    InvalidCursor,
}

/// Maps a core incidence-listing error to the inspection error it reports.
///
/// `NotEnabled` cannot occur in practice (inspection enables neighbor
/// listing on `compact-bfs`), and `IncidenceError` is `#[non_exhaustive]`:
/// both, like any variant a later core release adds, are reported as an
/// unsupported listing, never as a success.
fn inspect_incidence_error(error: IncidenceError) -> InspectError {
    match error {
        IncidenceError::UnknownNode => InspectError::UnknownNode,
        IncidenceError::UnknownCursor => InspectError::InvalidCursor,
        _ => InspectError::Unsupported,
    }
}

/// A lifecycle operation reached a plain graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LifecycleUnsupported;

/// Result of a lifecycle mutation in service terms.
pub(crate) type LifecycleResult = Result<bool, LifecycleFailure>;

/// Why a lifecycle mutation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleFailure {
    /// The graph has no lifecycle support.
    Unsupported,
    /// A domain error from the facade.
    Domain(LifecycleError),
}

impl From<LifecycleUnsupported> for LifecycleFailure {
    fn from(_: LifecycleUnsupported) -> Self {
        Self::Unsupported
    }
}

impl From<LifecycleError> for LifecycleFailure {
    fn from(error: LifecycleError) -> Self {
        Self::Domain(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incidence_errors_map_to_inspection_errors() {
        assert_eq!(
            inspect_incidence_error(IncidenceError::UnknownNode),
            InspectError::UnknownNode
        );
        assert_eq!(
            inspect_incidence_error(IncidenceError::UnknownCursor),
            InspectError::InvalidCursor
        );
        // The fallback arm: never reported as a successful listing.
        assert_eq!(
            inspect_incidence_error(IncidenceError::NotEnabled),
            InspectError::Unsupported
        );
    }

    fn graph(lifecycle: bool, inspection: bool) -> ServiceGraph {
        ServiceGraph::empty(
            EngineConfig::CompactBfs,
            GraphLimits::default(),
            lifecycle,
            inspection,
        )
    }

    /// Asserts the node index equals the registered node set.
    fn assert_index_consistent(graph: &ServiceGraph) {
        let order = graph.order.as_ref().unwrap();
        assert_eq!(order.len(), graph.node_count());
        assert!(order.iter().all(|&node| graph.contains_node(node)));
    }

    fn all_nodes(graph: &ServiceGraph, filter: NodeFilter, limit: usize) -> Vec<u64> {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = graph.list_nodes(after, limit, filter).unwrap();
            seen.extend(page.nodes.iter().map(|n| n.node));
            match page.next_after {
                Some(next) => after = Some(next),
                None => return seen,
            }
        }
    }

    fn all_edges(graph: &ServiceGraph, filter: EdgeFilter, limit: usize) -> Vec<(u64, u64)> {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = graph.list_edges(after, limit, filter).unwrap();
            seen.extend(page.edges.iter().map(|e| (e.source, e.target)));
            match page.next {
                Some(next) => after = Some(next),
                None => return seen,
            }
        }
    }

    /// A plain graph has no lifecycle facade; a lifecycle graph routes the
    /// plain mutations to its registered topology.
    #[test]
    fn plain_and_lifecycle_variants_route_operations() {
        let mut plain = graph(false, false);
        assert_eq!(plain.lifecycle().err(), Some(LifecycleUnsupported));
        assert_eq!(plain.lifecycle_mut().err(), Some(LifecycleUnsupported));
        assert_eq!(
            plain.set_node_active(1, false),
            Err(LifecycleFailure::Unsupported)
        );
        assert_eq!(
            plain.set_edge_enabled(1, 2, false),
            Err(LifecycleFailure::Unsupported)
        );
        assert_eq!(plain.remove_node(1), Err(LifecycleFailure::Unsupported));
        assert_eq!(
            LifecycleFailure::from(LifecycleUnsupported),
            LifecycleFailure::Unsupported
        );
        let mut lifecycle = graph(true, false);
        assert_eq!(lifecycle.link(1, 2), Ok(true));
        assert_eq!(lifecycle.cut(1, 2), Ok(true));
        assert_eq!(lifecycle.connected(1, 2), Ok(false));
        assert!(lifecycle.lifecycle().is_ok());
        assert!(!lifecycle.is_inspectable());
        let _ = plain.link(1, 2);
    }

    /// Without inspection every route reports `Disabled` and no index exists.
    #[test]
    fn inspection_is_rejected_when_disabled() {
        let mut graph = graph(true, false);
        graph.link(1, 2).unwrap();
        assert!(graph.order.is_none());
        assert_eq!(graph.inspect_node(1), Err(InspectError::Disabled));
        assert_eq!(
            graph.list_nodes(None, 10, NodeFilter::All),
            Err(InspectError::Disabled)
        );
        assert_eq!(
            graph.list_incident(1, None, 10, EdgeFilter::All),
            Err(InspectError::Disabled)
        );
        assert_eq!(
            graph.list_edges(None, 10, EdgeFilter::All),
            Err(InspectError::Disabled)
        );
        let wrapped = ServiceGraph::from(ConnectivityGraph::new(EngineConfig::CompactBfs));
        assert!(!wrapped.is_inspectable());
        // Without inspection the plain engine keeps no index-to-ID vector.
        let mut plain = super::tests::graph(false, false);
        plain.link(1, 2).unwrap();
        assert!(matches!(
            &plain.topology,
            Topology::Plain(engine)
                if engine.as_compact().unwrap().neighbors_after(1, None).err()
                    == Some(IncidenceError::NotEnabled)
        ));
    }

    /// The index follows every node-set change, including rejected links
    /// (which register nothing) and lifecycle removal.
    #[test]
    fn node_index_tracks_registration_and_removal() {
        for lifecycle in [false, true] {
            let limits = GraphLimits {
                max_nodes: Some(4),
                max_edges: None,
            };
            let mut graph = ServiceGraph::empty(EngineConfig::CompactBfs, limits, lifecycle, true);
            graph.add_node(u64::MAX).unwrap();
            graph.link(1, 2).unwrap();
            graph.link(2, 3).unwrap();
            assert_index_consistent(&graph);
            assert_eq!(
                graph.link(5, 6),
                Err(GraphError::NodeLimitExceeded { limit: 4 })
            );
            assert_eq!(
                graph.add_node(7),
                Err(GraphError::NodeLimitExceeded { limit: 4 })
            );
            assert_eq!(graph.link(3, 3), Err(GraphError::SelfLoop { node: 3 }));
            assert_index_consistent(&graph);
            assert_eq!(all_nodes(&graph, NodeFilter::All, 2), [1, 2, 3, u64::MAX]);
            if lifecycle {
                assert_eq!(graph.remove_node(2), Ok(true));
                assert_eq!(graph.remove_node(2), Ok(false));
                assert_index_consistent(&graph);
                assert_eq!(all_nodes(&graph, NodeFilter::All, 10), [1, 3, u64::MAX]);
            }
        }
    }

    /// Node pages: ascending IDs, exact detail records, state filters, and a
    /// cursor that survives the removal of the node it names.
    #[test]
    fn node_pages_order_filter_and_detail() {
        let mut graph = graph(true, true);
        for node in [40, 10, 9_007_199_254_740_993, 30, 20] {
            graph.add_node(node).unwrap();
        }
        graph.link(10, 20).unwrap();
        graph.set_node_active(30, false).unwrap();
        let page = graph.list_nodes(None, 2, NodeFilter::All).unwrap();
        assert_eq!(
            page.nodes.iter().map(|n| n.node).collect::<Vec<_>>(),
            [10, 20]
        );
        assert_eq!(page.next_after, Some(20));
        assert_eq!(
            graph.inspect_node(10),
            Ok(NodeRecord {
                node: 10,
                active: true,
                degree: Some(1)
            })
        );
        assert_eq!(graph.inspect_node(11), Err(InspectError::UnknownNode));
        assert_eq!(all_nodes(&graph, NodeFilter::Inactive, 1), [30]);
        assert_eq!(
            all_nodes(&graph, NodeFilter::Active, 3),
            [10, 20, 40, 9_007_199_254_740_993]
        );
        graph.remove_node(20).unwrap();
        let rest = graph.list_nodes(Some(20), 10, NodeFilter::All).unwrap();
        assert_eq!(
            rest.nodes.iter().map(|n| n.node).collect::<Vec<_>>(),
            [30, 40, 9_007_199_254_740_993]
        );
        assert_eq!(rest.next_after, None);
        // An empty graph has an empty, complete first page.
        let empty = super::tests::graph(false, true);
        assert_eq!(
            empty.list_nodes(None, 5, NodeFilter::All),
            Ok(NodePage {
                nodes: vec![],
                next_after: None
            })
        );
    }

    /// A selective filter over many nodes stops at the scan budget and
    /// resumes from the last examined node.
    #[test]
    fn sparse_filters_respect_the_scan_budget() {
        let mut graph = graph(true, true);
        let total = INSPECTION_SCAN_BUDGET as u64 + 5;
        for node in 0..total {
            graph.add_node(node).unwrap();
        }
        graph.set_node_active(total - 1, false).unwrap();
        let first = graph.list_nodes(None, 10, NodeFilter::Inactive).unwrap();
        assert!(first.nodes.is_empty());
        assert_eq!(first.next_after, Some(INSPECTION_SCAN_BUDGET as u64 - 1));
        let second = graph
            .list_nodes(first.next_after, 10, NodeFilter::Inactive)
            .unwrap();
        assert_eq!(
            second.nodes.iter().map(|n| n.node).collect::<Vec<_>>(),
            [total - 1]
        );
        assert_eq!(second.next_after, None);
        // Edges: a long run of isolated nodes is crossed over several pages.
        assert_eq!(
            all_edges(&graph, EdgeFilter::All, 10),
            Vec::<(u64, u64)>::new()
        );
    }

    /// Incident pages in plain mode follow registration order; in lifecycle
    /// mode, ascending neighbor ID with flags.
    #[test]
    fn incident_pages_in_both_modes() {
        let mut plain = graph(false, true);
        for neighbor in [9, 1, 7] {
            plain.link(5, neighbor).unwrap();
        }
        let page = plain.list_incident(5, None, 2, EdgeFilter::All).unwrap();
        assert_eq!(
            page.edges.iter().map(|e| e.neighbor).collect::<Vec<_>>(),
            [9, 1]
        );
        assert!(page.edges.iter().all(|e| e.enabled && e.effective));
        assert_eq!(page.next_after, Some(1));
        let rest = plain.list_incident(5, Some(1), 2, EdgeFilter::All).unwrap();
        assert_eq!(
            rest.edges.iter().map(|e| e.neighbor).collect::<Vec<_>>(),
            [7]
        );
        assert_eq!(rest.next_after, None);
        assert_eq!(
            rest.node,
            NodeRecord {
                node: 5,
                active: true,
                degree: Some(3)
            }
        );
        assert_eq!(
            plain.list_incident(5, Some(424_242), 2, EdgeFilter::All),
            Err(InspectError::InvalidCursor)
        );
        assert_eq!(
            plain.list_incident(6, None, 2, EdgeFilter::All),
            Err(InspectError::UnknownNode)
        );
        assert!(
            plain
                .list_incident(5, None, 5, EdgeFilter::Disabled)
                .unwrap()
                .edges
                .is_empty()
        );

        let mut lifecycle = graph(true, true);
        for neighbor in [9, 1, 7] {
            lifecycle.link(5, neighbor).unwrap();
        }
        lifecycle.set_edge_enabled(5, 7, false).unwrap();
        lifecycle.set_node_active(9, false).unwrap();
        let page = lifecycle
            .list_incident(5, None, 10, EdgeFilter::All)
            .unwrap();
        assert_eq!(
            page.edges,
            [
                EdgeRecord {
                    neighbor: 1,
                    enabled: true,
                    effective: true
                },
                EdgeRecord {
                    neighbor: 7,
                    enabled: false,
                    effective: false
                },
                EdgeRecord {
                    neighbor: 9,
                    enabled: true,
                    effective: false
                },
            ]
        );
        let ineffective = lifecycle
            .list_incident(5, None, 10, EdgeFilter::Ineffective)
            .unwrap();
        assert_eq!(ineffective.edges.len(), 2);
        let enabled = lifecycle
            .list_incident(5, None, 10, EdgeFilter::Enabled)
            .unwrap();
        assert_eq!(enabled.edges.len(), 2);
        let effective = lifecycle
            .list_incident(5, None, 10, EdgeFilter::Effective)
            .unwrap();
        assert_eq!(effective.edges.len(), 1);
        assert_eq!(
            lifecycle
                .list_incident(5, Some(424_242), 10, EdgeFilter::All)
                .unwrap()
                .edges
                .len(),
            0
        );
    }

    /// Engines without per-node adjacency support node listing only.
    #[test]
    fn experimental_engines_list_nodes_but_not_edges() {
        for engine in [
            EngineConfig::EulerTour,
            EngineConfig::Hdt,
            EngineConfig::ClusterForest,
        ] {
            let mut graph = ServiceGraph::empty(engine, GraphLimits::default(), false, true);
            graph.link(1, 2).unwrap();
            assert_eq!(all_nodes(&graph, NodeFilter::All, 10), [1, 2]);
            assert_eq!(
                graph.inspect_node(1),
                Ok(NodeRecord {
                    node: 1,
                    active: true,
                    degree: None
                })
            );
            assert_eq!(
                graph.list_incident(1, None, 10, EdgeFilter::All),
                Err(InspectError::Unsupported)
            );
            assert_eq!(
                graph.list_edges(None, 10, EdgeFilter::All),
                Err(InspectError::Unsupported)
            );
        }
    }

    /// Every edge is listed exactly once, with `source < target`, for every
    /// page size and in both modes.
    #[test]
    fn edge_pages_list_each_edge_once() {
        for lifecycle in [false, true] {
            let mut graph = graph(lifecycle, true);
            let mut expected = Vec::new();
            for (a, b) in [(3, 1), (1, 2), (2, 3), (u64::MAX, 1), (4, 5), (9, 8)] {
                graph.link(a, b).unwrap();
                expected.push((a.min(b), a.max(b)));
            }
            expected.sort_unstable();
            for limit in [1, 2, 3, 100] {
                let mut listed = all_edges(&graph, EdgeFilter::All, limit);
                listed.sort_unstable();
                assert_eq!(listed, expected, "lifecycle={lifecycle} limit={limit}");
            }
        }
        let mut lifecycle = graph(true, true);
        lifecycle.link(1, 2).unwrap();
        lifecycle.link(2, 3).unwrap();
        lifecycle.set_node_active(3, false).unwrap();
        assert_eq!(all_edges(&lifecycle, EdgeFilter::Effective, 1), [(1, 2)]);
        assert_eq!(all_edges(&lifecycle, EdgeFilter::Ineffective, 1), [(2, 3)]);
        lifecycle.set_edge_enabled(1, 2, false).unwrap();
        assert_eq!(all_edges(&lifecycle, EdgeFilter::Disabled, 1), [(1, 2)]);
        assert_eq!(all_edges(&lifecycle, EdgeFilter::Enabled, 1), [(2, 3)]);
    }

    /// Mutations between pages: edges present throughout are listed exactly
    /// once; edges added or removed meanwhile may or may not appear.
    #[test]
    fn edge_traversal_survives_concurrent_changes() {
        for lifecycle in [false, true] {
            let mut graph = graph(lifecycle, true);
            for a in 0..30u64 {
                for b in (a + 1)..30 {
                    if (a * 7 + b) % 3 == 0 {
                        graph.link(a, b).unwrap();
                    }
                }
            }
            let stable: Vec<(u64, u64)> = all_edges(&graph, EdgeFilter::All, 1000)
                .into_iter()
                .filter(|(a, b)| (a + b) % 2 == 0)
                .collect();
            let mut seen = Vec::new();
            let mut after = None;
            let mut round = 0u64;
            loop {
                let page = graph.list_edges(after, 4, EdgeFilter::All).unwrap();
                seen.extend(page.edges.iter().map(|e| (e.source, e.target)));
                // Cut unstable edges (possibly the cursor's), add new ones, and in
                // lifecycle mode remove a node that holds no stable edge.
                for &PairRecord {
                    source: a,
                    target: b,
                    ..
                } in &page.edges
                {
                    if (a + b) % 2 == 1 {
                        graph.cut(a, b).unwrap();
                    }
                }
                round += 1;
                graph.link(100 + round, 200 + round).unwrap();
                if lifecycle && round == 3 {
                    graph.link(1_000, 1_001).unwrap();
                    graph.remove_node(1_000).unwrap();
                }
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
            for edge in &stable {
                assert_eq!(seen.iter().filter(|&&s| s == *edge).count(), 1, "{edge:?}");
            }
            assert_index_consistent(&graph);
        }
    }

    /// Import candidates carry an index built off-lock.
    #[test]
    fn from_parts_keeps_the_supplied_index() {
        let mut engine = ConnectivityGraph::new(EngineConfig::CompactBfs);
        engine.link(1, 2).unwrap();
        let graph = ServiceGraph::from_parts(Topology::Plain(engine), Some([1, 2].into()));
        assert_eq!(all_nodes(&graph, NodeFilter::All, 1), [1, 2]);
        assert!(!graph.is_lifecycle());
        assert_eq!(graph.engine(), EngineConfig::CompactBfs);
    }
}
