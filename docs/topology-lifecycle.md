# Node and edge lifecycle contract (M10, v0.5.0; storage updated in v0.6.0)

**Status:** frozen contract for v0.5.0 implementation (M10.1). It refines the
[M10 plan](superpowers/plans/2026-11-13-topology-lifecycle.md). Deviations
from the plan's proposals are listed in [Decisions](#decisions-and-deviations).

Knotrel remains an exact connectivity engine for undirected simple graphs.
Lifecycle support adds **registered topology** with independent flags, and
answers connectivity over the **effective graph** derived from it. It adds no
persistence, typed nodes, multi-graph support or electrical simulation.

## 1. Model

- **Registered node:** a node ID present in the topology. It has one flag,
  `active`.
- **Registered edge:** an unordered pair `{u, v}` of distinct registered
  nodes. It has one flag, `enabled`.
- **Effective edge:** a registered edge that is enabled **and** whose two
  endpoints are both active.
- **Effective graph:** the active nodes and the effective edges.
  `connected` answers on this graph only.

Flags describe intended state supplied by the application or source. They are
not measured electrical availability.

## 2. Operations

`true` from a mutation means one canonical state change; `false` means no
change. Every validation listed below happens **before** any mutation, so an
error never leaves a partial change.

| Operation | Effect | No-op (`false`) | Errors, in precedence order |
| --- | --- | --- | --- |
| `add_node(n)` | Registers `n` active and isolated. | `n` already registered, active or inactive. It is **not** reactivated. | node limit |
| `link(u, v)` | Registers missing endpoints as active, then registers `{u, v}` enabled. | Edge already registered, enabled or disabled. It is **not** re-enabled. | self-loop; node limit; edge limit (only for a new edge) |
| `cut(u, v)` | Removes the registered edge, effective or not. | Edge or an endpoint not registered. | self-loop |
| `set_node_active(n, a)` | Sets the node flag. Incident edges and their `enabled` flags are kept. | Flag already `a`. | unknown node |
| `set_edge_enabled(u, v, e)` | Sets the edge flag, also while an endpoint is inactive. | Flag already `e`. | self-loop; unknown node (`u` checked before `v`); unknown edge |
| `remove_node(n)` | Removes `n` and every incident registered edge, including disabled edges and edges to inactive neighbors. | `n` not registered. | none |
| `connected(u, v)` | Both endpoints are first checked for registration (`u`, then `v`). Only then: `true` iff a path exists in the effective graph; an inactive endpoint gives `false`, including `connected(n, n)`; an active `n` gives `connected(n, n) = true`. | — | unknown node (`u` checked before `v`), before any flag check |
| `node_active(n)` | `Some(active)` if registered, else `None`. | — | none |
| `edge_state(u, v)` | `Some({enabled, effective})` if registered, else `None`. `u == v` returns `None` in the core; HTTP reports `self_loop`. | — | none (core) |

Consequences:

- Reactivating a node restores exactly its incident edges that are enabled and
  whose other endpoint is active. A disabled edge stays disabled.
- An enabled edge with an inactive endpoint is registered and enabled but not
  effective.
- `remove_node(n)` followed by `add_node(n)` or `link(n, x)` creates a fresh
  active node with no edges. Removed edges never return implicitly. Before a
  compaction, the new registration reuses the removed node's retired engine
  vertex, which is isolated and carries no adjacency (section 5).
- Linking to an inactive endpoint registers an enabled edge that is not
  effective until the endpoint is activated.

## 3. Counts and limits

| Count | Meaning |
| --- | --- |
| `node_count` | Registered nodes, active or inactive. |
| `edge_count` | Registered edges, enabled or disabled, effective or not. |
| `active_node_count` | Registered nodes whose flag is active. |
| `enabled_edge_count` | Registered edges whose flag is enabled. |
| `effective_edge_count` | Edges in the effective graph. |

`max_nodes` and `max_edges` bound **registered** nodes and edges. Deactivating
or disabling never frees capacity; only `remove_node` and `cut` do.

Without lifecycle support nothing changes: `node_count` and `edge_count` keep
their meaning, and the additional counts are not reported.

## 4. Atomicity, revisions and failure

- Each lifecycle command is one indivisible step under the graph's exclusive
  mutable access (`&mut` in Rust, the graph lock in the server). No query
  observes a partly deactivated or partly removed node.
- In the server, one command that returns `changed: true` advances
  `state_version` by exactly one, whatever the number of incident edges. A
  flag change advances it even when connectivity does not change. No-ops and
  rejected commands do not advance it.
