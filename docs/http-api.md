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

Each request body is limited to 1 MiB. At most 32 graph jobs can be queued or
running. Admission occurs after JSON parsing; this limit bounds graph work, not
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
