# HTTP API v1

This document is the v0.1.0 preview contract. Every statement here is backed by
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
  after it completes. Graph state changes only through `/v1/operations` and
  `/v1/batch`.
- **Restart loses the whole graph.** Nothing is written to disk and nothing is
  recovered. A clean shutdown (SIGINT/SIGTERM) discards the graph too: the next
  process always starts empty.

## Non-guarantees

The v0.1.0 preview does not provide any of the following:

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

| Resource | Bound in v0.1.0 |
| --- | --- |
| Request body | 1 MiB, fixed; larger bodies get 413 |
| Batch length | 1–1024 operations, fixed |
| Queued plus running graph jobs | `KNOTREL_MAX_PENDING_JOBS`, default 32; excess gets 503 `busy` |
| Graph nodes / edges | `KNOTREL_MAX_NODES` / `KNOTREL_MAX_EDGES`; default 100 000 nodes and 500 000 edges, `unlimited` is an explicit opt-out |
| Header read and keep-alive idle time | `KNOTREL_HEADER_READ_TIMEOUT_MS`, default 5000 ms |
| Request time (body receipt plus handling) | `KNOTREL_REQUEST_TIMEOUT_MS`, default 30000 ms |
| Concurrent connections | `KNOTREL_MAX_CONNECTIONS`, default 1024; excess gets 503 `too_many_connections` |
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

Knotrel v0.1.0 has **no authentication, authorization or TLS**. Anyone who can
open a TCP connection to the listener can read and mutate the graph. The
supported deployment is a single trusted local client:

- The listener defaults to `127.0.0.1:8080`. A non-loopback `KNOTREL_ADDR`
  (including `0.0.0.0` and `[::]`) fails startup unless
  `KNOTREL_ALLOW_NON_LOOPBACK=true` is also set. When it is set the executable
  logs a warning at startup.
- Opting in is an operator acknowledgement, not a hardening step. Exposure to
  untrusted clients, including the public internet, stays unsupported.
- If you must expose it to other hosts, put it behind a reverse proxy or
  gateway that provides TLS, authentication, authorization, rate limiting and
  rate limiting and write timeouts, and restrict the network path. A private network on its own is not
  sufficient: every host on it is then trusted with full read/write access.
- Logs contain the method, path, status and latency of each request and the
  startup settings. They never contain request bodies, node IDs or parser
  messages. Client error messages never quote request input, parser positions or
  internal paths.

## Request validation order

For `POST /v1/operations` and `POST /v1/batch`, the first failing step decides
the response, and nothing is executed unless every step passes:

| Step | Check | Failure |
| --- | --- | --- |
| 1 | Route and method | 404 / 405 (Axum default, empty body) |
| 2 | `Content-Type` is a JSON media type | 415 `invalid_request` |
| 3 | Body is at most 1 MiB | 413 `invalid_request` |
| 4 | Body is syntactically valid JSON | 400 `invalid_request` |
| 5 | Shape: known `op`, required fields, no unknown or duplicate fields, valid node IDs (the whole batch) | 422 `invalid_request` |
| 6 | Batch length is 1–1024 | 422 `invalid_batch_size` |
| 7 | A graph job slot is available | 503 `busy` / `unavailable` |
| 8 | Operations run in order under the graph lock | per-operation outcome |

Steps 2–6 never touch the graph, so a rejected request changes nothing. Step 7
happens after parsing; `busy` means no operation was admitted.

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
| 400 | Malformed JSON (`invalid_request`) |
| 404 | Unknown query vertex (`unknown_node`) |
| 413 | Body exceeds 1 MiB (`invalid_request`) |
| 415 | Missing/unsupported JSON content type (`invalid_request`) |
| 422 | Invalid fields/IDs (`invalid_request`), self-loop (`self_loop`), batch size (`invalid_batch_size`), or graph capacity (`node_limit_exceeded`, `edge_limit_exceeded`) |
| 503 | Graph job capacity exhausted (`busy`) or worker unavailable (`unavailable`) |

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

One mutex spans the entire batch, so other graph requests cannot interleave with
it. Separate requests execute in lock-acquisition order, not necessarily arrival
order. A connectivity result reflects all earlier operations in that order.

Batch response headers (`ETag`, `X-Knotrel-State-Version`, `X-Knotrel-Generation`,
`X-Knotrel-Instance-Id`, etc.) are captured under the same lock acquisition and
reflect the graph's **final** coherent state after all operations in the batch
have completed. Earlier operations in the batch are not labeled individually with
intermediate revisions: continue-on-error applies each mutation in order, and only
effective mutations (`changed: true`) advance the revision.

## Concurrency and order

Every graph operation, including queries and `GET /v1/stats`, takes the same
mutex. Overlapping requests are therefore serialized: the outcome equals some
sequential order of whole requests (a batch is one indivisible unit in that
order). Queries are not run in parallel with each other or with mutations. The
order is the order of lock acquisition and is not otherwise specified, so
clients must not infer ordering between requests that overlap in time. A
request that is admitted but whose client disconnects can still complete.

