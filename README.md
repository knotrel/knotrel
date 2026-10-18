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
| `knotrel-core` | Synchronous graph operations and exact connectivity |
| `knotrel-server` | Axum HTTP API, validation and bounded blocking graph work |

Rust **1.98.1**, edition **2024**, and Cargo resolver **3** are pinned. The
toolchain file installs Clippy and rustfmt when using rustup. Cargo.lock records
the resolved dependency versions. Packages are currently unpublished.

## Installation and Running

### From a prebuilt release binary

Download the archive for your platform from the GitHub Releases page, verify its SHA-256 checksum, and run:

```sh
# Verify checksum on Linux:
sha256sum -c knotrel-server-v0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
# Or on macOS:
# shasum -a 256 -c knotrel-server-v0.1.0-aarch64-apple-darwin.tar.gz.sha256

# Extract and start on Linux (x86_64):
tar -xzf knotrel-server-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
cd knotrel-server-v0.1.0-x86_64-unknown-linux-gnu
./knotrel-server

# Or on macOS (Apple Silicon):
# tar -xzf knotrel-server-v0.1.0-aarch64-apple-darwin.tar.gz
# cd knotrel-server-v0.1.0-aarch64-apple-darwin
# ./knotrel-server
```

On Windows (PowerShell):
```powershell
Expand-Archive knotrel-server-v0.1.0-x86_64-pc-windows-msvc.zip
cd knotrel-server-v0.1.0-x86_64-pc-windows-msvc
.\knotrel-server.exe
```

### From source

```sh
cargo run --release --locked -p knotrel-server
```

The server listens on `127.0.0.1:8080` and uses compact BFS by default.
Configuration is read once before binding:

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_ADDR` | `127.0.0.1:8080` | IP address and port; non-loopback needs `KNOTREL_ALLOW_NON_LOOPBACK=true` |
| `KNOTREL_ALLOW_NON_LOOPBACK` | `false` | `true` permits a non-loopback `KNOTREL_ADDR` (no auth/TLS; see [trust model](docs/http-api.md#trust-model)) |
| `KNOTREL_ENGINE` | `compact-bfs` | `compact-bfs`, experimental `ett` or `hdt` |
| `KNOTREL_MAX_PENDING_JOBS` | `32` | Positive limit on queued plus running graph jobs |
| `KNOTREL_MAX_NODES` | `100000` | Maximum registered nodes; zero allowed; `unlimited` opts out |
| `KNOTREL_MAX_EDGES` | `500000` | Maximum live edges; zero allowed; `unlimited` opts out |
| `RUST_LOG` | `info` | Logging filter |

For example, to opt into the experimental forest:

```sh
KNOTREL_ENGINE=ett KNOTREL_MAX_PENDING_JOBS=16 \
  cargo run --release --locked -p knotrel-server
curl -sS http://127.0.0.1:8080/v1/info
```

> **Security:** v0.1.0 has no authentication, authorization or TLS, and no
> protection for exposure to untrusted clients or the public internet. The server
> listens on loopback by default and refuses a non-loopback `KNOTREL_ADDR` unless
> `KNOTREL_ALLOW_NON_LOOPBACK=true` is set. A private network alone is not
> sufficient; see the [trust model](docs/http-api.md#trust-model).

Invalid values and unknown `KNOTREL_` variables stop startup. `hdt` selects the
experimental level-based HDT engine. `GET /v1/info` reports the effective engine and limits without
locking the graph. Settings cannot be changed through operation requests.
See [configuration and API details](docs/http-api.md#startup-configuration-and-engine-selection).

`GET /ready` reports readiness (`ready`, or 503 `starting`/`draining`/`unavailable`);
`GET /health` is liveness only.
On Unix (Linux/macOS), graceful shutdown is initiated by SIGINT or SIGTERM: the server turns
unready, rejects new graph work with 503 `shutting_down`, lets admitted requests
finish, and exits 0. If the drain deadline expires before all work finishes, the server
exits 2; a second signal forces immediate exit (130). On Windows, Unix signals are not
supported by the OS; process shutdown is initiated via Ctrl+C or platform termination.
Header-read, request and connection limits are configurable; see
[lifecycle details](docs/http-api.md#lifecycle-health-and-shutdown). All graph
data is lost when the process exits or restarts; restarting with another engine
does not migrate existing data.

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

## Development and Quality Gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --locked --features knotrel-server/test-support
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
./scripts/verify-source-packages.sh
./scripts/smoke-test-release-binary.sh
```

