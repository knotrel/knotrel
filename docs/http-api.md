# HTTP API v1

This document is the current HTTP API contract (v0.5.0; first published as the
v0.1.0 preview). Every statement here is backed by
an automated test (`crates/knotrel-server/tests/`) or is explicitly labeled a
non-guarantee. A machine-readable standard definition is available in the
[OpenAPI specification](openapi.yaml). Default address: `127.0.0.1:8080`.

## State and lifetime

- One server process owns one in-memory undirected simple graph. Routers created
  by `router(Graph)` or `router_with_config(ServerConfig)` own exactly one graph;
  clones of a router share it and independent routers never do.
- The standalone executable starts with an empty graph using the engine chosen
  by `KNOTREL_ENGINE`. `router(Graph)` starts from the supplied graph.
- A successful mutation is visible to every request that acquires the graph
  after it completes. In standalone mode graph state changes only through
  `/v1/operations` and `/v1/batch`. In source-managed mode it changes only
  through the feeder endpoints (`/v1/feeder/import`, `/v1/feeder/events`);
  direct client mutations are rejected (see [Mode rules](#mode-rules)).
- **Restart loses the whole graph.** Nothing is written to disk and nothing is
  recovered. A clean shutdown (SIGINT/SIGTERM) discards the graph too: the next
  process always starts empty.

## Non-guarantees

The server does not provide any of the following:

- persistence, snapshots, write-ahead logging or recovery after a restart or crash;
- high availability, replication or failover;
- rollback of earlier operations in a batch, or any transaction spanning
  several requests;
- request deduplication or exactly-once delivery;
- authentication, authorization, TLS or protection against untrusted clients.
  Exposing the service beyond a trusted local boundary is unsupported (see
  [Trust model](#trust-model));
- cancellation of graph work: a request that times out, or whose client
  disconnects, can still have its mutation applied (see
  [Lifecycle, health and shutdown](#lifecycle-health-and-shutdown));
- a graph-operation deadline, write timeouts or slow-reader protection;
- capacity signalling from `/health` or `/ready`.

## Resource bounds

| Resource | Bound |
| --- | --- |
| Request body | 1 MiB, fixed; larger bodies get 413. `POST /v1/feeder/import` is bounded instead by `KNOTREL_MAX_IMPORT_BODY_BYTES` (default 64 MiB): a declared `Content-Length` above it gets 413 before any body byte is read, and a body (chunked included) that grows past it while received gets 413 too. Only the limit answers 413: a body that cannot be read to its end for another reason (client abort, malformed chunked encoding, receive error) gets 400 `invalid_request` (`request body could not be read`), on every route |
| Batch length | 1–1024 operations, fixed |
| Queued plus running graph jobs | `KNOTREL_MAX_PENDING_JOBS`, default 32; excess gets 503 `busy`. In source-managed mode, plus `KNOTREL_FEEDER_RESERVED_JOBS` (default 4) for feeder events, barriers and gaps and 2 for feeder sessions and heartbeats; excess on either feeder lane also gets 503 `busy`. `503 import_in_progress` means only that another import holds the single import permit |
| Graph nodes / edges | `KNOTREL_MAX_NODES` / `KNOTREL_MAX_EDGES`; default 100 000 nodes and 500 000 edges, `unlimited` is an explicit opt-out |
| Header read and keep-alive idle time | `KNOTREL_HEADER_READ_TIMEOUT_MS`, default 5000 ms |
| Request time (body receipt plus handling) | `KNOTREL_REQUEST_TIMEOUT_MS`, default 30000 ms |
| Concurrent connections | `KNOTREL_MAX_CONNECTIONS`, default 1024; excess gets 503 `too_many_connections` |
| Feeder source positions | event `source_position` and `prev_source_position`, import `source_position`, `barrier_source_position` (when present): 1 to 256 **bytes** of UTF-8; otherwise 422 `invalid_request`, after the earlier checks of the request (see `applied_source_position` under "Graph identity") |
| Feeder `event_id` | at most 64 **bytes** of UTF-8; otherwise 422 `invalid_request` |
| Shutdown drain | `KNOTREL_SHUTDOWN_TIMEOUT_MS`, default 10000 ms |
| Write timeouts, per-request memory and CPU | not bounded beyond the limits above |

The body and batch limits are enforced before the request is parsed or
admitted to the graph worker. Graph limits are cardinalities, not memory, CPU or
RSS ceilings. Connection and time bounds limit idle and stalled clients, but a
client can still consume CPU within them and a slow reader is not detected. This
is why untrusted exposure is unsupported.
Embedders that call `router(Graph)` keep the supplied graph's own (unlimited)
limits; only `router_with_config` and the standalone executable apply the
defaults above.

## Trust model

Knotrel has **no authentication, authorization or TLS**. Anyone who can
open a TCP connection to the listener can read and mutate the graph and, in
source-managed mode, take over the feeder session. Every client that can reach
the listener is therefore trusted. The supported exposures for 1.0 are
([threat model §1](threat-model.md#1-scope-and-supported-exposure)):

- **Loopback** (the default): every process and user on the host that can
  open a loopback connection is trusted.
- **Trusted network**: a non-loopback bind with
  `KNOTREL_ALLOW_NON_LOOPBACK=true`, reachable only from a network whose every
  host is trusted with full read/write access.
- **Behind a gateway**: Knotrel reachable only by the gateway (on loopback in
  the gateway's network namespace, or on a network only the gateway reaches);
  the gateway terminates TLS and authenticates and authorizes every request,
  with feeder routes on a separate credential
  ([deployment guide](deployment.md#gateway-with-tls-and-authentication)).

Exposure to any client that is not trusted, including the public internet or a
shared network, is unsupported. A private network is sufficient only when every
host on it is trusted.

**Web pages are not trusted clients, and Knotrel does not check `Host`.** A
page opened in a browser that can reach the listener (for example a browser on
the same host as a loopback listener) can use DNS rebinding to read and mutate
the graph and take over the feeder session. The console container and the
gateway example check `Host` only for requests that pass through them: using
them does not protect a listener the browser can still reach directly. The
protection is that the listener is **not reachable from any context where
untrusted web content runs**: no browser on the listener's host (loopback) or
network (trusted network), or a listener reachable only through the gateway or
the console, as in the gateway example, where Knotrel is reachable from neither
the host nor other containers. Knotrel 1.0 has no `Host` validation of its own
(an optional allowlist is a 1.x proposal, [threat model](threat-model.md) N1).

- The listener defaults to `127.0.0.1:8080`. A non-loopback `KNOTREL_ADDR`
  (including `0.0.0.0` and `[::]`) fails startup unless
  `KNOTREL_ALLOW_NON_LOOPBACK=true` is also set. When it is set the executable
  logs a warning at startup.
- Opting in is an operator acknowledgement, not a hardening step. Exposure to
  untrusted clients, including the public internet, stays unsupported.
- If clients on other hosts are not all trusted, use the gateway exposure:
  a reverse proxy or gateway that provides TLS, authentication, authorization,
  rate limiting and write timeouts, with the network path restricted so that
  only the gateway reaches Knotrel.
- The [threat model](threat-model.md) lists the threats per trust boundary
  and the test that validates each mitigation.
- Logs contain the method, path, status and latency of each request and the
  startup settings. They never contain request bodies, node IDs or parser
  messages.
- Client error messages are **not** free of client data; what they exclude
  is precise. They never quote a string taken from the current request:
  identifiers (instance, session, source, graph and event IDs), source
  positions, epochs, `If-Match` values, or operation, field and query
  parameter names. They never contain parser positions or internal paths.
  They may contain numbers from the request (node IDs, delivery sequences,
  generations, byte lengths) and the server's own state, which includes
  values a feeder supplied earlier (for example the applied source position,
  the server's `source_id` or `source_epoch`, or a retained `event_id`).

## Request validation order

For `POST /v1/operations` and `POST /v1/batch`, the first failing step decides
the response, and nothing is executed unless every step passes:

| Step | Check | Failure |
| --- | --- | --- |
| 1 | Route and method | 404 / 405 (Axum default, empty body) |
| 2 | `Content-Type` is a JSON media type | 415 `invalid_request` |
| 3 | Body is at most 1 MiB | 413 `invalid_request` |
| 3a | Body is read to its end (no client abort, malformed chunked encoding or receive error) | 400 `invalid_request` |
| 4 | Body is syntactically valid JSON | 400 `invalid_request` |
| 5 | Shape: known `op`, required fields, no unknown or duplicate fields, valid node IDs (the whole batch). The five [lifecycle operations](#node-and-edge-lifecycle-opt-in) always parse, whatever the setting; `active` or `enabled` on a legacy operation is an unknown field | 422 `invalid_request` |
| 5a | Lifecycle support off (`KNOTREL_TOPOLOGY_LIFECYCLE=false`): no operation is `set_node_active`, `set_edge_enabled`, `remove_node`, `node_state` or `edge_state` | 409 `topology_lifecycle_disabled` |
| 6 | Source-managed mode only: no operation is a mutation (`add_node`, `link`, `cut`, `set_node_active`, `set_edge_enabled`, `remove_node`) | 409 `direct_mutation_disabled` |
| 7 | Batch length is 1–1024 | 422 `invalid_batch_size` |
| 8 | A graph job slot is available | 503 `busy` / `unavailable` |
| 9 | Operations run in order under the graph lock | per-operation outcome |

Steps 2–7 never touch the graph, so a rejected request changes nothing. Step 8
happens after parsing; `busy` means no operation was admitted. Because the mode
check precedes the size check, a source-managed batch that contains a mutation
returns 409 even when it exceeds 1024 operations, while an empty or oversized
query-only batch returns 422 `invalid_batch_size`. Step 5a rejects the whole
request, including any legacy operations in the same batch; a malformed
operation anywhere in the batch still returns 422 first.

## Single operations

Send JSON to `POST /v1/operations` with `Content-Type: application/json`.
Both operation endpoints also accept valid `application/*+json` media types and
parameters such as `charset=utf-8`. Media type names are case-insensitive.
Missing, malformed or unsupported types (including `application/jsonp` and
`text/*+json`) return 415 before graph execution. A `+json` string in a parameter
does not make another media type JSON.

| Operation | Request | Success response |
| --- | --- | --- |
| Add isolated vertex | `{"op":"add_node","node":"1"}` | `{"result":"changed","changed":true}` |
| Insert undirected edge | `{"op":"link","source":"1","target":"2"}` | `{"result":"changed","changed":true}` |
| Remove undirected edge | `{"op":"cut","source":"1","target":"2"}` | `{"result":"changed","changed":true}` |
| Query connectivity | `{"op":"connected","source":"1","target":"2"}` | `{"result":"connected","connected":true}` |
| Set node flag (lifecycle) | `{"op":"set_node_active","node":"2","active":false}` | `{"result":"changed","changed":true}` |
| Set edge flag (lifecycle) | `{"op":"set_edge_enabled","source":"1","target":"2","enabled":false}` | `{"result":"changed","changed":true}` |
| Remove node and incident edges (lifecycle) | `{"op":"remove_node","node":"2"}` | `{"result":"changed","changed":true}` |
| Query node state (lifecycle) | `{"op":"node_state","node":"2"}` | `{"result":"node_state","registered":true,"active":false}` |
| Query edge state (lifecycle) | `{"op":"edge_state","source":"1","target":"2"}` | `{"result":"edge_state","registered":true,"enabled":false,"effective":false}` |

The five lifecycle operations require `KNOTREL_TOPOLOGY_LIFECYCLE=true`; see
[Node and edge lifecycle](#node-and-edge-lifecycle-opt-in).

Node IDs are strings of ASCII decimal digits representing `0` through
`18446744073709551615`. Leading zeroes are accepted and normalized numerically.
JSON numbers, signs, spaces and overflow are rejected. Unknown fields and
operations are rejected to make client typos visible.

`link` creates absent endpoints. `changed` is false for repeated inserts or cuts
of missing edges, including unknown cut endpoints. `cut` never removes vertices.
Self-loops are invalid for both link and cut and cannot create vertices. An
existing vertex connects to itself; unknown query endpoints are errors.

Successful operations return 200. A graph error has this shape:

```json
{"result":"error","code":"unknown_node","message":"unknown node: 3"}
```

| Status | Meaning / error code |
| --- | --- |
| 400 | Malformed JSON, or a body that could not be read before its end (`invalid_request`) |
| 404 | Unknown query vertex, or unknown node of `set_node_active` / `set_edge_enabled` (`unknown_node`); unregistered edge of `set_edge_enabled` (`unknown_edge`) |
| 409 | Mutation sent in source-managed mode (`direct_mutation_disabled`); lifecycle operation while lifecycle support is off (`topology_lifecycle_disabled`) |
| 413 | Body exceeds 1 MiB (`invalid_request`) |
| 415 | Missing/unsupported JSON content type (`invalid_request`) |
| 422 | Invalid fields/IDs (`invalid_request`), self-loop (`self_loop`, also for `set_edge_enabled` and `edge_state`), batch size (`invalid_batch_size`), or graph capacity (`node_limit_exceeded`, `edge_limit_exceeded`) |
| 503 | Graph job capacity exhausted (`busy`), worker unavailable (`unavailable`), draining (`shutting_down`) or request timed out (`request_timeout`); in source-managed mode also `uninitialized` (no baseline yet), `loading` (baseline import running) or `desynchronized` (rebuild required), for queries |

**Version headers.** Every response of an executed operation carries the
version headers (`ETag`, `X-Knotrel-Instance-Id`, `-Graph-Id`, `-Generation`,
`-State-Version`, `-Source-Position`, `-Sync-Status`), captured under the same
lock acquisition: 200, the domain errors (404, 422 `self_loop`,
`node_limit_exceeded`, `edge_limit_exceeded`) and the source-managed 503
`uninitialized`, `loading` and `desynchronized`. Refusals before execution
(400, 409, 413, 415, 422 shape errors and 503 `busy`, `unavailable`,
`shutting_down`, `request_timeout`) carry none.

Clients should branch on `code`, not human-readable message text. The message of
a 400 response is fixed (`request body is not valid JSON`) and never contains
parser internals, byte offsets or echoed input. A 422 shape error carries a
short description such as "missing field `op`" or "request body does not match
the expected shape". Unregistered paths use Axum's default 404 and wrong methods
a 405 with an `Allow` header; both have an empty body.

## Batches

`POST /v1/batch` accepts 1–1024 operations:

```json
{"operations":[
  {"op":"link","source":"1","target":"2"},
  {"op":"connected","source":"1","target":"2"},
  {"op":"cut","source":"1","target":"2"}
]}
```

```json
{"results":[
  {"result":"changed","changed":true},
  {"result":"connected","connected":true},
  {"result":"changed","changed":true}
]}
```

Results correspond one-to-one with operations. A batch returns 200 even when an
individual operation returns an error result. Processing continues after graph
errors. Earlier successes remain applied: **batches do not provide rollback**.
An invalid envelope, malformed operation or invalid batch size rejects the whole
request before any graph operation executes.

In source-managed mode, while the graph is uninitialized, loading or
desynchronized, an all-query batch is answered 200 with one error result per
operation (`uninitialized`, `loading` or `desynchronized`, the codes that
`/v1/operations` answers with 503) and the version headers; nothing executed.

One lock acquisition spans the entire batch, so other graph requests cannot
interleave with it: a batch with a mutation holds the graph lock exclusively,
and a query-only batch holds it shared, which excludes every mutation. Separate requests execute in lock-acquisition order, not necessarily arrival
order. A connectivity result reflects all earlier operations in that order.

Batch response headers (`ETag`, `X-Knotrel-State-Version`, `X-Knotrel-Generation`,
`X-Knotrel-Instance-Id`, etc.) are captured under the same lock acquisition and
reflect the graph's **final** coherent state after all operations in the batch
have completed. Earlier operations in the batch are not labeled individually with
intermediate revisions: continue-on-error applies each mutation in order, and only
effective mutations (`changed: true`) advance the revision.

### Revision progression arithmetic

Clients observing final revision $V_{\text{final}}$ in `X-Knotrel-State-Version`
can compute the exact revision progression directly from the `results` array:
- Total revision advance: $\Delta V = \sum_{i=0}^{N-1} [\text{results}[i].\text{result} == \text{"changed"} \land \text{results}[i].\text{changed} == \text{true}]$
- Pre-batch baseline revision: $V_{\text{start}} = V_{\text{final}} - \Delta V$
- Intermediate revision after operation $k$: $V_k = V_{\text{start}} + \sum_{i=0}^{k} [\text{results}[i].\text{result} == \text{"changed"} \land \text{results}[i].\text{changed} == \text{true}]$

Operations resulting in `changed: false` (idempotent duplicates or missing cuts),
connectivity queries (`connected`), and domain errors never advance the revision.
Lifecycle commands follow the same rule: one `changed: true` is one increment,
whatever the number of incident edges a `set_node_active` or `remove_node`
affects; `node_state` and `edge_state` never advance it.

### Mode rules

In standalone mode (`KNOTREL_MODE=standalone`), `/v1/batch` accepts arbitrary
combinations of mutations and queries. In source-managed mode
(`KNOTREL_MODE=source_managed`), direct client mutations are rejected with HTTP 409
`direct_mutation_disabled`; `/v1/batch` is permitted only if every operation is a
query (`connected`, and with lifecycle support `node_state` and `edge_state`,
which follow the same readiness rules as `connected`). This check runs before
the batch size check (see [Request validation order](#request-validation-order)).

## Node and edge lifecycle (opt-in)

Lifecycle support is available only with `KNOTREL_ENGINE=compact-bfs`; with
`ett` or `hdt` the server refuses to start (their lifecycle support is
deferred), and no engine is substituted.

With `KNOTREL_TOPOLOGY_LIFECYCLE=true` the server keeps **registered
topology** with one flag per node (`active`) and per edge (`enabled`), and
answers connectivity over the **effective graph**: the active nodes and the
edges that are enabled and whose two endpoints are both active. Flags describe
intended state supplied by the client or source, not measured availability.
The frozen contract is [topology-lifecycle.md](topology-lifecycle.md). With the
default `false`, the server holds the plain graph as before, every response
keeps its previous shape, and the five operations below are rejected with 409
`topology_lifecycle_disabled` (validation step 5a).

| Operation | Effect | `changed: false` when | Errors, in precedence order |
| --- | --- | --- | --- |
| `set_node_active` | Sets the node flag. Incident edges and their `enabled` flags are kept. | Flag already has that value | `unknown_node` (404) |
| `set_edge_enabled` | Sets the edge flag, also while an endpoint is inactive. | Flag already has that value | `self_loop` (422); `unknown_node` (404, `source` before `target`); `unknown_edge` (404) |
| `remove_node` | Removes the node and every incident registered edge, including disabled edges and edges to inactive neighbors. | Node not registered | none |
| `node_state` | Query: `registered`, `active`. | — | none |
| `edge_state` | Query: `registered`, `enabled`, `effective`. | — | `self_loop` (422) when `source == target` |

All fields shown in the request examples above are required and no others are
allowed. An absent node or edge in `node_state` or `edge_state` is not an
error: it reports `registered: false` with `false` flags. Errors use the usual
error envelope; in a batch they are per-item results and processing
continues.

Legacy operations keep their behavior on registered topology: `add_node` of a
registered node is a no-op and does not reactivate it; `link` of a registered
edge is a no-op and does not re-enable it; `link` registers missing endpoints
active and the new edge enabled; `cut` removes the registered edge, effective
or not. Reactivating a node restores exactly its incident edges that are
enabled and whose other endpoint is active. After `remove_node`, a new
`add_node` or `link` of the same ID creates a fresh active node with no edges;
removed edges never return implicitly.

**`connected`.** Both endpoints are first checked for registration (`source`,
then `target`): an unregistered endpoint is 404 `unknown_node`, before any
flag check. Only then is the answer `true` iff a path exists in the effective
graph. An inactive endpoint gives `false`, including `connected(n, n)`; an
active registered `n` gives `connected(n, n) = true`.

**Counts and limits.** `node_count` and `edge_count` are registered counts,
whatever the flags. `GET /v1/stats` and the import response add
`active_node_count` (registered nodes whose flag is active),
`enabled_edge_count` (registered edges whose flag is enabled, effective or
not) and `effective_edge_count` (edges in the effective graph). These three
fields are present only with lifecycle support. `max_nodes`, `max_edges` and
`remaining_*` use registered counts: deactivating or disabling never frees
capacity; only `remove_node` and `cut` do.

**Revisions.** Each lifecycle command is one indivisible step under the graph
lock; no query observes a partly deactivated or partly removed node. A command
returning `changed: true` advances `state_version` by exactly one, also when
connectivity does not change (for example disabling an edge whose endpoint is
inactive). No-ops, queries and errors do not advance it. `generation` changes
only on import publication.

**Cost.** Deactivating or removing a node costs work proportional to its
registered degree, and a removal can trigger a whole-graph engine rebuild
while holding the graph lock (amortized; see
[topology-lifecycle.md, section 5](topology-lifecycle.md#5-engines-complexity-and-storage)).
None of these operations is logarithmic in general.

### Import with lifecycle flags

`POST /v1/feeder/import` accepts flags only with lifecycle support:

```json
{
  "nodes": ["201", {"node": "202", "active": false}, "900"],
  "edges": [
    {"source": "201", "target": "202"},
    {"source": "202", "target": "203", "enabled": false}
  ]
}
```

(other import fields omitted). A `nodes` entry is a node ID string (active) or
an object `{"node", "active"}` with both members required. An `edges` entry
accepts an optional `enabled` (default `true`). Edge endpoints missing from
`nodes` are registered active. The import must carry the complete registered
topology: every node and edge with its flags, including isolated nodes,
inactive nodes and disabled edges. Repeated entries must agree; a node or edge
repeated with a different flag is 422 `invalid_node` or `invalid_edge` (edges
compare as unordered pairs). Legacy payloads are unchanged and mean all
active, all enabled.

With lifecycle support off, any node object or any `enabled` member rejects
the import with 409 `topology_lifecycle_disabled`, even when the value is the
default. The check runs in the payload phase, after JSON parsing and the
`instance_id` and `graph_id` checks, before the candidate is built and before
the graph lock is requested; `knotrel_imports_total` classifies it as
`payload_rejected`.

## Concurrency and order

Every graph request, including queries and `GET /v1/stats`, takes the same
graph lock (since v0.7.0: a read-write lock). A request whose operations
are all queries (`connected`, `node_state`, `edge_state`) and `GET /v1/stats`
take it shared and can run in parallel with each other; any request with a
mutation, and every feeder and import request, takes it exclusively. The
outcome still equals some sequential order of whole requests (a batch is one
indivisible unit in that order): a query request sees no part of a concurrent
mutation, and parallel query requests do not change state. While a mutation
waits for the lock, new query requests wait behind it, so queries cannot
starve the feeder. The order is the order of lock acquisition and is not
otherwise specified, so clients must not infer ordering between requests that
overlap in time. A request that is admitted but whose client disconnects can
still complete.

## Admission and cancellation

Each request body is limited to 1 MiB. By default, at most 32 graph jobs can be
queued or running; `KNOTREL_MAX_PENDING_JOBS` sets this limit at startup.

In source-managed mode, feeder requests are admitted on their own lanes, so
query load can never refuse them:

| Lane | Requests | Permits |
| --- | --- | --- |
| Shared | queries, stats, inspection (and every graph job in standalone mode) | `KNOTREL_MAX_PENDING_JOBS` (32) |
| Feeder data | `/v1/feeder/events`, `/v1/feeder/barrier`, `/v1/feeder/gap` | `KNOTREL_FEEDER_RESERVED_JOBS` (4) |
| Feeder control | `/v1/feeder/session`, `/v1/feeder/heartbeat` | 2 |
| Import | `/v1/feeder/import` | 1 |

A full lane refuses only its own requests, always with `503 busy`: a full
shared lane answers `busy` to queries while heartbeats and events are still
admitted, a full feeder data lane answers `busy` to events, barriers and gaps,
and a full feeder control lane answers `busy` to sessions and heartbeats. Only
the import lane differs: a second import while one holds the permit gets `503
import_in_progress`, before its body is read. (Before v1.0.0 the two feeder
lanes also answered `import_in_progress`; see
[compatibility](compatibility.md#3-what-compatible-means).) The total
bound on queued plus running graph jobs is the sum of the lanes. Lanes only
gate admission; admitted jobs share the same workers and graph lock. Design
and measurement: [feeder admission](integrations/feeder-admission-design.md).
Admission occurs after JSON parsing; this limit bounds graph work, not buffered
HTTP bodies (those are bounded by the connection limit times 1 MiB). Workers
run outside Tokio's async executor. `busy` means no graph operation was admitted.

Once admitted, a job may complete after a client disconnects. A lost response or
`unavailable` error leaves the mutation outcome uncertain. Mutations can be
retried idempotently, although `changed` may differ on retry. A retry of a batch
containing queries can observe newer state. There is no request deduplication or
exactly-once delivery guarantee.

## Lifecycle, health and shutdown

### Probes

| Route | Status | Body | Meaning |
| --- | --- | --- | --- |
| `GET /health` | 200 | `{"status":"ok"}` | Process liveness. Never takes the graph lock; stays 200 while draining. |
| `GET /ready` | 200 | `{"status":"ready"}` | Initialized and accepting graph work. In source-managed mode, requires verified synchronization barrier and active feeder lease. |
| `GET /ready` | 503 | `{"status":"starting"}` | Router built, listener not yet serving. |
| `GET /ready` | 503 | `{"status":"draining"}` | Shutdown has begun. |
| `GET /ready` | 503 | `{"status":"unavailable"}` | The graph worker failed (panic or lost task). Work is rejected with `unavailable`; restart the process. |
| `GET /ready` | 503 | `{"status":"uninitialized"}` | Source-managed mode: server started, awaiting initial baseline import. |
| `GET /ready` | 503 | `{"status":"loading"}` | Source-managed mode: initial baseline candidate is being parsed/constructed off-lock. |
| `GET /ready` | 503 | `{"status":"catching_up"}` | Source-managed mode: baseline or replacement image published; streaming updates toward barrier. |
| `GET /ready` | 503 | `{"status":"stale"}` | Source-managed mode: feeder lease expired against host monotonic clock without heartbeat. |
| `GET /ready` | 503 | `{"status":"desynchronized"}` | Source-managed mode: unrepairable source gap, conflicting duplicate, predecessor mismatch, or source epoch reset. |

#### Readiness evaluation precedence

Readiness enforces a strict evaluation precedence (seven not-ready levels, then ready):
1. **Permanent worker failure:** `503 unavailable` (worker panic or aborted task).
2. **Draining:** `503 draining` (SIGINT/SIGTERM received, shutdown in progress).
3. **Starting:** `503 starting` (router constructed, listener not yet serving).
4. **Uninitialized / Loading:** `503 uninitialized` or `503 loading` (no published graph image).
5. **Desynchronized:** `503 desynchronized` (irrecoverable stream divergence; requires full rebuild).
6. **Expired lease / Stale:** `503 stale` (feeder lease expired; evaluated lock-free against monotonic clock).
7. **Catching up:** `503 catching_up` (graph image published; awaiting verification barrier $B$).
8. **Ready:** `200 ready` (feeder lease active, barrier verified, no gaps or worker failures).

`/ready` carries `Cache-Control: no-store`, does not take the graph lock or an admission permit,
and is not a capacity check (a full job queue still reports ready; clients see `busy`). In source-managed
mode, `/ready` inspects the monotonic lease deadline lock-free; if the deadline has elapsed, it lazily
records `Stale` and reports 503 without blocking on slow graph workers. In the standalone executable
`starting` lasts only until the listener is serving, so external probes normally see connection refused
rather than `starting` before readiness. Embedded routers created with `router(...)` or
`router_with_config(...)` in standalone mode are initially ready; however, they can transition to 503
`unavailable` if a graph worker fails or panics. `router_with_lifecycle(config, lifecycle)` exposes all
lifecycle phases, and `serve(...)` drives them: `starting` before `serve` marks the lifecycle ready,
then the states above, and `503 draining` from the moment the drain begins, whatever the
synchronization status, while `GET`/`HEAD` on `/health` and `/ready` (and on `/metrics` when
enabled) stay answerable and every other request, including another method on those paths, gets
`503 shutting_down` with `Connection: close`. This behavior is stable
([stability.md](stability.md)).

### Metrics (opt-in)

`GET /metrics` exists only when `KNOTREL_METRICS_ENABLED=true` (embedded:
`ServerConfig::with_metrics_enabled(true)`); otherwise the route is not
registered and returns 404 like any unknown path. It is served on the same
listener and has the same trust model as every other route: **no
authentication**. It returns 200 with
`Content-Type: text/plain; version=0.0.4; charset=utf-8` and
`Cache-Control: no-store`.

Like the probes, a scrape takes no graph lock and no admission or import
permit; it does use synchronization internal to the metrics registry and
reads each admission lane's available permits (one atomic load per lane). It is answered during saturation, after a worker failure and while
draining (with `Connection: close`). It exposes HTTP request counts and
server-side durations by route template, held admission permits and the
permit limit per admission lane (`lane` label: `shared`, and in
source-managed mode `feeder_data`, `feeder_control` and `import`), the
unavailable flag, the state `/ready` would report, and build
information. Names, labels, exact semantics and the series bound are
specified in the [observability contract](observability-contract.md#7-first-m62-slice-released-in-v040).
An HTTP `503 request_timeout` or a cancelled request in these metrics never
means the graph job finished or did not run; see
[admission and cancellation](#admission-and-cancellation).

Two further counters are recorded where the decision is made, not derived
from response codes ([contract section 8](observability-contract.md#8-second-m62-increment-rejection-and-uncertain-outcomes-released-in-v040)):

- `knotrel_requests_rejected_total{reason,lane}`: requests refused before any
  graph job or import ran for them: `busy` (admission permits of the
  request's lane exhausted, all answered `503 busy`, including feeder session,
  heartbeat, events, gap and barrier calls; `lane` names the lane),
  `import_capacity` (`503 import_in_progress`, `lane="import"`), `shutting_down` (every
  non-probe request refused while draining, including `/v1/info` and
  unmatched routes that would not create a graph job) or `unavailable`
  (both `lane="none"`). Validation, domain, feeder-protocol and
  partial batch or event failures are not counted.
- `knotrel_requests_outcome_uncertain_total{cause}`: observations that
  an admitted state-changing job may have taken effect without its caller
  learning the result: `request_timeout`, `cancelled` or `worker_panic`.
  The unit is an observation, not a request. A request that times out and
  whose worker later panics adds two. Read-only requests (`/v1/stats`,
  `connected`-only lists) are not counted.

`knotrel_graph_nodes` and `knotrel_graph_edges` report the live graph's
size without taking the graph lock or an admission permit, unlike
`/v1/stats` ([contract section 10](observability-contract.md#10-third-m62-increment-graph-node-and-edge-gauges-released-in-v040)).
With lifecycle support they are registered counts, and
`knotrel_graph_active_nodes`, `knotrel_graph_enabled_edges` and
`knotrel_graph_effective_edges` are published with them at the same
boundaries; these three gauges exist only with lifecycle support
([contract section 19](observability-contract.md#19-m10-node-and-edge-lifecycle-metrics)).
The worker publishes them under the graph lock when a state-changing job's
graph work returns or an import is published, so they keep the previous
values while a batch, event request or import is in flight. They always hold
the last published values: a worker panic before publication leaves the
previous counts, and a panic after it does not roll them back, so after a
worker failure they do not guarantee the service's current graph state.
They are two independent samples, not a coherent pair.

`knotrel_graph_job_queue_seconds`, `knotrel_graph_lock_wait_seconds` and
`knotrel_graph_job_execution_seconds` split worker time by `kind`
(`operation`, `batch`, `stats`, `feeder_session`, `feeder_heartbeat`,
`feeder_events`, `feeder_gap`, `feeder_barrier`, `import_publish`; only the
first three in standalone mode)
([contract section 11](observability-contract.md#11-fourth-m62-increment-graph-job-queue-lock-wait-and-execution-histograms-released-in-v040)).
They cover:
- **queue:** submission to the blocking pool until the worker starts;
- **lock wait:** the worker's wait for the graph lock;
- **execution:** the time it holds the lock.

Workers record them even after the caller timed out or was cancelled.
Requests rejected before admission produce no timing observations. An
admitted worker records each phase it reaches, including when it later
rejects an import or other work.
They exclude HTTP parsing and serialization and, for imports, the body read,
parsing and candidate construction, so they do not add up to HTTP latency.

`knotrel_graph_operations_total{op,outcome}` counts each operation from
`/v1/operations` and each `/v1/batch` entry once, when the worker has
applied it. `op` is `add_node`, `link`, `cut` or `connected`, and with
lifecycle support also `set_node_active`, `set_edge_enabled`, `remove_node`,
`node_state` and `edge_state`. `outcome` is `changed`, `unchanged` or `error`
for mutations, `true`, `false` or `error` for `connected`, and `registered`,
`unregistered` or `error` for `node_state` and `edge_state`. Series exist only
for operations the mode can execute (no mutations in source-managed mode). A
batch counts every entry, including entries after a domain error.
`knotrel_feeder_events_total{result}` (source-managed mode only) counts each
event the event loop evaluates:
- `changed`: applied and changed the graph;
- `unchanged`: an applied no-op mutation or `progress` event;
- `duplicate`: an exact retained duplicate;
- `rejected`: the event at which the request stopped.

Events after a rejected event are not counted.

Both counters count nothing for:
- requests refused before evaluation, such as a malformed or empty body,
  admission rejection, `direct_mutation_disabled`, the source-managed
  `uninitialized`, `loading` and `desynchronized` gate for operations, and
  event-request checks such as `stale_generation` that run before any event;
- gap, barrier and import calls.

Feeder events never count as operations. Workers count outcomes even after
the caller timed out or was cancelled. A worker panic keeps the outcomes
already counted and adds none for the interrupted item
([contract section 12](observability-contract.md#12-fifth-m62-increment-graph-operation-and-feeder-event-outcome-counters-released-in-v040)).

`knotrel_feeder_rejections_total{code}` (source-managed mode only) counts
each session, heartbeat, events, gap or barrier request the service rejects,
once, under the underlying error code. A partial event failure counts its
underlying code, not `partial_failure`, and any graph-domain event error
counts as `event_failed`. A lifecycle event refused because lifecycle
support is off counts as `topology_lifecycle_disabled`. The counter unit is a
request, unlike
`knotrel_feeder_events_total{result="rejected"}`, which counts events. It
does not count admission refusals (including the `503 busy` returned when a
feeder lane is exhausted), timeouts, cancellations,
panics, refusals by the HTTP layer before the service is called (mode,
instance ID, graph ID, malformed JSON) or imports.

`knotrel_desynchronizations_total{cause}` (source-managed mode only) counts
each transition into `desynchronized`: `gap_declared`,
`source_epoch_mismatch`, `source_predecessor_mismatch` or
`conflicting_duplicate`. Requests on an already desynchronized graph,
including a repeated gap, add none. Out-of-order sequences and unverifiable
duplicates are rejected without a transition. Neither counter resets on
graph replacement, and workers count even after the caller timed out or was
cancelled
([contract section 13](observability-contract.md#13-sixth-m62-increment-feeder-rejection-and-desynchronization-counters-released-in-v040)).

`knotrel_feeder_lease_remaining_seconds` (source-managed mode only) is the
current feeder lease deadline minus the server's monotonic clock, computed at
scrape in whole milliseconds. It is positive while the lease is valid, `0`
throughout the deadline millisecond (feeder calls may already fail with
`lease_expired`, while `/ready` still reports the lease as live) and negative
from the next millisecond, when every check, including `/ready`, reports it
expired. Scraping never renews the lease. The series is absent until
the first successful session registration; heartbeats and takeovers move the
deadline, imports do not.

`knotrel_feeder_last_applied_age_seconds` (source-managed mode only) is the
monotonic time since the last applied non-duplicate event (including no-op
mutations and `progress`) or published import. Duplicates, rejected events
and imports, gaps, barriers, sessions, heartbeats and queries do not reset
it; a partial failure keeps the time of its last applied prefix event. It is
absent before the first application. It measures inactivity, **not source
lag**: an idle source legitimately makes it large, and only `/ready` reports
synchronization. Both gauges are sampled independently of each other and of
the other gauges
([contract section 14](observability-contract.md#14-seventh-m62-increment-feeder-lease-remaining-and-last-applied-age-released-in-v040)).

`knotrel_import_in_progress` (source-managed mode only) is `1` while an import
holds the single import permit (body read, worker queue, preparation and
publication, also after its caller has gone) and `0` otherwise.
`knotrel_imports_total{result}` counts each import that obtained the permit
once, by the phase it reached: `published`; `payload_rejected` (failed body
read, malformed or invalid payload, request identity, graph limits);
`publication_rejected` (anything after the graph-lock request: unavailable,
session or lease fencing, import mode, `If-Match`, generation exhaustion); or
`worker_failed` (a panic before publication); or `abandoned` (ownership ended
before the worker started: the caller abandoned the body read, or the worker
closure was dropped without running). An import that publishes after its
caller timed out counts as `published`, never `abandoned`. Imports refused
before the permit, including
`503 import_in_progress`, are not imports.
`knotrel_import_duration_seconds{phase}` times the disjoint `receive` (permit
acquired to body read returned), `prepare` (worker parse and candidate build)
and `publish` (graph-lock request to the end of the worker) phases
([contract section 15](observability-contract.md#15-eighth-m62-increment-import-gauge-results-and-phase-durations-released-in-v040)).
`knotrel_catchup_duration_seconds{kind}` (`initial` for `create_only`,
`replacement` for `replace`) times completed catch-up intervals from import
publication to the first `synchronized` barrier in that generation
([contract section 16](observability-contract.md#16-ninth-m62-increment-catch-up-duration-released-in-v040)).

### Graceful shutdown

On the first SIGINT or SIGTERM (Unix; Ctrl-C elsewhere) the server:

1. switches `/ready` to 503 `draining`;
2. keeps accepting connections only so it can answer them: any request other
   than a `GET` or `HEAD` on `/health`, `/ready` or (when enabled) `/metrics`
   is rejected with 503 and
   `{"result":"error","code":"shutting_down",...}`, and every response carries
   `Connection: close`;
3. closes idle keep-alive connections and connections still reading request
   headers, and lets requests that were already admitted (being received or
   running) finish and answer normally, with `Connection: close`;
4. exits once every connection and every outstanding graph worker job has
   finished, or after `KNOTREL_SHUTDOWN_TIMEOUT_MS`, whichever comes first.
   `serve()` aborts open network connection tasks at the deadline, but cannot
   forcibly terminate running `spawn_blocking` graph workers. In the standalone
   executable, the process exits with status 2 (terminating the process and any
   lingering workers). Embedded callers of `serve()` receive
   `ShutdownOutcome::DeadlineExceeded` and must handle outstanding worker
   tasks explicitly.

A rejection with `shutting_down` means the request was not admitted and no graph
change occurred, so it is safe to retry elsewhere. A second signal forces
immediate exit.

| Exit status | Meaning |
| --- | --- |
| 0 | Drained completely |
| 1 | Invalid configuration or the listener could not be bound |
| 2 | Drain deadline exceeded. The standalone process terminates with status 2; embedded callers receive `DeadlineExceeded` and must account for lingering background workers |
| 130 | Second signal received during the drain |

### Timeouts and connection bound

- **Header read / idle.** `KNOTREL_HEADER_READ_TIMEOUT_MS` bounds the time
  allowed to receive a complete request head, including the wait for the next
  request on a keep-alive connection. A silent, slow-header or idle connection
  is closed. There is no separate idle setting.
- **Request.** `KNOTREL_REQUEST_TIMEOUT_MS` bounds body receipt plus handling.
  When exceeded the client gets 503 `request_timeout` with `Connection: close`.
  Cancelling the handler does not cancel a graph job that was already handed to
  a worker, so **the outcome of a timed-out mutation is unknown**. Reading the
  graph afterwards or retrying does not immediately resolve it if the mutation
  worker is still queued or active: a subsequent read can race with the still-running
  mutation.
- **Connections.** At most `KNOTREL_MAX_CONNECTIONS` are served concurrently. An
  extra connection receives a static 503 `too_many_connections` and is closed;
  this write is best effort and a client may instead see a reset. Capacity is
  reclaimed as connections close.
- **Not provided.** Write timeouts and slow-reader protection, per-request CPU or
  memory limits, and a deadline on an admitted graph traversal. A drain whose
  graph job outlasts the shutdown timeout ends with exit status 2.

## Error codes

Every error body is `{"result":"error","code":...,"message":...}` (a feeder
partial failure adds the frontier fields). Clients branch on `code`, never on
`message`, and treat an unknown code as an error of its status class. Every
code, its status and where it comes from (the same table is in the
`ErrorCode` schema of [openapi.yaml](openapi.yaml)):

| Code | Status | Where |
| --- | --- | --- |
| `barrier_not_reached` | 409 | barrier: `barrier_delivery_sequence` ahead of the applied sequence |
| `barrier_position_mismatch` | 409 | barrier: equal sequence, different `barrier_source_position` (when supplied) |
| `busy` | 503 | the request's admission lane is exhausted: operations, batch, stats, inspection (shared lane); session, heartbeat (feeder control lane); events, barrier, gap (feeder data lane) |
| `conflicting_duplicate` | 409 | events: retained sequence with a different `event_id` or payload, or a retained `event_id` reused; desynchronizes |
| `desynchronized` | 503 | operations in source-managed mode while the graph is desynchronized; in a batch, every item of a 200 |
| `direct_mutation_disabled` | 409 | operations, batch: a mutation in source-managed mode |
| `edge_limit_exceeded` | 422 | operations, batch items, events, import |
| `event_failed` | 422 | events: top-level `code` of a multi-event partial failure caused by a graph-domain error; the domain code is in `error.code` |
| `fenced_feeder_session` | 409 | session (older epoch), heartbeat, import, events, gap, barrier |
| `generation_exhausted` | 409 | import: the generation counter cannot advance |
| `graph_already_initialized` | 409 | import: `create_only` on an initialized graph |
| `graph_desynchronized` | 409 | events, barrier: the graph is desynchronized |
| `graph_uninitialized` | 409 | events, gap, barrier: no baseline imported |
| `import_in_progress` | 503 | import: another import holds the import permit (before the body is read) |
| `inspection_disabled` | 409 | inspection routes, inspection off (experimental) |
| `inspection_unsupported` | 409 | `GET /v1/nodes/{node}/edges`, `GET /v1/edges` on an engine without adjacency (experimental) |
| `invalid_batch_size` | 422 | batch: not 1–1024 operations |
| `invalid_cursor` | 422 | inspection: the cursor names an unregistered node (experimental) |
| `invalid_edge` | 422 | events, import: an edge the graph refuses for a reason other than a self-loop or a limit; with lifecycle support, a conflicting duplicate edge in an import |
| `invalid_graph_id` | 400 | import, events, gap, barrier: `graph_id` other than `default` |
| `invalid_instance_id` | 400 | every feeder route: `X-Knotrel-Instance-Id` missing or not a valid header value |
| `invalid_lease_duration` | 400 | session: `lease_duration_ms` outside 1000–60000 |
| `invalid_node` | 422 | events, import: a node the graph refuses for a reason other than a limit; with lifecycle support, a conflicting duplicate node in an import |
| `invalid_request` | 400, 413, 415, 422 | every JSON route: malformed JSON or a body that could not be read before its end, such as a client abort or malformed chunked encoding (400), body over 1 MiB on routes other than import (413), wrong `Content-Type` (415), shape, ID or bound violations (422); inspection query errors (422) |
| `lease_expired` | 409 | import, events, gap, barrier: the feeder lease expired |
| `loading` | 503 | operations in source-managed mode while a baseline loads; in a batch, every item of a 200 |
| `mode_conflict` | 409 | every feeder route in standalone mode |
| `node_limit_exceeded` | 422 | operations, batch items, events, import |
| `out_of_order_sequence` | 409 | events: delivery sequence gap; carries `expected_sequence` |
| `payload_too_large` | 413 | import: body over `KNOTREL_MAX_IMPORT_BODY_BYTES`, declared by `Content-Length` or received; never another body read failure |
| `precondition_failed` | 412 | import `replace`: missing or mismatching `If-Match`, or no initialized graph |
| `request_timeout` | 503 | any route: `KNOTREL_REQUEST_TIMEOUT_MS` exceeded; a state-changing outcome is unknown |
| `self_loop` | 422 | operations, batch items, events, import |
| `sequence_exhausted` | 409 | events: the delivery sequence cannot advance |
| `session_required` | 409 | heartbeat, import, events, gap, barrier: no registered session |
| `shutting_down` | 503 | every request except `GET`/`HEAD` on `/health`, `/ready` and `/metrics`, while draining (also unknown routes and other methods on those paths) |
| `source_epoch_mismatch` | 409 | events: different `source_epoch`; desynchronizes |
| `source_mismatch` | 409 | events: different `source_id` |
| `source_predecessor_mismatch` | 409 | events: `prev_delivery_sequence` or `prev_source_position` does not match; desynchronizes |
| `stale_generation` | 409 | events, gap, barrier: `generation` is not the current one |
| `stale_instance_id` | 409 | every feeder route: header or body `instance_id` of another process instance |
| `superseded_barrier` | 409 | barrier: `barrier_delivery_sequence` behind the applied sequence |
| `too_many_connections` | 503 | no route: written by the accept loop to a connection beyond `KNOTREL_MAX_CONNECTIONS`, before any request is read |
| `topology_lifecycle_disabled` | 409 | operations, batch, events, import: a lifecycle operation, event or flag without lifecycle support |
| `unavailable` | 503 | every graph route: the graph worker failed (restart required) |
| `uninitialized` | 503 | operations in source-managed mode before a baseline; in a batch, every item of a 200 |
| `unknown_edge` | 404, 422 | operations: `set_edge_enabled` on an unregistered edge (404); the same as a batch item; lifecycle events (422) |
| `unknown_node` | 404, 422 | operations: unknown query vertex or lifecycle node (404); the same as a batch item; inspection `GET /v1/nodes/{node}` and `/edges` (404); lifecycle events (422) |
| `unverifiable_duplicate` | 409 | events: a retry of a sequence older than the deduplication window |

In a batch, per-operation errors appear in the `results` of a 200, without a
status of their own. Besides the codes above, unregistered paths get 404 and
wrong methods 405 with an `Allow` header, both with an empty body (503
`shutting_down` instead while draining), and a `/metrics` encoding failure
gets an empty 500. Request and response schemas of every route, including the
feeder session, heartbeat and import, are in [openapi.yaml](openapi.yaml);
their protocol semantics are in the
[integration contract](integration-contract.md).

## Startup configuration and engine selection

The standalone executable reads a single configuration snapshot before opening
its listener. There is no file loader or runtime reload in this version.

| Variable | Accepted value | Default |
| --- | --- | --- |
| `KNOTREL_ADDR` | IP socket address, such as `127.0.0.1:8080` or `[::1]:8080`; non-loopback requires `KNOTREL_ALLOW_NON_LOOPBACK=true` | `127.0.0.1:8080` |
| `KNOTREL_ALLOW_NON_LOOPBACK` | Exactly `true` or `false` | `false` |
| `KNOTREL_ENGINE` | Exactly `compact-bfs`, `ett` or `hdt` | `compact-bfs` |
| `KNOTREL_MODE` | Exactly `standalone`, `source_managed` or `source-managed`; `source-managed` is an accepted, stable alternative spelling of `source_managed` | `standalone` |
| `KNOTREL_MAX_IMPORT_BODY_BYTES` | ASCII decimal integer from 1 through 536870912 (512 MiB) | `67108864` (64 MiB) |
| `KNOTREL_DEDUP_WINDOW_SIZE` | ASCII decimal integer from 1 through 1000000 | `10000` |
| `KNOTREL_METRICS_ENABLED` | Exactly `true` or `false`; `true` serves [`GET /metrics`](#metrics-opt-in) | `false` |
| `KNOTREL_TOPOLOGY_LIFECYCLE` | Exactly `true` or `false`; `true` enables [node and edge lifecycle](#node-and-edge-lifecycle-opt-in) support and requires `KNOTREL_ENGINE=compact-bfs` (with `ett` or `hdt` startup fails before binding; their lifecycle support is deferred) | `false` |
| `KNOTREL_INSPECTION` | Exactly `true` or `false`; `true` enables the read-only [inspection routes](#inspection-opt-in) and keeps an ordered node index | `false` |
| `KNOTREL_MAX_PENDING_JOBS` | ASCII decimal integer from 1 through Tokio's `Semaphore::MAX_PERMITS` | `32` |
| `KNOTREL_FEEDER_RESERVED_JOBS` | ASCII decimal integer from 1 through 64; admission permits for feeder events, barriers and gaps, used in source-managed mode only | `4` |
| `KNOTREL_MAX_NODES` | Nonnegative ASCII decimal integer fitting `usize`, or exactly `unlimited` | `100000` |
| `KNOTREL_MAX_EDGES` | Nonnegative ASCII decimal integer fitting `usize`, or exactly `unlimited` | `500000` |
| `KNOTREL_MAX_CONNECTIONS` | ASCII decimal integer from 1 through 1000000 | `1024` |
| `KNOTREL_HEADER_READ_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `5000` |
| `KNOTREL_REQUEST_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `30000` |
| `KNOTREL_SHUTDOWN_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `10000` |

Unknown variables in the reserved `KNOTREL_` namespace, empty or non-Unicode
values, unsupported engines and invalid addresses/limits fail startup. Within
that namespace, the `KNOTREL_CONSOLE_` prefix is reserved for the
[operational console](console.md#container-configuration) container, together
with the console's two deprecated aliases `KNOTREL_UPSTREAM` and
`KNOTREL_UPSTREAM_TIMEOUT`: the server never defines a setting under these
names and ignores them silently, without failing or logging, so one
environment file can configure the server and the console. Every other
unknown `KNOTREL_*` name still fails startup. Values
are not trimmed; engine names are case-sensitive. Unrelated environment
variables are ignored by this loader. `RUST_LOG` is handled separately by the
logging subscriber (falling back to `info` if its filter is missing or invalid),
and `LOG_FORMAT=json` selects one JSON object per log line. Log fields, request
correlation and redaction are described in the
[observability contract, section 17](observability-contract.md#17-m63-structured-logs-released-in-v040).
No configuration file or implicit engine fallback is used.

`compact-bfs` uses the existing compact-adjacency `Graph`. `ett` opts into
experimental `ForestGraph`, the AVL Euler-tour forest with candidate-pruned
replacement search. It is not HDT, and it has no polylogarithmic update guarantee.
`hdt` opts into experimental `HdtGraph`, adding edge levels and promotions with
O(log² N) amortized edge updates over a history reaching N vertices. It uses more
memory, O(E + V log V), and a single cut can still be expensive. All three engines
provide the same exact graph and batch semantics. None caches query answers;
the indexed engines maintain their connectivity structures on updates.

The job limit covers admitted queued plus running graph work, including
`GET /v1/stats`, not concurrent mutation workers. All graph operations still acquire the same graph lock (see [Concurrency and order](#concurrency-and-order)). Body
size (1 MiB) and batch size (1–1024) are fixed in this version.

`GET /v1/info` returns HTTP 200 and immutable effective settings, for example:

```json
{
  "engine": "ett",
  "experimental": true,
  "max_pending_jobs": 16,
  "max_body_bytes": 1048576,
  "max_batch_operations": 1024,
  "max_nodes": null,
  "max_edges": null
}
```

The endpoint does not acquire the graph mutex or an admission permit, and
contains no graph data. It does not report the lifecycle phase; use `/ready`.
In source-managed mode it also contains `"mode": "source_managed"` and the
current `instance_id`; both are absent in standalone mode, where the instance ID
is available from the `X-Knotrel-Instance-Id` header of `/v1/stats`,
`/v1/operations` and `/v1/batch`. No endpoint reports the server software
version.
In source-managed mode it also contains `feeder_reserved_jobs`, the effective
`KNOTREL_FEEDER_RESERVED_JOBS`.
With `KNOTREL_TOPOLOGY_LIFECYCLE=true` it adds `"topology_lifecycle": true`;
the field is absent when lifecycle support is off. Engine and
limit fields describe the actual service instance, including embedded routers.
The default response has `engine: "compact-bfs"`, `experimental: false` and
`max_pending_jobs: 32`.

`experimental` reports the configured engine's qualification
([compatibility.md](compatibility.md#22-stable-interface-experimental-engine)):
`true` means the engine is qualified experimental as to performance, maturity
and usage recommendation, which carry no promise. It is not a stability class:
the engine's interfaces, including its `engine` value, are stable either way.
It is `false` for `compact-bfs` and `true` for `ett` and `hdt`, possibly for
the whole 1.x series. It may change from `true` to `false` in a minor release
when an engine is promoted (recorded in the release notes), and never from
`false` to `true` for an existing engine. Neither change is breaking.

Operation and batch requests cannot configure the engine or the server. An
`engine` field, for example, is rejected as an unknown field before mutation.
There is no administrative engine-switch endpoint. A future migration would
need to preserve every vertex (including isolated ones) and edge, construct the
new state, and replace the old state only on success. Restarting today's
in-memory server instead loses the graph.

Embedded callers use `ServerConfig::new(address, engine, max_pending_jobs)` and
`router_with_config(config)` to create a selected empty graph. The constructor
validates admission limits; it does not read the environment. The router does
not bind the configured address: the caller owns its listener. The original
`router(Graph)` preserves its supplied, possibly populated graph and uses the
default limits. Direct core callers use `ConnectivityGraph::new(EngineConfig)`;
the core never loads server configuration or reads the environment.

## Graph capacity errors

`KNOTREL_MAX_NODES` and `KNOTREL_MAX_EDGES` optionally set per-instance ceilings
at startup. Missing values default to 100 000 nodes and 500 000 edges; the
literal `unlimited` removes a ceiling; zero is allowed. Empty, signed,
non-decimal, whitespace-padded and out-of-range values fail before binding.
`GET /v1/info` includes `max_nodes` and `max_edges`, each an integer or `null`.
These settings cannot be overridden in operation requests or changed at runtime.

An `add_node` or `link` that would exceed capacity returns HTTP 422 with
`result: "error"` and code `node_limit_exceeded` or `edge_limit_exceeded`.
Self-loop validation precedes capacity checks; if both ceilings would be
exceeded, the node error takes precedence. A duplicate remains a successful
no-op even at capacity. Rejection leaves graph state unchanged, including all
new endpoints of a link. Missing-edge cuts keep their existing no-op semantics.

Batches retain HTTP 200 and ordered per-operation outcomes. A capacity error
does not roll back earlier operations or prevent later operations. A later cut
can release edge capacity for a subsequent link within that batch. Cuts never
remove registered vertices. Concurrent requests check and mutate under the same
graph lock, so they cannot jointly exceed the configured ceilings.

These bounds limit cardinalities, not memory, execution time or total process
RSS. They apply independently to each router/graph instance; cloned routers
share one instance. No hierarchy or typed-node model is introduced.

## Live graph snapshot versus instance information

| Contract | `GET /v1/info` | `GET /v1/stats` |
| --- | --- | --- |
| Purpose | Effective immutable configuration | Current coherent graph state |
| Graph lock | Not acquired | Acquired shared on a blocking worker |
| Admission permit | Not required | Shares the graph job limit |
| Saturated graph workers | Still available | HTTP 503 `busy` |
| State changes | No live fields | Reflects completed effective mutations |

`GET /v1/stats` returns HTTP 200 with this shape:

```json
{
  "node_count": 42000,
  "edge_count": 68000,
  "max_nodes": 100000,
  "max_edges": 200000,
  "remaining_nodes": 58000,
  "remaining_edges": 132000,
  "state_version": "157"
}
```

In source-managed mode the object also contains `instance_id`, `generation`
(a JSON integer up to 2^64 − 1, so JavaScript clients must parse it
losslessly), `sync_status` and, once one exists, the raw `source_position`.
The response carries the same version headers as `/v1/operations`.

With lifecycle support the object also contains `active_node_count`,
`enabled_edge_count` and `effective_edge_count`, captured under the same lock;
`node_count` and `edge_count` are then registered counts (see
[Node and edge lifecycle](#node-and-edge-lifecycle-opt-in)).

Counts are integers; each maximum and remaining capacity is an integer or
`null` (unlimited). The two limits are repeated to make this snapshot usable
without a second request. Zero capacity is reported as zero, not null.
Remaining node capacity counts isolated vertices too: cuts do not remove nodes.
The endpoint does not report RAM, allocator capacities or estimated throughput.

All fields are captured while holding the same lock as graph operations. They
represent one logical instant, even if another request changes the graph before
the response reaches the client. A snapshot sees either the state before a batch
or the state after its entire execution, including any per-operation errors;
it cannot see an intermediate batch state. JSON serialization occurs after the
lock is released. Responses carry `Cache-Control: no-store`, including errors.

`state_version` is a decimal string, not a JSON number or timestamp:

- It starts at `"0"` when the router/service instance is created. This also applies
  to the legacy `router(Graph)` with an already populated graph: earlier changes
  are not counted. Cloned routers share state and revision; distinct instances
  have independent revisions.
- Each operation returning `changed: true` increments it once. A link creating
  two endpoints still increments once, and so does a `set_node_active` or
  `remove_node` affecting many incident edges. A batch increments once per effective
  operation, not once per request. Removing and then reinserting an edge advances
  it twice even if the final topology equals the original topology.
- Duplicates, missing-edge cuts, queries, rejected operations, malformed requests
  and stats reads never increment it. An admitted mutation that finishes after
  its caller disconnects still increments it.
- The revision is instance-local and not persisted. It does not identify a
  restart, provide a global ordering across instances, or authorize a conditional
  write. Do not compare it lexicographically or parse it as a JavaScript Number;
  clients can compare for equality or parse it as an arbitrary-precision integer.
  The decimal counter neither wraps nor saturates at a machine-integer boundary.

Stats reads use the same bounded admission and blocking-worker execution as
mutations: they can wait behind a graph operation, or receive HTTP 503 `busy`
when all permits are occupied. A panic while executing a graph job permanently
marks this service instance unavailable: queued and subsequently admitted graph
jobs return HTTP 503 `unavailable`,
using the existing error envelope, even if the original caller disconnected.
A state-changing job sets the marker under the exclusive graph lock; a
read-only job that panics sets it before releasing its shared lock (since
v0.7.0). Read-only requests already running in parallel with a panicking
read-only request complete normally: it could not change the state they read.
The marker does not depend on lock poisoning.
A partial mutation is not rolled back and cannot be read through `/v1/stats`.
Recover by replacing/restarting the service; in-memory data is not persisted.
Disconnecting an admitted stats caller does not release its permit before its worker finishes. `/v1/info`
`/health` and `/ready` remain independent of this queue.

## Inspection (opt-in)

Four read-only routes let a client discover nodes and edges without knowing
their IDs, for example the operational console's data browser. They are
enabled with `KNOTREL_INSPECTION=true`. When it is off (the default) every
route answers 409 `inspection_disabled` before admission (a request whose
query string is invalid gets its 422 `invalid_request` first, because the
query is parsed before the switch is checked), and the server keeps no extra
state. `GET /v1/info` adds `"inspection": true` when it is
on.

| Route | Result |
| --- | --- |
| `GET /v1/nodes?after=&limit=&state=` | Nodes in ascending numeric ID |
| `GET /v1/nodes/{node}` | One node: `active` and registered `degree` |
| `GET /v1/nodes/{node}/edges?after=&limit=&state=` | The node's incident edges with `enabled` and `effective` |
| `GET /v1/edges?after_source=&after_target=&limit=&state=` | Every edge once, as `source < target` |

IDs are JSON strings, exact over the whole u64 range. `limit` is 1–1000
(default 100). Node `state` is `all`, `active` or `inactive`. Edge `state` is
`all`, `enabled`, `disabled`, `effective` or `ineffective`. Without lifecycle
support every node is active and every edge enabled and effective. Unknown or
repeated parameters, malformed IDs and out-of-range values get 422
`invalid_request`; a `{node}` path segment that does not percent-decode to
UTF-8 (for example `%FF`) is a malformed ID too, except that it gets 409
`inspection_disabled` when inspection is off. An unregistered `{node}` gets
404 `unknown_node`.

Example:

```text
GET /v1/nodes?limit=2
200 {"nodes":[{"node":"2","active":true,"degree":2},
              {"node":"7","active":true,"degree":0}],"next_after":"7"}
GET /v1/nodes/2/edges?state=ineffective
200 {"node":{"node":"2","active":true,"degree":2},
     "edges":[{"neighbor":"3","enabled":true,"effective":false}],"next_after":null}
```

**Order and cursors.** Nodes are ordered by ID. Incident edges are ordered by
neighbor in the engine's stable incidence order: ascending neighbor ID with
lifecycle support, neighbor registration order without it. `/v1/edges`
orders by `source`, then by `source`'s incidence order. A non-null
`next_after` (or `next_after_source` and `next_after_target`) is the cursor of
the next page; `null` means the traversal is complete. A cursor is a
position, not a token: it never expires and stays valid after the element it
names is removed. The exception is plain `compact-bfs`, where an incidence
cursor must name a registered node (nodes are never removed there);
otherwise the route answers 422 `invalid_cursor`.

**Consistency.** Each page is computed under one acquisition of the shared
graph lock, and its version headers (`ETag`, `X-Knotrel-*`) describe that
instant. Pages are **not** a snapshot:

- an element present for the whole traversal is returned exactly once;
- elements added or removed meanwhile may or may not appear;
- when `X-Knotrel-Instance-Id` or `X-Knotrel-Generation` changes between
  pages, the traversal spans different graphs, so restart it.

**Bounded work.** A page examines at most 10 000 nodes or incidences (the
scan budget), so a selective filter cannot hold the lock for long. Such a page
can hold fewer than `limit` items while its cursor is non-null; keep
following the cursor. A page costs `O(log V)` to position plus at most the
budget, independent of graph size. Node detail is `O(1)` expected.

**Engines and modes.**

- Node listing and node detail work with every engine. `ett` and `hdt` keep no
  per-node adjacency: they report `degree: null` and answer 409
  `inspection_unsupported` for incident and edge listing.
- The routes use the same bounded admission, blocking worker and shared lock as
  `GET /v1/stats` (job kind `inspect` in the metrics), with no readiness gate.
- They are available in both modes and never mutate. Responses carry
  `Cache-Control: no-store`, except the 422 query errors, which are returned
  before the request reaches the inspection path (these routes are
  experimental; the header may be added to them in a minor release).

**Costs.** Only with inspection enabled, the service keeps an ordered set of
registered node IDs and, for plain `compact-bfs`, an index→ID vector used to
list neighbors (8 bytes per node). Registering or removing a node updates
them in `O(log V)`; links between existing nodes and flag changes do not
touch them. Imports build them off-lock before publication. With inspection
disabled the server keeps neither. Measurements:
[console inspection costs](console-inspection-costs.md).

## Graph identity, versioning and coherent response headers

Knotrel exposes explicit graph identity and version metadata on single operations
(`POST /v1/operations`, see "Single operations" for which outcomes), batch
responses (`POST /v1/batch`), live stats snapshots (`GET /v1/stats`), the
inspection reads (`GET /v1/nodes`, `/v1/nodes/{node}`, `/v1/nodes/{node}/edges`,
`/v1/edges`), atomic import responses (`POST /v1/feeder/import`), and the feeder
events, gap and barrier responses (including partial failures). Feeder session
and heartbeat responses, `/v1/info`, the probes and `/metrics` carry none; the
instance ID is also in `GET /v1/info` (`instance_id`, source-managed mode).
All query results and their accompanying metadata headers are captured under the
**same** graph lock acquisition boundary; the server never executes a query and
subsequently reacquires the lock to read metadata.

### Identity and version concepts

These concepts remain strictly distinct:

1. **`instance_id` (`X-Knotrel-Instance-Id`):**
   An ephemeral process incarnation identifier (format: `srv-<pid>-<nanos>-<counter>`)
   generated fresh at process startup. It changes on every start or restart,
   including separate server instances constructed from cloned configuration.
   In source-managed mode, every feeder request must supply this token; requests
   bearing an old `instance_id` from a previous server incarnation are rejected
   immediately with HTTP 409 `stale_instance_id`.

2. **`graph_id` (`X-Knotrel-Graph-Id`):**
   The single supported logical graph identifier, always `"default"`. Knotrel is a
   single-graph connectivity accelerator and does not implement dynamic multi-graph
   routing. Feeder payloads specifying any `graph_id` other than `"default"` are
   rejected with HTTP 400 `invalid_graph_id` without touching the graph.

3. **`generation` (`X-Knotrel-Generation`):**
   A monotonic 64-bit integer (`u64`) identifying the published baseline/rebuild
   lineage. It is `0` in standalone mode or before initial import, becomes `1`
   on the first atomic baseline import, and increments monotonically ($g \to g + 1$)
   on each subsequent atomic replacement.
   **Generation exhaustion:** If the generation counter reaches `u64::MAX`,
   subsequent replacement imports are rejected with HTTP 409 `generation_exhausted`
   without arithmetic overflow or wrapping, leaving live graph state untouched.

4. **`state_version` (`X-Knotrel-State-Version`):**
   An arbitrary-precision decimal local revision counter (`"0"`, `"1"`, `"2"`, ...),
   stored internally as little-endian digits without fixed-width integer wraparound.
   It starts at `"0"` on startup or upon publication of an imported graph image.
   Each effective graph mutation (`changed: true`) increments it once. Duplicate
   operations, missing-edge cuts, queries, and rejected operations never increment it.
   It counts local changes within the current published generation.

5. **`applied_source_position` (`X-Knotrel-Source-Position`):**
   An opaque UTF-8 cursor string (1 to 256 bytes) identifying the applied prefix
   of the authoritative external source stream. The server enforces the bound
   in bytes on every source position it accepts (event `source_position` and
   `prev_source_position`, import `source_position`, `barrier_source_position`)
   and refuses other values with 422 `invalid_request`; a multibyte string can
   exceed 256 bytes with fewer than 256 characters. The bound is checked at the
   request's existing validation step, so an earlier failure takes precedence:
   on events, after the session, lease, generation, initialization,
   desynchronization, `source_id` and `source_epoch` checks (a request with a
   different `source_epoch` and a bad position desynchronizes with 409
   `source_epoch_mismatch`); on a barrier, after the session, lease,
   generation, initialization, desynchronization and recovery-fence checks
   and before the sequence and position comparisons; on an import, after the instance and
   `graph_id` checks and before the session checks. OpenAPI's
   `minLength`/`maxLength` count characters and are a looser approximation. Knotrel never interprets this string
   as an integer, never parses its internal structure, and never compares cursors
   lexicographically.
   **HTTP safety:** In HTTP headers (`X-Knotrel-Source-Position` and `ETag`),
   source cursors are safely RFC 3986 percent-encoded (all characters other than
   `[a-zA-Z0-9\-_~]` are encoded as `%XX`). This prevents header injection,
   CRLF smuggling, and token delimiter collisions (such as dots or commas).

6. **`sync_status` (`X-Knotrel-Sync-Status`):**
   Reports current feeder synchronization status: `uninitialized`, `loading`,
   `catching_up`, `ready`, `stale`, or `desynchronized`.
   After an import the status is `catching_up` until a matching barrier
   verifies the stream (`ready`); see "Ordered incremental event stream and gap
   declaration".

### Replacement precondition exposure (`ETag` and `If-Match`)

Every query response, batch response, stats snapshot, and import publication returns
an `ETag` header formatted as:

```http
ETag: "<generation>.<state_version>.<percent_encoded_source_position>"
```

Example: `ETag: "1.0.pos-10"` (or `ETag: "1.0.0%2F16B3740"` when the cursor contains delimiters such as `/`), or `ETag: "0.15."` (in standalone mode where generation is 0 and source position is omitted).

This `ETag` is directly usable as the mandatory composite `If-Match` header when
performing an atomic replacement import (`POST /v1/feeder/import` with `mode: "replace"`).
The token guarantees that:
- Replacement succeeds only if the live graph remains exactly at the expected
  generation, state version, and applied source position.
- If any mutation or replacement occurred concurrently, or if the server restarted
  with a new `instance_id`, the token becomes stale and replacement is rejected with
  HTTP 412 `precondition_failed` or HTTP 409 `stale_instance_id`.

## Ordered incremental event stream and gap declaration

In source-managed mode (`KNOTREL_MODE=source_managed`), graph state advances solely
through authoritative feeder events. Direct mutations (`POST /v1/operations` with `link`/`cut`/`add_node`
and mutating batches) are rejected with HTTP 409 `direct_mutation_disabled`.

### Incremental update batches (`POST /v1/feeder/events`)

The feeder streams ordered incremental updates and progress events in batches of 1 to 1024 events.
Requests must include `Content-Type: application/json` and the mandatory `X-Knotrel-Instance-Id` header.

#### Envelope schema

```json
{
  "instance_id": "srv-12345678-1234-1234-1234-123456789abc",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-session-1",
  "feeder_epoch": 1,
  "source_id": "pg-cluster-prod-01",
  "source_epoch": "ep-1",
  "events": [
    {
      "event_id": "evt-001",
      "delivery_sequence": 1,
      "prev_delivery_sequence": 0,
      "source_position": "pos-01",
      "prev_source_position": "pos-00",
      "op": "link",
      "source": "1",
      "target": "2"
    },
    {
      "event_id": "evt-002",
      "delivery_sequence": 2,
      "prev_delivery_sequence": 1,
      "source_position": "pos-02",
      "prev_source_position": "pos-01",
      "op": "progress"
    }
  ]
}
```

#### Event operations

Each event specifies exactly one graph operation or progress marker:
- `add_node`: registers an isolated vertex (`{"op": "add_node", "node": "1"}`).
- `link`: inserts an undirected edge between `source` and `target`.
- `cut`: removes an undirected edge between `source` and `target`.
- `progress`: non-mutating source progress checkpoint (`{"op": "progress"}`).

With lifecycle support, three further operations are accepted. Every event
keeps the envelope fields above:

| `op` | Required | Forbidden |
| --- | --- | --- |
| `set_node_active` | `node`, `active` | `source`, `target`, `enabled` |
| `set_edge_enabled` | `source`, `target`, `enabled` | `node`, `active` |
| `remove_node` | `node` | `source`, `target`, `active`, `enabled` |

`add_node`, `link`, `cut` and `progress` must not carry `active` or `enabled`:
422 `invalid_request`, whatever the setting. With lifecycle support off, an
event using one of the three new operations rejects the whole request with 409
`topology_lifecycle_disabled` before any event is applied. The check runs in
the per-event prevalidation under the graph lock, after the session,
generation and source checks, in event order together with `invalid_request`
(the first failing event decides). Domain errors of lifecycle events
(`unknown_node`, `unknown_edge`, `self_loop`) follow invariant 6 below: 422
with the domain code for a single-event request, `partial_failure` with outer
code `event_failed` for a multi-event request. `remove_node` of an absent node
is an applied no-op.

#### Invariants and validation

1. **Standalone rejection:** Standalone servers reject feeder endpoints with HTTP 409 `mode_conflict`.
2. **Instance & session fencing:** Validates `instance_id` (409 `stale_instance_id`), `graph_id == "default"` (400 `invalid_graph_id`), `generation` (409 `stale_generation`), active feeder lease (`feeder_session_id` and `feeder_epoch`, 409 `fenced_feeder_session`), and unexpired lease (409 `lease_expired`).
3. **Source lineage binding:** `source_id` and `source_epoch` must match the baseline established at import. A `source_id` mismatch returns 409 `source_mismatch` without changing state; a `source_epoch` mismatch returns 409 `source_epoch_mismatch` and transitions the service to `desynchronized`.
4. **Sequence continuity and gaps:** Batch events must be contiguously ordered ($D_i = D_{prev} + 1$). The first event must satisfy $D_{prev} == D_{applied}$. A future sequence returns HTTP 409 `out_of_order_sequence` without changing state; a wrong `prev_source_position` returns 409 `source_predecessor_mismatch` and transitions the service to `desynchronized`.
5. **Bounded deduplication:** A dual-index ring buffer (default 10,000 entries, configurable via `KNOTREL_DEDUP_WINDOW_SIZE` from 1 to 1,000,000) provides deterministic retry protection:
   - **Exact duplicate retry:** Resending a retained sequence with identical `event_id` and canonical payload returns HTTP 200 `already_applied` (`result: "duplicate"`). Local revision and topology are untouched. The canonical payload is the operation type, the normalized endpoints and, for lifecycle events, the flag value; a retry that changes the operation or the flag is a conflicting duplicate.
   - **Conflicting duplicate:** Resending a retained sequence with different payload or `event_id`, or reusing an active `event_id` at a new sequence, returns HTTP 409 `conflicting_duplicate` and permanently transitions sync status to `desynchronized`.
   - **Evicted duplicate:** Retrying an event evicted from the window ($D \le D_{applied} - W$) returns HTTP 409 `unverifiable_duplicate`. State is preserved without desynchronization.
   - **Evicted event ID reuse:** An event ID evicted from the window can be reused fresh at $D_{applied} + 1$.
6. **Fail-fast batch semantics:** Processing stops at the first failing event (e.g. self-loop, capacity limit, `unknown_node` or `unknown_edge` 422, or a sequence/duplicate 409). Feeder events have no query operation and no 404 outcome. Whole-request checks (fencing, generation, source lineage, and every event's shape and bounds) run before any event is applied. The already committed prefix remains applied in memory (not durably); a multi-event partial failure reports `failed_batch_index`, `failed_delivery_sequence`, `failed_event_id`, and current applied frontier. Uncommitted events are not recorded in the deduplication window and may be retried at $D_{applied} + 1$ with corrected payload. The full failure matrix, including timeout and capacity outcomes, is in `integration-contract.md` §9.3.
7. **Sequence boundary behavior and checked arithmetic:** Delivery sequences are represented as unsigned 64-bit integers (`u64`). When $D_{applied} = \text{u64::MAX}$, checked arithmetic (`checked_add(1)`) prevents integer overflow panics. Because the wire protocol bounds delivery sequence to `u64`, no wire event can supply a sequence strictly greater than `u64::MAX` ($2^{64}$ is unrepresentable). Consequently, valid retained retries of sequence `u64::MAX` within the deduplication window return HTTP 200 `already_applied` (`result: "duplicate"`), while wrapped sequences ($D = 0$) or older evicted sequences are rejected with HTTP 409 `unverifiable_duplicate` without state mutation. The internal `SequenceExhausted` error variant acts as a defensive guard for checked sequence increments and is unreachable over the wire without protocol extensions beyond `u64`.
8. **No-op & progress tracking:** No-op mutations (e.g. linking existing edges) and `progress` events advance `applied_source_position` and delivery sequence without incrementing `state_version`.
9. **Desynchronized state event rejection:** If the graph transitions to `desynchronized` (due to declared gap, conflicting duplicate, predecessor mismatch, or source epoch mismatch), subsequent event submissions are rejected under lock with HTTP 409 `graph_desynchronized` without mutating topology, revision, delivery sequence, source position, or deduplication state. Diagnostic `/stats` remains available. Recovery requires an atomic rebuild via `POST /v1/feeder/import` (mode `replace`).

### Declared unrepairable gap (`POST /v1/feeder/gap`)

When the feeder experiences unrecoverable source data loss (e.g. truncated WAL or expired event retention), it declares an explicit gap:

```json
{
  "instance_id": "srv-12345678-1234-1234-1234-123456789abc",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-session-1",
  "feeder_epoch": 1,
  "failed_delivery_sequence": 1500,
  "reason": "PostgreSQL replication slot WAL removed"
}
```

- Returns HTTP 200 `{"result": "gap_acknowledged", "status": "desynchronized"}`.
- Transitions sync status to `desynchronized`.
- Subsequent queries return HTTP 503 `desynchronized`.
- Feeder heartbeats renew session lease but probe reports `desynchronized`.
- Live graph state remains intact in memory for diagnostics; recovery requires an atomic graph replacement via `POST /v1/feeder/import`.

### Synchronization barrier verification (`POST /v1/feeder/barrier`)

In source-managed mode, the feeder declares synchronization completion at barrier position $B$ ($D_{barrier}, P_{barrier}$) via `POST /v1/feeder/barrier`. The barrier is strictly verification-only:
- It **never** mutates graph topology.
- It **never** advances `applied_source_position` or `delivery_sequence`.
- It asserts under the graph lock that Knotrel has ingested and applied all events up to sequence $D_B$ and cursor $P_B$.

#### Request envelope schema

```json
{
  "instance_id": "srv-12345678-1234-1234-1234-123456789abc",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-session-1",
  "feeder_epoch": 1,
  "barrier_delivery_sequence": 102,
  "barrier_source_position": "0/16B3800"
}
```

- **Headers:** `Content-Type: application/json` and mandatory `X-Knotrel-Instance-Id: <instance_id>`.
- `barrier_delivery_sequence`: required unsigned 64-bit integer ($D_B$).
- `barrier_source_position`: optional string cursor ($P_B$), 1 to 256 bytes of UTF-8 when present (otherwise 422 `invalid_request`). If omitted, only delivery sequence $D_B$ is validated.

#### Precondition checks (Evaluated under graph lock)

1. **Operating mode:** Standalone servers reject the request with HTTP 409 `mode_conflict`.
2. **Instance identity:** Request `instance_id` must match live server incarnation; mismatch returns HTTP 409 `stale_instance_id`.
3. **Graph identity:** `graph_id` must equal `"default"`; any other ID returns HTTP 400 `invalid_graph_id`.
4. **Feeder ownership & fencing:** `feeder_session_id` and `feeder_epoch` must match the active registered lease. Mismatches return HTTP 409 `fenced_feeder_session`.
5. **Monotonic lease expiry:** Current monotonic time must not exceed `lease_deadline`. Expired lease returns HTTP 409 `lease_expired`.
6. **Generation check:** Request `generation` must match live published generation; mismatch returns HTTP 409 `stale_generation`.
7. **Worker health:** Permanent worker failure returns HTTP 503 `unavailable`.
8. **Recovery fence:** after a lease expiry, a barrier under `feeder_epoch <= last_expired_epoch` returns HTTP 409 `fenced_feeder_session`, whatever its sequence and position (see below).

#### Error precedence

When a barrier meets several rejection conditions, the first in this order
decides the response. Every rejection leaves the graph, the applied sequence
and position, the synchronization status and `/ready` unchanged.

1. Operating mode: `409 mode_conflict`.
2. `X-Knotrel-Instance-Id` header, then (after the body is parsed) body
   `instance_id`: `409 stale_instance_id`.
3. `graph_id`: `400 invalid_graph_id`.
4. Service availability and the feeder data lane: `503 unavailable`, `503 busy`.
5. Permanent worker failure (B6): `503 unavailable`.
6. Session and lease: `409 session_required`, then `409 fenced_feeder_session`
   (session or epoch mismatch), then `409 lease_expired`.
7. Generation: `409 stale_generation`.
8. Uninitialized or initial loading (B3, B4): `409 graph_uninitialized`.
9. Desynchronized (B5): `409 graph_desynchronized`.
10. Recovery fence (`feeder_epoch <= last_expired_epoch`): `409 fenced_feeder_session`.
11. `barrier_source_position` outside 1 to 256 bytes: `422 invalid_request`.
12. Premature sequence (B8): `409 barrier_not_reached`.
13. Older sequence (B9): `409 superseded_barrier`.
14. Position mismatch at equal sequence (B7): `409 barrier_position_mismatch`.
15. Exact match (B1, B2): `200`.

Before v1.0.0 the recovery fence came after B8, B9 and B7, so a fenced
barrier with a wrong sequence or position got `barrier_not_reached`,
`superseded_barrier` or `barrier_position_mismatch`. The reorder is an
approved v1.0.0 exception
([compatibility.md](compatibility.md#3-what-compatible-means)).

#### Barrier Decision Table (B1 through B9)

Under the graph mutex, $(D_B, P_B)$ is matched against live frontier $(D_{applied}, P_{applied})$:

| Case | Sequence Condition | Position Condition | Current Sync State | Outcome & HTTP Response | Resulting Sync State |
|---|---|---|---|---|---|
| **B1: Exact Match (Restores Ready)** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `CatchingUp` or `Stale` (published $g > 0$) | **`200 OK`** (`result: "synchronized"`, `status: "ready"`) | Transitions to **`Ready`** |
| **B2: Exact Match (Affirms Ready)** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Ready` (published $g > 0$) | **`200 OK`** (`result: "already_synchronized"`, `status: "ready"`) | Remains **`Ready`** |
| **B3: Exact Match While Uninitialized** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Uninitialized` ($g == 0$) | **`409 Conflict`** (`code: "graph_uninitialized"`) | Remains **`Uninitialized`** |
| **B4: Exact Match While Initial Loading** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Loading` (initial baseline, $g == 0$) | **`409 Conflict`** (`code: "graph_uninitialized"`) | Remains **`Loading`** |
| **B5: Exact Match While Desynchronized** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Desynchronized` | **`409 Conflict`** (`code: "graph_desynchronized"`) | Remains **`Desynchronized`** |
| **B6: Exact Match While Failed** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Failed` | **`503 Service Unavailable`** (`code: "unavailable"`) | Remains **`Failed`** |
| **B7: Position Mismatch at Equal Sequence** | $D_B == D_{applied}$ | $P_B \ne P_{applied}$ | Any | **`409 Conflict`** (`code: "barrier_position_mismatch"`) | **Unchanged** |
| **B8: Premature Sequence** | $D_B > D_{applied}$ | Any | Any | **`409 Conflict`** (`code: "barrier_not_reached"`) | **Unchanged** |
| **B9: Older Sequence (Conservative Rejection)** | $D_B < D_{applied}$ | Any | Any | **`409 Conflict`** (`code: "superseded_barrier"`) | **Unchanged** |

#### Heartbeat lease renewal versus synchronization proof

- When a feeder lease expires, Knotrel transitions to `Stale`.
- A subsequent heartbeat (`POST /v1/feeder/heartbeat`) extends the monotonic lease deadline and returns HTTP 200, but **does not restore `Ready`**. The server remains in `Stale`.
- Readiness returns to `Ready` **only** upon a successful barrier declaration (Case B1).
- Heartbeats **never** clear `Desynchronized`, `Uninitialized`, `Loading`, `CatchingUp`, or `Failed` states.

#### Recovery fencing across lease expiry and session takeover

In-flight barriers from a superseded, disconnected, or expired feeder session cannot corrupt live synchronization state:
- **Recovery fencing across lease expiry:** If a lease expired, Knotrel records the expired epoch (`last_expired_epoch`). Even if the same feeder re-extends the lease via heartbeat, any barrier submitted under `feeder_epoch <= last_expired_epoch` is rejected with HTTP 409 `fenced_feeder_session`, whatever its sequence and position (the fence precedes B8, B9 and B7). Readiness recovery from `Stale` strictly requires registering a newly incremented epoch (`POST /v1/feeder/session` with `feeder_epoch > last_expired_epoch`) and submitting fresh barrier verification proof.
- **Session takeover fencing:** If a session takeover occurred, incoming barrier requests bearing an older `feeder_epoch` fail precondition checks with HTTP 409 `fenced_feeder_session`.
- **Superseded sequence:** If new events were ingested by the active session, the applied delivery sequence has advanced ($D_{applied} > D_B$), causing older barriers to be conservatively rejected under Case B9 with HTTP 409 `superseded_barrier`.
- In all cases, live graph topology, sequence, cursor, and synchronization status remain strictly untouched.

## External feeder operational lifecycle and recovery

### Operational monitoring

Operators and feeder processes monitor Knotrel instances through four primary HTTP endpoints:
1. **Liveness (`GET /health`):** Returns HTTP 200 `OK` as long as the HTTP listener and background worker threads are running. Liveness probes should not restart instances merely because synchronization is in progress.
2. **Readiness (`GET /ready`):** Returns HTTP 200 when `status == "ready"` (or standalone server is initialized). Returns HTTP 503 during `uninitialized`, `loading`, `catching_up`, `stale`, `desynchronized`, `draining`, or `unavailable`. Load balancers and service meshes must direct query traffic only to ready instances.
3. **Instance Discovery (`GET /v1/info`):** In source-managed mode returns `instance_id` and `mode: "source_managed"` in addition to the effective settings (both fields are absent in standalone mode). No endpoint reports the server software version. Feeders poll `/v1/info` to discover ephemeral server instance IDs and detect process restarts.
4. **Live Statistics (`GET /v1/stats`):** Returns live topology cardinality (`node_count`, `edge_count`, and with lifecycle support `active_node_count`, `enabled_edge_count`, `effective_edge_count`), graph limits, and current synchronization metadata (`generation`, `state_version`, `source_position`, `sync_status`). The `ETag` header exposes the exact `<generation>.<state_version>.<percent_encoded_source_position>` token required for optimistic concurrency on replacement imports (`mode: "replace"`).

### Standard synchronization lifecycle

A protocol-compliant feeder coordinates synchronization using the following sequence:
```
1. Discover:    GET /v1/info                     -> Read ephemeral instance_id
2. Lease:       POST /v1/feeder/session          -> Acquire session lease (epoch 1)
3. Keepalive:   POST /v1/feeder/heartbeat        -> Background worker ticks at lease_duration / 3
4. Baseline:    POST /v1/feeder/import           -> Upload snapshot S (mode "create_only")
5. Stream:      POST /v1/feeder/events           -> Stream ordered updates (D = 1..N)
6. Promote:     POST /v1/feeder/barrier          -> Assert high-water mark B (status -> "ready")
7. Incremental: POST /v1/feeder/events           -> Continuous steady-state updates
```

### Recovery procedures

Because Knotrel maintains volatile, in-memory graph state without local persistent storage:
1. **Server restart or process crash:**
   - The newly started Knotrel instance generates a fresh `instance_id` and starts with `sync_status: "uninitialized"`.
   - Existing feeder connections receive HTTP 409 `stale_instance_id`.
   - The feeder must rediscover the new `instance_id` via `GET /v1/info` and perform a clean initial rebuild (`mode: "create_only"`).
2. **Feeder lease expiry:**
   - When a network partition or feeder pause causes the lease deadline to pass, Knotrel transitions to `stale` (and `/ready` returns HTTP 503).
   - Feeder heartbeats renew the lease deadline but keep status `stale`.
   - To restore `ready`, the feeder must register a bumped `feeder_epoch > last_expired_epoch` and declare barrier proof matching the current source position.
3. **Declared gap or unrecoverable desynchronization:**
   - When source history is lost (or after declaring `POST /v1/feeder/gap`), Knotrel enters `desynchronized`.
   - To recover, the feeder reads the current `ETag` via `GET /v1/stats` and submits an atomic replacement import via `POST /v1/feeder/import` (`mode: "replace"`, `If-Match: "<etag>"`).
   - Once the replacement snapshot is published, the feeder replays contiguous events and declares a fresh barrier to restore `ready`.

### Executable examples and tooling

- **Reference Feeder:** Build and execute the full integration lifecycle against a real child server process:
  ```bash
  cargo build --bin knotrel-server --example external_feeder
  cargo run --example external_feeder -- --spawn-server
  ```
  Exercises server discovery, observable heartbeat lease renewals, baseline snapshot import, post-snapshot catch-up event streaming, barrier promotion, independent oracle verification, local fault proxy response drop recovery, and crash/rebuild recovery.
- **Reproducible Measurements:** Run automated end-to-end benchmarks with separated pure query timing, independent BFS ground-truth verification, sampled maximum process RSS (polled every 25ms; brief transient allocations between sampling intervals may not be captured) alongside final sampled process RSS, and statistical aggregation across repeated runs:
  ```bash
  cargo build --bin knotrel-server --example measure_e2e
  ./scripts/measure-e2e.sh small
  # For release performance benchmarks with repeated runs:
  ./scripts/measure-e2e.sh --release --runs 3 medium
  ```
  Individual measurement reports are retained with timestamps in `target/measurements/` without overwriting prior evidence.