- `generation` changes only on import publication, never on removal or
  activation.
- Batches keep their contract: ordered, continue after a domain error, no
  rollback across operations.
- **Embedded failure contract:** a method that returns `Err` made no change.
  A panic during a mutation (an internal invariant violation or an allocation
  failure that unwinds) leaves the graph unspecified; discard it. The server
  already fails closed on worker panics: the service becomes unavailable and
  queued work is refused.

## 5. Engines, complexity and storage

The lifecycle facade (`knotrel_core::LifecycleGraph`) keeps a catalog of
registered topology and an inner engine. The inner engine is a
`ConnectivityGraph` created **without limits** (limits are enforced on the
catalog, so retired slots can never cause `NodeLimitExceeded`). Its vertices
are **every registered node, whatever its flag, plus retired slots**; its
edges are **exactly the effective edges**. No engine state machine is
duplicated and no engine deletion invariant is modified.

**Supported engines (v0.5.0).** Lifecycle support is available only with
`compact-bfs`. Lifecycle support for the experimental `ett` and `hdt` engines
is deferred: their resource behavior under lifecycle churn was not
established well enough for this release (see decision 8). Every
`LifecycleGraph` constructor returns `LifecycleError::UnsupportedEngine` for
them before examining any input, and the server refuses to start with
lifecycle support and either engine. No engine is ever substituted. The
plain (non-lifecycle) `ett` and `hdt` engines, their APIs and their server
configurations are unchanged.

**Costs.** Let `d` be the registered degree of the node concerned, `d_eff` its
effective degree, `U` the cost of one effective edge insertion or deletion in
the engine (for `compact-bfs`, the only engine with lifecycle support,
`O(d_max)` because adjacency lists are sorted vectors), and `V`,
`E_eff` the registered nodes and effective edges at the time of the call.
Hash-map lookups are expected `O(1)`, not worst case.

| Operation | Per call, worst case (hash lookups expected `O(1)`) | Amortized over a sequence |
| --- | --- | --- |
| `set_node_active` | `O(d)` catalog scan plus `d_eff × U` | same |
| `set_edge_enabled`, `link`, `cut` | `O(d_u + d_v)` catalog work plus at most one `U` | same |
| `remove_node` without compaction | `O(d + sum of neighbor degrees)` catalog work plus `d_eff × U` | same |
| `remove_node` that triggers compaction | the above plus an `O(V log V + E)` one-pass engine rebuild from the catalog | adds `O(log V + E / V)` rebuild work per removal, with `V` and `E` taken at rebuild time |
| `connected` | the engine's query cost plus two flag checks | same |

Deactivation and removal cut effective incident edges in descending neighbor
order. For `compact-bfs` this removes from the end of the node's sorted
adjacency vector, so a high-degree removal does not pay `O(d)` shifts per
edge.

None of these is logarithmic in general. Removing or deactivating a
high-degree hub costs work proportional to its degree, and a compacting
removal holds the server's graph lock for a whole-graph rebuild.

**Engine slots and compaction.** The engines do not support vertex deletion
and key vertices by node ID. A removed node's engine vertex stays as an
isolated *retired* slot: all its edges were cut, and since v0.6.0 its adjacency
allocation is released, so a retired slot keeps only its engine ID-map entry
and an empty vector header. Registering the **same** ID again reuses that
slot. After every removal, if retired slots exceed `max(V, 1024)`, the facade
rebuilds the engine from the catalog: all registered nodes (active or not) and
all effective edges. More than `max(V, 1024)` removals separate two rebuilds,
which gives the amortized term above. Engine vertices stay at most
`V + max(V, 1024)`, that is at most `2V + 1024`. Each rebuild holds the
server's graph lock. (v0.6.0 evaluated a `max(V / 4, 1024)` threshold and
rejected it: fresh-ID churn became about 90% slower.)

**Engine construction.** Bulk construction (`try_from_topology`, flagged
imports) and compaction build the `compact-bfs` engine in one pass from the
catalog (since v0.6.0): engine indices follow ascending node IDs, each
effective edge is found once from its smaller endpoint, and each adjacency
vector is allocated once with capacity equal to the node's enabled registered
degree. Index order is internal; no result depends on it.

**Memory.** Quantities are reported separately and never substituted for one
another: live state, container capacity, temporary peaks, allocator retention
and RSS.

