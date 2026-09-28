# HTTP API v1

The server owns one in-memory undirected simple graph. Default address:
`127.0.0.1:8080`. There is no authentication, persistence or TLS termination in
this initial server. Use it as a local reference service.

## Single operations

Send JSON to `POST /v1/operations` with `Content-Type: application/json`.

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
| 422 | Invalid fields/IDs (`invalid_request`), self-loop (`self_loop`), or batch size (`invalid_batch_size`) |
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

The job limit covers admitted queued plus running graph work, not concurrent
mutation workers. All graph operations still acquire the same mutex. Body
size (1 MiB) and batch size (1–1024) are fixed in this version.

`GET /v1/info` returns HTTP 200 and immutable effective settings, for example:

```json
{
  "engine": "ett",
  "experimental": true,
  "max_pending_jobs": 16,
  "max_body_bytes": 1048576,
  "max_batch_operations": 1024
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