Public items require English Rustdoc. Nontrivial private algorithms also require
documentation of invariants, correctness and complexity. Workspace lints enforce
missing docs and forbid unsafe code; each crate inherits them explicitly.
CI builds and tests on Linux, Windows and macOS; formatting, Clippy and Rustdoc
gates run on Linux.

## Documentation and measurements

- [v0.1.0 release notes](docs/releases/v0.1.0-release-notes.md): preview highlights, support matrix, checksum verification, and known limitations.
- [Release procedure](docs/releases/release-procedure.md): step-by-step verification, artifact packaging, and authorized publication protocol.
- [v0.1.0 release scope](docs/releases/v0.1.0-scope.md): supported preview surface,
  verified runtime matrix, release blockers and performance-validation plan.
- [HTTP API](docs/http-api.md) ([OpenAPI specification](docs/openapi.yaml)): operations, batch semantics, limits, errors and standard OpenAPI definition.
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

For ordered bulk initialization, use
`ConnectivityGraph::try_from_edges(engine, limits, nodes, edges)` with borrowed
node and edge slices. It registers nodes first, then links edges in input order
through the existing limit-enforcing operations. Duplicate vertices and
duplicate/reversed edges are no-ops; missing endpoints are created implicitly.
The first incremental error is returned and no partial graph is published.
Successful graphs retain their limits and support normal dynamic operations.
This convenience API does not reserve or reorder input, build forests directly,
restore snapshots, provide recoverable allocation failure, or change HTTP batch
semantics. No speedup has been established; its work is the ordered incremental
construction work, with constant auxiliary space beyond the graph.


Experimental `HdtGraph` adds deterministic edge levels and promotions to avoid
repeated failed replacement work. Select it with `KNOTREL_ENGINE=hdt` or
`ConnectivityGraph::new(EngineConfig::Hdt)`. It uses O(E + V log V) logical memory;
update bounds are amortized, not per-request latency guarantees. Compact BFS
remains the default. See [HDT design and limits](docs/architecture/0008-experimental-hdt.md).

## Using `knotrel-core` as a Rust Library

`knotrel-core` provides synchronous, exact dynamic connectivity without HTTP or async dependencies.

Because packages in this repository are currently in preview development (`publish = false`),
integrate `knotrel-core` via path or git:

```toml
[dependencies]
# Local path dependency:
knotrel-core = { path = "path/to/knotrel/crates/knotrel-core" }
# Or via git repository:
# knotrel-core = { git = "https://github.com/knotrel/knotrel.git" }
# Future registry dependency (once published):
# knotrel-core = "0.1.0"
```

### Quick start

```rust
use knotrel_core::{ConnectivityGraph, EngineConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut graph = ConnectivityGraph::new(EngineConfig::CompactBfs);

    graph.add_node(1)?;
    graph.add_node(2)?;
    graph.link(1, 2)?;

    assert!(graph.connected(1, 2)?);

    graph.cut(1, 2)?;
    assert!(!graph.connected(1, 2)?);

    Ok(())
}
```

See runnable Cargo examples in [`crates/knotrel-core/examples`](crates/knotrel-core/examples):
- `cargo run -p knotrel-core --example basic`
- `cargo run -p knotrel-core --example bulk_import`
- `cargo run -p knotrel-core --example engine_selection`

## Engine Selection and Guidance

Knotrel provides three dynamic connectivity engines via [`EngineConfig`](crates/knotrel-core/src/configuration.rs):

### 1. Compact BFS (`EngineConfig::CompactBfs`) — Default
- **Algorithmic properties**: Sorted adjacency vectors with stable internal indices; $O(V + E)$ worst-case BFS queries; $O(\text{deg}(u) + \text{deg}(v))$ amortized link and cut; $O(V + E)$ logical memory. Adding vertices can trigger an $O(V)$ outer-vector reallocation.
- **Workload expectations**: Contiguous memory layout and low allocation overhead. Best suited for general workloads or mutation-heavy workloads where mutations dominate queries.
- **Limitations**: Query latency scales with component size in the worst case.

### 2. Euler Tour Trees (`EngineConfig::EulerTour`) — Experimental
- **Algorithmic properties**: Single spanning forest using AVL-balanced Euler tour sequences. $O(\log V)$ algorithmic queries by comparing tour roots. $O(\log V + \log E)$ amortized link and non-tree cut. Tree cuts partition the tree and scan non-tree edges of the smaller component ($O(\log V + \log E + s + c \log V)$ amortized) using candidate-pruned subtrees.
- **Workload expectations**: Effective for query-dominated workloads and tree-like topologies where replacement searches are rare.
- **Limitations**: In dense cyclic subgraphs with frequent bridge cuts, lacking an edge-level hierarchy can cause repeated scans over the same non-tree edges.