| Structure | Live state (64-bit) | Retained capacity | Not bounded here |
| --- | --- | --- | --- |
| Catalog node map | one 32-byte record per registered node | up to the historical maximum of registered nodes, rounded to the table's power-of-two growth; under fresh-ID delete/insert churn the table may double once at more than half load | allocator overhead |
| Incident lists | 16 bytes per incidence, two per registered edge | each list at most `max(4 × length, 16)` elements (shrunk below a quarter) | — |
| Engine | one ID-map entry and one vector header per engine vertex; 8 bytes per effective incidence | per vertex: at least its historical maximum degree since the last rebuild (cuts do not shrink); after a rebuild, its enabled registered degree, which can exceed its effective degree | — |
| Retired slots | at most `max(V, 1024)` ID-map entries and vector headers | — | — |
| Compaction | the old and the new engine are live together during a rebuild, plus an `O(V)` sorted ID vector | — | — |
| HTTP import | the request body while it is parsed, then the decoded request, the catalog and the new engine; the old graph stays live until just after publication (atomic replace; it is freed after the graph lock is released). A body that fails the streaming parse is parsed a second time by the former parser to keep the error contract, with that parser's working set (as in v0.5.0) | — | RSS, allocator retention |

Measured values (allocator byte counts and the qualification results) are in
the v0.6.0 qualification evidence and release notes; they are observations on
one host, not ceilings. Process RSS has no bound from Knotrel: the allocator
may retain freed pages. (Independently of lifecycle support, the `ett` and
`hdt` engines free an incidence set's storage when it becomes empty, and `hdt`
releases an upper level's record of a vertex that becomes isolated there,
reusing the slot.)

## 6. Rust API (`knotrel-core`)

Additive; `Graph`, `ConnectivityGraph` and `GraphError` are unchanged.

```rust
pub struct LifecycleGraph { /* private */ }
pub struct NodeSpec { pub node: NodeId, pub active: bool }
pub struct EdgeSpec { pub source: NodeId, pub target: NodeId, pub enabled: bool }
pub struct EdgeState { pub enabled: bool, pub effective: bool }
#[non_exhaustive]
pub enum LifecycleError {
    Graph(GraphError),
    UnknownEdge { source: NodeId, target: NodeId },
    ConflictingNode { node: NodeId },
    ConflictingEdge { source: NodeId, target: NodeId },
    UnsupportedEngine { engine: EngineConfig },
}

impl LifecycleGraph {
    pub const fn supports_engine(engine: EngineConfig) -> bool; // compact-bfs only
    pub fn new(engine: EngineConfig) -> Result<Self, LifecycleError>;
    pub fn with_limits(engine: EngineConfig, limits: GraphLimits)
        -> Result<Self, LifecycleError>;
    pub fn try_from_topology(engine, limits, nodes: &[NodeSpec], edges: &[EdgeSpec])
        -> Result<Self, LifecycleError>;
    pub fn add_node(&mut self, node) -> Result<bool, GraphError>;
    pub fn link(&mut self, source, target) -> Result<bool, GraphError>;
    pub fn cut(&mut self, source, target) -> Result<bool, GraphError>;
    pub fn set_node_active(&mut self, node, active: bool) -> Result<bool, LifecycleError>;
    pub fn set_edge_enabled(&mut self, source, target, enabled: bool)
        -> Result<bool, LifecycleError>;
    pub fn remove_node(&mut self, node) -> bool;
    pub fn connected(&self, source, target) -> Result<bool, GraphError>;
    pub fn node_active(&self, node) -> Option<bool>;
    pub fn edge_state(&self, source, target) -> Option<EdgeState>;
    pub fn node_count / edge_count / active_node_count / enabled_edge_count
        / effective_edge_count(&self) -> usize;
    pub fn engine_vertex_count(&self) -> usize;   // includes retired slots
    pub fn storage_stats(&self) -> LifecycleStorage; // O(V): sizes and capacities
    pub fn limits(&self) -> GraphLimits;
    pub fn engine(&self) -> EngineConfig;
}
```

`new`, `with_limits` and `try_from_topology` return
`UnsupportedEngine { engine }` for `ett` and `hdt`; `try_from_topology` checks
the engine before any entry. `try_from_topology` registers `nodes`, then
`edges` (creating missing endpoints active). Repeated entries with identical flags are accepted; a node
or edge repeated with a different flag returns `ConflictingNode` or
`ConflictingEdge` (edges compare as unordered pairs). Self-loops and limits
return `LifecycleError::Graph`.

## 7. Server opt-in