## Admission and cancellation

Each request body is limited to 1 MiB. By default, at most 32 graph jobs can be
queued or running; `KNOTREL_MAX_PENDING_JOBS` sets this limit at startup.
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

Readiness enforces a strict 7-level evaluation precedence:
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
lifecycle phases, and `serve(...)` drives them.

### Graceful shutdown

On the first SIGINT or SIGTERM (Unix; Ctrl-C elsewhere) the server:

1. switches `/ready` to 503 `draining`;
2. keeps accepting connections only so it can answer them: any request other
   than `/health` or `/ready` is rejected with 503 and
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

## Startup configuration and engine selection

The standalone executable reads a single configuration snapshot before opening
its listener. There is no file loader or runtime reload in this version.

| Variable | Accepted value | Default |
| --- | --- | --- |
| `KNOTREL_ADDR` | IP socket address, such as `127.0.0.1:8080` or `[::1]:8080`; non-loopback requires `KNOTREL_ALLOW_NON_LOOPBACK=true` | `127.0.0.1:8080` |
| `KNOTREL_ALLOW_NON_LOOPBACK` | Exactly `true` or `false` | `false` |
| `KNOTREL_ENGINE` | Exactly `compact-bfs`, `ett` or `hdt` | `compact-bfs` |
| `KNOTREL_MODE` | Exactly `standalone` or `source_managed` | `standalone` |
| `KNOTREL_MAX_IMPORT_BODY_BYTES` | ASCII decimal integer from 1 through 1073741824 | `67108864` (64 MiB) |
| `KNOTREL_DEDUP_WINDOW_SIZE` | ASCII decimal integer from 1 through 1000000 | `10000` |
| `KNOTREL_MAX_PENDING_JOBS` | ASCII decimal integer from 1 through Tokio's `Semaphore::MAX_PERMITS` | `32` |
| `KNOTREL_MAX_NODES` | Nonnegative ASCII decimal integer fitting `usize`, or exactly `unlimited` | `100000` |
| `KNOTREL_MAX_EDGES` | Nonnegative ASCII decimal integer fitting `usize`, or exactly `unlimited` | `500000` |
| `KNOTREL_MAX_CONNECTIONS` | ASCII decimal integer from 1 through 1000000 | `1024` |
| `KNOTREL_HEADER_READ_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `5000` |
| `KNOTREL_REQUEST_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `30000` |
| `KNOTREL_SHUTDOWN_TIMEOUT_MS` | ASCII decimal milliseconds from 1 through 3600000 | `10000` |

Unknown variables in the reserved `KNOTREL_` namespace, empty or non-Unicode
values, unsupported engines and invalid addresses/limits fail startup. Values
are not trimmed; engine names are case-sensitive. Unrelated environment
variables are ignored by this loader. `RUST_LOG` is handled separately by the
logging subscriber (falling back to `info` if its filter is missing or invalid).
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
`GET /v1/stats`, not concurrent mutation workers. All graph operations still acquire the same mutex. Body
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
contains no graph data. It does not report the lifecycle phase; use `/ready`. Engine and
limit fields describe the actual service instance, including embedded routers.
The default response has `engine: "compact-bfs"`, `experimental: false` and
`max_pending_jobs: 32`.

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
| Graph mutex | Not acquired | Acquired on a blocking worker |
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
  two endpoints still increments once. A batch increments once per effective
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
The marker is protected by the graph mutex and does not depend on mutex poisoning.
A partial mutation is not rolled back and cannot be read through `/v1/stats`.
Recover by replacing/restarting the service; in-memory data is not persisted.
Disconnecting an admitted stats caller does not release its permit before its worker finishes. `/v1/info`
`/health` and `/ready` remain independent of this queue.

## Graph identity, versioning and coherent response headers

Knotrel exposes explicit graph identity and version metadata across graph query
responses (`POST /v1/operations`), batch responses (`POST /v1/batch`), live stats
snapshots (`GET /v1/stats`), and atomic import responses (`POST /v1/feeder/import`).
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
   of the authoritative external source stream. Knotrel never interprets this string
   as an integer, never parses its internal structure, and never compares cursors
   lexicographically.
   **HTTP safety:** In HTTP headers (`X-Knotrel-Source-Position` and `ETag`),
   source cursors are safely RFC 3986 percent-encoded (all characters other than
   `[a-zA-Z0-9\-_~]` are encoded as `%XX`). This prevents header injection,
   CRLF smuggling, and token delimiter collisions (such as dots or commas).