### 3. Holm–de Lichtenberg–Thorup (`EngineConfig::Hdt`) — Experimental
- **Algorithmic properties**: Deterministic implementation of Holm, de Lichtenberg, and Thorup (2001) with an append-only growth policy for expanding vertex universes. Maintains a hierarchy of $O(\log V)$ levels materialized lazily as promotions require them. Promotes examined tree and non-tree edges to upper levels during downward search, bounding amortized updates to $O(\log^2 V)$; queries take $O(\log V)$; logical memory is $O(E + V \log V)$.
- **Workload expectations**: Avoids repeated scan storms in dense graphs undergoing frequent bridge cuts.
- **Limitations**: Higher structural complexity and memory footprint can mean higher constant factors on small or sparse graphs; verify against your actual workload. Amortized bounds represent sequence averages rather than worst-case per-request latency bounds.

### Complexity & Memory Qualifications
- **Algorithmic vs. API costs**: Algorithmic complexity bounds apply to internal index structures. The public API maps external `NodeId` (`u64`) keys via hash maps (`IntMap` / `FxHashMap`), which rely on expected $O(1)$ lookups and do not offer deterministic worst-case constant bounds for arbitrary ID distributions.
- **Logical memory vs. heap/RSS**: Stated memory bounds ($O(V + E)$ and $O(E + V \log V)$) describe logical graph entity storage. Allocated vector capacities are retained after cuts and do not bound process heap memory or RSS.

### Empirical findings and recommendations
- **No universal winner**: Benchmark comparisons show that no single engine dominates across all topologies and operation mixes.
- **Uncertain workloads**: If the benchmark repository does not test your specific graph scale and topology, do not assume polylogarithmic engines are faster.
- **Pragmatic advice**: Start with **Compact BFS** (`EngineConfig::CompactBfs`) as the simple, robust, low-memory default. Consider **ETT** for query-dominated workloads on trees/forests, or **HDT** for dense graphs with frequent bridge cuts, empirically measuring actual topology, scale, and mutation mix.

## Graph cardinality limits

The standalone server defaults to 100 000 nodes and 500 000 edges. Override them
with `KNOTREL_MAX_NODES` and/or `KNOTREL_MAX_EDGES` before starting the server:

```sh
KNOTREL_MAX_NODES=100000 KNOTREL_MAX_EDGES=200000 cargo run --locked -p knotrel-server
```

Absent variables use the defaults; the literal `unlimited` removes a ceiling.
Zero forbids new entries; other values must be ASCII decimal integers fitting
`usize`. Limits are fixed for each graph instance and reported by `/v1/info`
(`null` for unlimited).
A rejected operation changes nothing, including implicitly created endpoints.
Duplicate vertices/edges remain successful no-ops. Cuts free edge capacity but
retain vertices and their capacity usage. These are not RAM limits: allocation
capacities can remain high after cuts.

Embedded callers use `ConnectivityGraph::with_limits(engine, GraphLimits {
max_nodes: Some(100000), max_edges: Some(200000) })`. The raw `Graph`,
`ForestGraph` and `HdtGraph` algorithm types remain unlimited; use the wrapper
for enforced limits. `ConnectivityGraph::add_node` now returns
`Result<bool, GraphError>` so callers must handle capacity errors. Server
embedders can use `ServerConfig::with_graph_limits`; `ServerConfig::new` applies
the defaults, while the legacy `router(Graph)` retains unlimited cardinality. See [ADR 0009](docs/architecture/0009-graph-limits.md).

Rust callers matching `GraphError` exhaustively must also handle the new
`NodeLimitExceeded` and `EdgeLimitExceeded` variants.

## Inspecting the running instance

`GET /v1/info` reports fixed engine/configuration settings without entering the
graph queue. `GET /v1/stats` reports a coherent snapshot of node/edge counts,
limits, remaining capacity and a decimal-string `state_version`. The version
starts at zero for each instance and advances once per effective operation,
including each effective operation in a batch. Errors and no-ops do not advance
it; a restart resets it. Stats shares the graph job limit and can return 503
`busy`, while info remains available. Stats responses disable HTTP storage via
`Cache-Control: no-store`.

```sh
curl -sS http://127.0.0.1:8080/v1/stats
```

See the [snapshot contract](docs/http-api.md#live-graph-snapshot-versus-instance-information)
for batch isolation, unlimited capacities, precision and instance scope.
