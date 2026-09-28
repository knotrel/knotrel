# Rust foundation

Knotrel provides exact connectivity for an in-memory, undirected simple graph.
This first implementation is a reference engine, not an optimized dynamic
connectivity algorithm. It establishes semantics and a measurable baseline.

## Components

The `knotrel` Cargo workspace contains `knotrel-core` (synchronous, dependency-free)
and `knotrel-server` (Axum and Tokio). The independent [`knotrel-benchmarks`](https://github.com/knotrel/knotrel-benchmarks)
workspace consumes the sibling core by path during local development.
Both workspaces pin Rust 1.98.1, edition 2024, resolver 3. Packages are unpublished.
Every exported item needs English Rustdoc; nontrivial private algorithms document
invariants, correctness and complexity. CI checks formatting, Clippy, tests and docs.

## Graph contract

- Node IDs are unsigned 64-bit integers. HTTP encodes them as decimal strings to
  preserve precision in JavaScript clients.
- `add_node` creates isolated vertices; `link` implicitly creates its endpoints.
- Edges are undirected, unique and have distinct endpoints. Self-loops are errors.
- Repeated links and cuts are idempotent; cuts leave vertices alive.
- Queries require both vertices to exist; an existing vertex connects to itself.
- The original `ReferenceGraph` uses ordered adjacency sets: O(log V) link/cut
  and O((V + E) log V) queries. The default `Graph` now uses stable indices and
  sorted compact adjacency: O(V + E) queries with O(V) per-call scratch. Updates
  cost O(log V + deg(source) + deg(target)) amortized; adding vertices can trigger
  O(V) outer-vector growth. Logical storage is O(V + E); retained capacity tracks
  historical per-vertex maximum degrees. See [compact core](0004-compact-core.md).

## HTTP contract

A single process owns one graph. `POST /v1/operations` executes one operation;
`POST /v1/batch` executes 1–1024 operations in order with per-operation results.
Batches are isolated from other requests, but are **not rollback transactions**:
an operation error leaves earlier successes in place and processing continues.
Malformed JSON or an invalid batch size executes nothing. A mutex protects the
whole request, inside a blocking task. A semaphore admits at most 32 requests;
excess requests receive 503 without joining an unbounded worker queue. The permit
stays with the worker if its HTTP caller disconnects. Bodies are limited to 1 MiB.

The server defaults to loopback, supports graceful shutdown and stores everything
in memory. Persistence, authentication, graph namespaces, distributed execution
and optimized connectivity structures are future work. A disconnected client
must treat an admitted mutation as having an unknown outcome and retry idempotently.

## Benchmark contract

The first harness runs a deterministic chain split/rejoin workload against the
core, validates every query and emits JSON metadata and timing. Correctness checks
and graph construction are outside timed operation intervals. This is a local
baseline; GraphScope and HTTP adapters are not implemented. Comparisons must use
the same graph, update trace, query answers and visibility guarantees, and report
hardware, versions, seeds, memory, latency distributions and total runtime.