6. **`sync_status` (`X-Knotrel-Sync-Status`):**
   Reports current feeder synchronization status: `uninitialized`, `loading`,
   `catching_up`, `ready`, `stale`, or `desynchronized`.
   *(Note: Ordered update streaming and progress events belong to Feature 0.2.4;
   synchronization barrier verification and automated readiness transitions belong
   to Feature 0.2.5. After Feature 0.2.2/0.2.3 import, source-managed mode remains
   in `catching_up`.)*

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
      "op": "link",
      "source": "1",
      "target": "2"
    },
    {
      "event_id": "evt-002",
      "delivery_sequence": 2,
      "prev_delivery_sequence": 1,
      "source_position": "pos-02",
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

#### Invariants and validation

1. **Standalone rejection:** Standalone servers reject feeder endpoints with HTTP 409 `mode_conflict`.
2. **Instance & session fencing:** Validates `instance_id` (409 `stale_instance_id`), `graph_id == "default"` (400 `invalid_graph_id`), `generation` (409 `stale_generation`), active feeder lease (`feeder_session_id` and `feeder_epoch`, 409 `fenced_feeder_session`), and unexpired lease (409 `lease_expired`).
3. **Source lineage binding:** `source_id` and `source_epoch` must match the baseline established at import. Any mismatch returns 409 `source_mismatch` or `source_epoch_mismatch` and transitions the service to `desynchronized`.
4. **Sequence continuity and gaps:** Batch events must be contiguously ordered ($D_i = D_{prev} + 1$). The first event must satisfy $D_{prev} == D_{applied}$. Gaps or out-of-order delivery return HTTP 409 `gap_detected`.
5. **Bounded deduplication:** A dual-index ring buffer (default 10,000 entries, configurable via `KNOTREL_DEDUP_WINDOW_SIZE` from 1 to 1,000,000) provides deterministic retry protection:
   - **Exact duplicate retry:** Resending a retained sequence with identical `event_id` and canonical payload returns HTTP 200 `already_applied` (`result: "duplicate"`). Local revision and topology are untouched.
   - **Conflicting duplicate:** Resending a retained sequence with different payload or `event_id`, or reusing an active `event_id` at a new sequence, returns HTTP 409 `conflicting_duplicate` and permanently transitions sync status to `desynchronized`.
   - **Evicted duplicate:** Retrying an event evicted from the window ($D \le D_{applied} - W$) returns HTTP 409 `unverifiable_duplicate`. State is preserved without desynchronization.
   - **Evicted event ID reuse:** An event ID evicted from the window can be reused fresh at $D_{applied} + 1$.
6. **Fail-fast batch semantics:** Processing stops at the first domain error (e.g. self-loop 422, unknown node 404, capacity limits 422). The already committed prefix remains applied in memory; partial failure reports `failed_batch_index`, `failed_delivery_sequence`, `failed_event_id`, and current applied frontier. Uncommitted events are not recorded in the deduplication window and may be retried at $D_{applied} + 1$ with corrected payload.
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
- `barrier_source_position`: optional string cursor ($P_B$). If omitted, only delivery sequence $D_B$ is validated.

#### Precondition checks (Evaluated under graph lock)

1. **Operating mode:** Standalone servers reject the request with HTTP 409 `mode_conflict`.
2. **Instance identity:** Request `instance_id` must match live server incarnation; mismatch returns HTTP 409 `stale_instance_id`.
3. **Graph identity:** `graph_id` must equal `"default"`; any other ID returns HTTP 400 `invalid_graph_id`.
4. **Feeder ownership & fencing:** `feeder_session_id` and `feeder_epoch` must match the active registered lease. Mismatches return HTTP 409 `fenced_feeder_session`.
5. **Monotonic lease expiry:** Current monotonic time must not exceed `lease_deadline`. Expired lease returns HTTP 409 `lease_expired`.
6. **Generation check:** Request `generation` must match live published generation; mismatch returns HTTP 409 `stale_generation`.
7. **Worker health:** Permanent worker failure returns HTTP 503 `unavailable`.

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
- **Recovery fencing across lease expiry:** If a lease expired, Knotrel records the expired epoch (`last_expired_epoch`). Even if the same feeder re-extends the lease via heartbeat, any barrier submitted under `feeder_epoch <= last_expired_epoch` is rejected with HTTP 409 `fenced_feeder_session`. Readiness recovery from `Stale` strictly requires registering a newly incremented epoch (`POST /v1/feeder/session` with `feeder_epoch > last_expired_epoch`) and submitting fresh barrier verification proof.
- **Session takeover fencing:** If a session takeover occurred, incoming barrier requests bearing an older `feeder_epoch` fail precondition checks with HTTP 409 `fenced_feeder_session`.
- **Superseded sequence:** If new events were ingested by the active session, the applied delivery sequence has advanced ($D_{applied} > D_B$), causing older barriers to be conservatively rejected under Case B9 with HTTP 409 `superseded_barrier`.
- In all cases, live graph topology, sequence, cursor, and synchronization status remain strictly untouched.