`KNOTREL_TOPOLOGY_LIFECYCLE` is exactly `true` or `false` (default `false`),
also `ServerConfig::with_topology_lifecycle`. With `false`, the server holds
the plain engine as before and no catalog is allocated; every engine and mode
behaves as in v0.4.0 apart from the additive changes listed in the release
notes (lifecycle request names, events and flagged import entries get 409
`topology_lifecycle_disabled` instead of 422, and one more feeder-rejection
series).

`true` requires `KNOTREL_ENGINE=compact-bfs` (the default). With `ett` or
`hdt`, `ServerConfig::from_env` fails with "`KNOTREL_TOPOLOGY_LIFECYCLE`
requires `KNOTREL_ENGINE=compact-bfs`; node and edge lifecycle support for
`<engine>` is not available in this release", so the executable exits with
status 1 before binding or serving. Embedded callers get the same
`ConfigError` from `ServerConfig::with_topology_lifecycle(true)`, which
returns `Result<ServerConfig, ConfigError>`; the engine is fixed by
`ServerConfig::new`, so no router can be built with an unsupported
combination.

`GET /v1/info` adds `"topology_lifecycle": true` only when enabled.

### Direct operations (`/v1/operations`, `/v1/batch`)

New operation objects (all fields required, no others allowed):

```json
{"op":"set_node_active","node":"202","active":false}
{"op":"set_edge_enabled","source":"201","target":"202","enabled":false}
{"op":"remove_node","node":"202"}
{"op":"node_state","node":"202"}
{"op":"edge_state","source":"201","target":"202"}
```

Results: mutations return `{"result":"changed","changed":bool}`. Queries:

```json
{"result":"node_state","registered":true,"active":false}
{"result":"edge_state","registered":true,"enabled":true,"effective":false}
```

An absent object reports `registered: false` and `false` flags; it is not an
error. `edge_state` with `source == target` returns `self_loop`.

New error codes: `unknown_edge` (HTTP 404 for a single operation) and
`topology_lifecycle_disabled` (HTTP 409). `unknown_node` keeps HTTP 404.

