# Console data browser — plan and inspection API design

**Status (2026-11-20):** implemented and locally validated on
`feature/console-data-browser` (stacked on the Operational Console branch, PR
#60). External CI pending. Not released.

## Goal

Turn the console from an HTTP request panel into a tool for exploring,
querying and diagnosing a Knotrel server, using Redis Insight as a functional
reference (key browser, detail pane, workbench, diagnostics). The **data
browser is the center**: find nodes and edges without knowing IDs, inspect a
node and its incident edges, act on the selection when the server allows it.

Out of scope for this increment: log streaming, collection or search, a
request profiler, an audit system, a timeseries backend, global graph
visualization, bulk destructive operations, and session takeover in
source-managed mode.

## Why new server APIs

The existing API answers questions about IDs the client already knows
(`connected`, `node_state`, `edge_state`). Nothing enumerates nodes or edges,
and the engines keep nodes in hash maps without a usable order. A browser
needs read-only enumeration with stable pagination.

## Inspection API (read-only, opt-in)

Enabled by `KNOTREL_INSPECTION=true` (default `false`). When it is disabled
the routes return 409 `inspection_disabled`, and the server keeps no extra
state and pays no extra mutation cost. `GET /v1/info` reports
`"inspection": true` when it is enabled.

| Route | Result |
| --- | --- |
| `GET /v1/nodes?after=&limit=&state=` | Nodes in ascending ID order |
| `GET /v1/nodes/{node}` | One node: registered, active, registered degree |
| `GET /v1/nodes/{node}/edges?after=&limit=&state=` | Incident edges of one node |
| `GET /v1/edges?after_source=&after_target=&limit=&state=` | Edges, each reported once as `(source < target)` |

**Ordering and pagination.**
- Nodes are ordered by ascending numeric ID. `after` is an exclusive lower
  bound (a node ID).
- Incident edges are ordered by neighbor, in the engine's stable incidence
  order: ascending neighbor ID with lifecycle support, neighbor registration
  order for plain `compact-bfs`. `after` is the last neighbor returned.
- `/v1/edges` is ordered by the smaller endpoint (ascending), then by that
  endpoint's incidence order. Its cursor is the last edge returned.
- `limit` is 1–1000 (default 100). `next_*` fields carry the cursor for the
  next page; `null` means the traversal is complete.
- A page may hold fewer than `limit` items while `next_*` is non-null. Each
  page examines at most 10 000 nodes or incidences (the scan budget), so a
  sparse filter cannot hold the lock for long. Follow the cursor.

**Filters.**
- Nodes: `state` ∈ {`all`, `active`, `inactive`}. Without lifecycle support
  every node is active.
- Edges: `state` ∈ {`all`, `enabled`, `disabled`, `effective`, `ineffective`}.
  Without lifecycle support every edge is enabled and effective.
- No query language: an exact ID is looked up with `GET /v1/nodes/{node}`.

**Concurrent changes and cursors.** Each page is computed under one
shared-lock acquisition and carries the version headers of that instant.
Pages are **not** a snapshot of the graph.
- An element present for the whole traversal is returned exactly once.
- Elements added or removed during the traversal may or may not appear.
- A cursor is a position (an ID or an edge), not a token: it never expires and
  stays valid after the element it names is removed.
- When `X-Knotrel-Instance-Id` or `X-Knotrel-Generation` changes between
  pages, the traversal spans different graphs. The console then restarts it.
- A plain `compact-bfs` incidence cursor must name a registered node (nodes
  are never removed in plain mode); otherwise the request gets 400
  `invalid_cursor`.

**Engines.** Node listing and node detail work with every engine. Incident
and edge listing need per-node adjacency: `compact-bfs` with or without
lifecycle support. `ett` and `hdt` return 409 `inspection_unsupported` for
those routes and report `degree: null`.

**Admission and modes.**
- The routes use the same bounded admission, blocking worker and shared graph
  lock as `GET /v1/stats` (job kind `inspect`).
- They are reads: available in standalone and source-managed modes, never
  mutating, with no readiness gate (diagnostics), exactly like `/v1/stats`.
- Draining and unavailable behavior is unchanged.

**Costs.**

| Item | Cost |
| --- | --- |
| Page | O(log V) seek plus at most 10 000 visited entries plus O(limit) output, under a shared lock. Independent of graph size. |
| Node detail | O(1) expected |
| Extra state (only when enabled) | A `BTreeSet<u64>` of registered node IDs: about 10–20 bytes per node, O(log V) insert or remove when a node is created or removed, built off-lock during import |
| Also only when enabled (plain `compact-bfs`) | The core `Graph` keeps an index→ID vector for neighbor listing: 8 bytes per node, one `push` when a node is registered. No change for edge operations. |

These costs are measured before any claim is made; see
[the cost check](#cost-check).

## Console

- **Navigation adapts to the mode:** Overview, Browser, Workbench,
  Performance, and Synchronization (source-managed only).
- **Browser:**
  - node and edge lists with cursor pagination and state filters;
  - exact ID lookup;
  - detail pane: flags, degree, incident edges paginated, with
    registered/enabled/effective definitions;
  - navigation edge → endpoints and node → incident edges;
  - filters and selection in the URL hash;
  - empty, loading, error and stale states;
  - restart prompt when the instance or generation changes.
- **Contextual operations** in the detail pane, limited to what the mode and
  configuration allow: link a neighbor, cut, enable or disable, activate or
  deactivate, remove with typed confirmation. Each mutation is sent once, then
  the detail is re-read; a removed element is reported as such.
- **Workbench:**
  - query forms;
  - a batch editor that validates against the contract before sending and
    shows per-operation results;
  - raw response view;
  - bounded local history and saved requests (browser storage, no
    credentials);
  - copy as `curl`;
  - browser round-trip time, clearly separated from server-side timing (only
    aggregate histograms exist in `/metrics`).
- **Performance:** parses `GET /metrics` when enabled.
  - Request and error rates handle counter resets; latency quantiles are
    estimated from histogram bucket deltas over the observed window.
  - Saturation shows outstanding jobs against capacity, plus lock-wait and
    queue time.
  - No history beyond the session; the metrics-disabled state is explicit.
- **Synchronization:** status, source position versus local revision (not
  comparable, so never subtracted), readiness reasons, and lease remaining
  plus last-applied age from metrics when available.
- **Overview:** condensed, with attention items; technical details collapsed.

The gateway allowlist grows by the inspection routes, `POST /v1/batch` and
`GET /metrics` (read-only, no graph data). Feeder routes stay unreachable.

## Cost check

Protocol and pass criteria are written before measuring, in
[docs/console-inspection-costs.md](../../console-inspection-costs.md).

## Re-qualification impact

With inspection disabled (the default), the server keeps no extra state. The
only change on the node-registration path is a check of an empty optional
field in the `compact-bfs` engine. No published evidence is invalidated by
design; the standard qualification of the next release confirms it.

With inspection enabled, the costs are those measured in the
[cost check](../../console-inspection-costs.md).
