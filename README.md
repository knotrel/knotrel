# Knotrel

[![Version](https://img.shields.io/github/v/release/knotrel/knotrel?color=blue&label=version)](https://github.com/knotrel/knotrel/releases)
[![License: ELv2](https://img.shields.io/badge/License-Elastic%202.0-blue.svg)](LICENSE.md)
[![CI](https://github.com/knotrel/knotrel/actions/workflows/ci.yml/badge.svg)](https://github.com/knotrel/knotrel/actions/workflows/ci.yml)
[![Lint](https://github.com/knotrel/knotrel/actions/workflows/lint.yml/badge.svg)](https://github.com/knotrel/knotrel/actions/workflows/lint.yml)
[![Vulnerabilities](https://github.com/knotrel/knotrel/actions/workflows/vulnerabilities.yml/badge.svg)](https://github.com/knotrel/knotrel/actions/workflows/vulnerabilities.yml)
[![Coverage](https://codecov.io/gh/knotrel/knotrel/branch/main/graph/badge.svg)](https://codecov.io/gh/knotrel/knotrel)
[![Quality Gate](https://sonarcloud.io/api/project_badges/measure?project=knotrel_knotrel&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=knotrel_knotrel)
[![Security Rating](https://sonarcloud.io/api/project_badges/measure?project=knotrel_knotrel&metric=security_rating)](https://sonarcloud.io/summary/new_code?id=knotrel_knotrel)
[![Reliability Rating](https://sonarcloud.io/api/project_badges/measure?project=knotrel_knotrel&metric=reliability_rating)](https://sonarcloud.io/summary/new_code?id=knotrel_knotrel)

Dynamic connectivity for undirected graphs: insert edges, delete edges, and ask
whether two vertices are connected.

The default core uses compact adjacency and breadth-first traversal. It supports
growing vertex sets and retains the original ordered-adjacency engine as
`ReferenceGraph` for comparison. An experimental Euler-tour forest is available
explicitly; no production scalability claim has been established.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `knotrel-core` | Dependency-free synchronous graph operations and exact connectivity |
| `knotrel-server` | Axum HTTP API, validation and bounded blocking graph work |

Rust **1.98.1**, edition **2024**, and Cargo resolver **3** are pinned. The
toolchain file installs Clippy and rustfmt when using rustup. Cargo.lock records
the resolved dependency versions. Packages are currently unpublished.

## Run

```sh
cargo run --release --locked -p knotrel-server
```

The server listens on `127.0.0.1:8080` and uses compact BFS by default.
Configuration is read once before binding:

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_ADDR` | `127.0.0.1:8080` | IP address and port |
| `KNOTREL_ENGINE` | `compact-bfs` | `compact-bfs`, experimental `ett` or `hdt` |
| `KNOTREL_MAX_PENDING_JOBS` | `32` | Positive limit on queued plus running graph jobs |
| `KNOTREL_MAX_NODES` | unlimited | Maximum registered nodes; zero allowed |
| `KNOTREL_MAX_EDGES` | unlimited | Maximum live edges; zero allowed |
| `RUST_LOG` | `info` | Logging filter |

For example, to opt into the experimental forest:

```sh
KNOTREL_ENGINE=ett KNOTREL_MAX_PENDING_JOBS=16 \
  cargo run --release --locked -p knotrel-server
curl -sS http://127.0.0.1:8080/v1/info
```

Invalid values and unknown `KNOTREL_` variables stop startup. `hdt` selects the
experimental level-based HDT engine. `GET /v1/info` reports the effective engine and limits without
locking the graph. Settings cannot be changed through operation requests.
See [configuration and API details](docs/http-api.md#startup-configuration-and-engine-selection).

Ctrl-C and Unix SIGTERM drain active requests. All graph data is lost when the
process exits; restarting with another engine does not migrate existing data.

```sh
curl -sS http://127.0.0.1:8080/v1/operations \
  -H 'Content-Type: application/json' \
  -d '{"op":"link","source":"1","target":"2"}'

curl -sS http://127.0.0.1:8080/v1/operations \
  -H 'Content-Type: application/json' \
  -d '{"op":"connected","source":"1","target":"2"}'
```

IDs are decimal strings over HTTP, preserving the full `u64` range. Links create
absent endpoints. Repeated links and cuts are idempotent; cuts retain vertices.
Self-loops are rejected. Queries involving unknown vertices return an error.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

Public items require English Rustdoc. Nontrivial private algorithms also require
documentation of invariants, correctness and complexity. Workspace lints enforce
missing docs and forbid unsafe code; each crate inherits them explicitly.
CI runs the quality gates on Linux and macOS.

## Documentation and measurements

- [HTTP API](docs/http-api.md): operations, batch semantics, limits and errors.
- [Architecture](docs/architecture/0001-foundation.md): contract, costs and scope.
- [Contributing](CONTRIBUTING.md): quality and documentation requirements.
- [knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks): reproducible
  local workloads and future comparisons. Keep its clone beside this repository.

Documentation lives here alongside the code. Authentication, persistence,
multiple graph namespaces and distributed execution are not implemented.

An opt-in experimental `ForestGraph` maintains exact connectivity using AVL
Euler-tour trees and smaller-side replacement scans. The default `Graph` and HTTP
service use compact BFS; `KNOTREL_ENGINE=ett` opts the server into `ForestGraph`. See [design and limits](docs/architecture/0005-experimental-euler-tour-forest.md).

The experimental forest skips candidate-free subtrees and stops replacement
searches lazily. See [candidate pruning and measured tradeoffs](docs/architecture/0006-candidate-pruning.md).


Embedded callers can choose an engine through `ConnectivityGraph::new(EngineConfig)`
without environment or HTTP dependencies. Existing `Graph`, `ForestGraph` and
`knotrel_server::router(Graph)` APIs remain available. See
[startup configuration design](docs/architecture/0007-startup-engine-configuration.md).


Experimental `HdtGraph` adds deterministic edge levels and promotions to avoid
repeated failed replacement work. Select it with `KNOTREL_ENGINE=hdt` or
`ConnectivityGraph::new(EngineConfig::Hdt)`. It uses O(E + V log V) logical memory;
update bounds are amortized, not per-request latency guarantees. Compact BFS
remains the default. See [HDT design and limits](docs/architecture/0008-experimental-hdt.md).

## Graph cardinality limits

Set `KNOTREL_MAX_NODES` and/or `KNOTREL_MAX_EDGES` before starting the server:

```sh
KNOTREL_MAX_NODES=100000 KNOTREL_MAX_EDGES=200000 cargo run --locked -p knotrel-server
```

Absent variables mean unlimited for backward-compatible startup. Zero forbids
new entries; values must be ASCII decimal integers fitting `usize`. Limits are
fixed for each graph instance and reported by `/v1/info` (`null` for unlimited).
A rejected operation changes nothing, including implicitly created endpoints.
Duplicate vertices/edges remain successful no-ops. Cuts free edge capacity but
retain vertices and their capacity usage. These are not RAM limits: allocation
capacities can remain high after cuts.

Embedded callers use `ConnectivityGraph::with_limits(engine, GraphLimits {
max_nodes: Some(100000), max_edges: Some(200000) })`. The raw `Graph`,
`ForestGraph` and `HdtGraph` algorithm types remain unlimited; use the wrapper
for enforced limits. `ConnectivityGraph::add_node` now returns
`Result<bool, GraphError>` so callers must handle capacity errors. Server
embedders can use `ServerConfig::with_graph_limits`; the legacy `router(Graph)`
retains unlimited cardinality. See [ADR 0009](docs/architecture/0009-graph-limits.md).

Rust callers matching `GraphError` exhaustively must also handle the new
`NodeLimitExceeded` and `EdgeLimitExceeded` variants.