**Request validation order.** The steps of the
[HTTP request validation order](http-api.md#request-validation-order) are
extended as follows; the first failing step decides the response:

- **Step 5 (shape, 422 `invalid_request`)** always knows the five new
  operation shapes, whatever the setting. A legacy operation carrying
  `active` or `enabled` fails here as an unknown field.
- **New step 5a (409 `topology_lifecycle_disabled`):** lifecycle support is
  off and the single operation, or any operation of a batch, is one of the
  five new operations.
- **Step 6 (409 `direct_mutation_disabled`):** in source-managed mode the
  mutation list becomes `add_node`, `link`, `cut`, `set_node_active`,
  `set_edge_enabled` and `remove_node`. `node_state` and `edge_state` are
  queries: source-managed query-only batches may contain `connected`,
  `node_state` and `edge_state`, with the same readiness rules as `connected`.
- Steps 7–9 are unchanged.

### Feeder events (`/v1/feeder/events`)

New `op` values, with the existing envelope fields:

| `op` | Required | Forbidden |
| --- | --- | --- |
| `set_node_active` | `node`, `active` | `source`, `target`, `enabled` |
| `set_edge_enabled` | `source`, `target`, `enabled` | `node`, `active` |
| `remove_node` | `node` | `source`, `target`, `active`, `enabled` |

`progress`, `link`, `cut` and `add_node` must not carry `active` or
`enabled`: 422 `invalid_request`, whatever the setting. A flag whose value is
`null` is a shape error (422) wherever a flag is accepted, so it is never
treated as absent. Feeder `set_edge_enabled` checks endpoints in the supplied
order, as direct operations do; only the duplicate fingerprint is
normalized.

With lifecycle support off, an event using one of the three new `op` values
rejects the whole request with 409 `topology_lifecycle_disabled`. The check
runs in the existing per-event prevalidation slot, under the graph lock after
session, generation and source checks and before any event is applied, in
event order together with `invalid_request` (the first failing event
decides). Within one event, a known lifecycle `op` without lifecycle support
is rejected with 409 before its fields are checked; an unknown `op` is still
422 `invalid_request`. It is counted in `knotrel_feeder_rejections_total` with
`code="topology_lifecycle_disabled"`, a new value of that closed label set.

The canonical operation used for duplicate detection includes the operation
type, normalized endpoints and the flag value. A retry with the same
`delivery_sequence` and `event_id` but a different flag or operation is a
conflicting duplicate. Domain errors follow the existing feeder failure
matrix: HTTP 422, a plain error with the domain code (`unknown_node`,
`unknown_edge` or `self_loop`) for a single-event request, and
`partial_failure` with outer code `event_failed` for a multi-event request.
Feeder events keep having no 404 outcome. `remove_node`
of an absent node is an applied no-op.

Stale-event safety comes from the existing ordering and fencing: a retained
duplicate is answered from the window without being applied, an evicted one is
`unverifiable_duplicate`, and a different sequence must be the next in order.
No event can resurrect removed topology except a new in-order `add_node` or
`link`.

### Import (`/v1/feeder/import`)

- `nodes` entries are either a node ID string (active) or an object
  `{"node":"202","active":false}`.
- `edges` entries accept an optional `"enabled": false` (default `true`).
- The imported graph contains every registered node and edge with its flags,
  including isolates, inactive nodes and disabled edges.
- Repeated entries must agree; a conflict is `invalid_node` or `invalid_edge`
  (HTTP 422). Legacy payloads are unchanged and mean all active, all enabled.
- With lifecycle support off, any node object or any `enabled` member rejects
  the import with `topology_lifecycle_disabled` (HTTP 409), even when the
  value is the default. The check belongs to the payload phase: after JSON
  parsing and the `instance_id`/`graph_id` checks, before the candidate is
  built and before the graph lock is requested. It is classified as
  `payload_rejected` in `knotrel_imports_total`.
- The candidate is built off-lock and published atomically, as before.

### Stats, import response and metrics

- `/v1/stats` and the import response keep `node_count` and `edge_count`
  (registered). With lifecycle support they add `active_node_count`,
  `enabled_edge_count` and `effective_edge_count`. `remaining_*` use
  registered counts.
- `knotrel_graph_nodes` and `knotrel_graph_edges` report registered counts.
  With lifecycle support only, three gauges are added:
  `knotrel_graph_active_nodes`, `knotrel_graph_enabled_edges` and
  `knotrel_graph_effective_edges`.
- `knotrel_graph_operations_total` gains `op` values `set_node_active`,
  `set_edge_enabled`, `remove_node` (outcomes `changed`, `unchanged`,
  `error`), `node_state` and `edge_state` (outcomes `registered`,
  `unregistered`, `error`). Their series exist only with lifecycle support
  and, as for other mutations, only in standalone mode for the mutations.
- `node_state` and `remove_node` cannot fail, so their `error` series exist
  for uniformity but never increment.
- The three new counts are maintained in `O(1)` per mutation by the core and
  read only when metrics are enabled, at the existing per-job publication
  boundary (observability contract section 10). With metrics disabled, or
  with lifecycle support off, no additional telemetry work is done.
- No label carries a node ID, edge, source cursor or event ID.

## Decisions and deviations

1. **Separate facade, not a mode of `ConnectivityGraph`.** Keeps the default
   path free of lifecycle bookkeeping and leaves `ConnectivityGraph`'s
   behavior and cost unchanged.
2. **Separate `LifecycleError` (non-exhaustive)** instead of new
   `GraphError` variants, so existing exhaustive matches still compile.
3. **Uniform retired-slot strategy with amortized rebuild** instead of
   per-engine vertex deletion. ETT and HDT deletion invariants are not
   modified.
4. **Absent objects in state queries are not errors** (`registered: false`),
   unlike `connected`, which keeps `unknown_node`.
5. **Whole-request rejection when lifecycle support is off,** consistent with
   `direct_mutation_disabled`, rather than per-item batch errors.
6. **Flags in import reject when lifecycle support is off even at the default
   value,** so a sender cannot assume flags were honored.
7. **Contract review (2026-11-16).** An independent review found no blockers.
   Its major findings were resolved in this text: `connected` checks
   registration before flags; the HTTP, feeder and import positions of
   `topology_lifecycle_disabled` are fixed; feeder domain errors keep 422; the
   cost table separates worst-case, compaction spikes and amortized terms; the
   memory statement is an estimate pending measurement.
8. **Lifecycle support limited to `compact-bfs` (owner decision B1,
   2026-11-16).** The qualification of earlier candidates found, on `ett`
   and `hdt` with lifecycle, removal regressions at high degree, heap growth
   failures, and for `hdt` retained storage that kept growing through 120
   cycles on one replayed path without an established practical steady state
   (no leak was demonstrated; per-level arenas are bounded by code but no
   practical bound for the whole process was established). Rather than
   accept those costs, `ett` and `hdt` lifecycle support is deferred. The
   rejection is explicit in the core constructors and at server
   configuration, never a silent engine switch. Those earlier results remain
   recorded for the candidates they measured; the exclusion does not turn
   them into passes.
