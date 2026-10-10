# HTTP API v1

The server owns one in-memory undirected simple graph. Default address:
`127.0.0.1:8080`. There is no authentication, persistence or TLS termination in
this initial server. Use it as a local reference service.

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

Clients should branch on `code`, not human-readable message text. Unregistered
paths/methods use Axum's default 404/405 responses, which may have an empty body.

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

## Admission, cancellation and health

Each request body is limited to 1 MiB. By default, at most 32 graph jobs can be
queued or running; `KNOTREL_MAX_PENDING_JOBS` sets this limit at startup. Admission occurs after JSON parsing; this limit bounds graph work, not
the number of network connections or concurrently buffered HTTP bodies. Workers
run outside Tokio's async executor. `busy` means no graph operation was admitted.

Once admitted, a job may complete after a client disconnects. A lost response or
`unavailable` error leaves the mutation outcome uncertain. Mutations can be
retried idempotently, although `changed` may differ on retry. A retry of a batch
containing queries can observe newer state. There is no request deduplication or
exactly-once delivery guarantee.

`GET /health` returns `{"status":"ok"}` without taking the graph lock. This is
process liveness, not a graph readiness or capacity check. Shutdown waits for
active work; it does not impose a hard deadline on graph traversal or slow clients.


## Startup configuration and engine selection

The standalone executable reads a single configuration snapshot before opening
its listener. There is no file loader or runtime reload in this version.

| Variable | Accepted value | Default |
| --- | --- | --- |
| `KNOTREL_ADDR` | IP socket address, such as `127.0.0.1:8080` or `[::1]:8080` | `127.0.0.1:8080` |
| `KNOTREL_ENGINE` | Exactly `compact-bfs`, `ett` or `hdt` | `compact-bfs` |
| `KNOTREL_MAX_PENDING_JOBS` | ASCII decimal integer from 1 through Tokio's `Semaphore::MAX_PERMITS` | `32` |
| `KNOTREL_MAX_NODES` | Nonnegative ASCII decimal integer fitting `usize` | unlimited |
| `KNOTREL_MAX_EDGES` | Nonnegative ASCII decimal integer fitting `usize` | unlimited |

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
contains no graph data. Like `/health`, it is not a readiness check. Engine and
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
at startup. Missing values are unlimited; zero is allowed. Empty, signed,
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
and `/health` remain independent of this queue and are not readiness checks.
