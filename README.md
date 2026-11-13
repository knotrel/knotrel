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

Knotrel is an in-memory engine for **exact connectivity in undirected graphs
whose edges change over time**. Use it as a Rust library (`knotrel-core`) or as
an HTTP service (`knotrel-server`).

**Example:** a network of devices and links. Links are added when cables, radio
paths or tunnels come up and removed when they fail. The application repeatedly
asks: *are device A and device B still connected, through any path?* Knotrel
keeps the graph current and answers each question exactly, without
approximation or cached staleness.

**Status:** v0.2.0 is the latest published preview. The `main` branch carries
the v0.3.0 release candidate, which is not published until it appears on the
[GitHub Releases page](https://github.com/knotrel/knotrel/releases).

## When Knotrel helps

- Edges are both **inserted and deleted**, and connectivity is queried
  repeatedly between changes.
- You need exact yes/no answers for arbitrary pairs, not a periodic batch
  recomputation.
- You want an embeddable synchronous Rust library or a small local HTTP
  service with bounded limits.

### When something simpler may suffice

- **Insertion-only graphs:** a union-find (disjoint-set) structure answers
  connectivity without supporting deletions.
- **Infrequent queries:** a breadth-first or depth-first search over your
  own adjacency data, run on demand, may be enough.
- **Durable or distributed graph storage:** Knotrel is not a database; see the
  integration model below.

## Quickstart

### HTTP service

From a clone of this repository:

```sh
cargo run --release --locked -p knotrel-server
```

The server listens on `127.0.0.1:8080`. In another terminal, connect three
devices, ask a question, remove a link and ask again:

```sh
op() {
  curl -sS http://127.0.0.1:8080/v1/operations \
    -H 'Content-Type: application/json' -d "$1"; echo
}
op '{"op":"link","source":"1","target":"2"}'       # {"result":"changed","changed":true}
op '{"op":"link","source":"2","target":"3"}'
op '{"op":"connected","source":"1","target":"3"}'  # {"result":"connected","connected":true}
op '{"op":"cut","source":"2","target":"3"}'
op '{"op":"connected","source":"1","target":"3"}'  # {"result":"connected","connected":false}
```

IDs are decimal strings over HTTP, preserving the full `u64` range. Links create
absent endpoints. Repeated links and cuts are idempotent; cuts retain vertices.
Self-loops are rejected. Queries involving unknown vertices return an error.
See the [HTTP API](docs/http-api.md) for batches, errors and limits.

For a guided scenario, the [power-grid tutorial](docs/tutorials/power-grid.md)
opens and closes connections in a small synthetic distribution network and
checks which customers keep a path to the source.

### Rust library

`knotrel-core` is synchronous and has no HTTP or async dependencies. The latest
published version is 0.2.0; use `"0.3.0"` only after v0.3.0 is published.

```toml
[dependencies]
knotrel-core = "0.2.0"
```

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

## Integration model

- **Your database or event source owns the authoritative graph.** Knotrel holds
  derived, reconstructible state in memory; it has no durable graph
  persistence.
- **Restart means rebuild.** All graph data is lost when the process exits;
  restarting with another engine does not migrate data. In source-managed mode
  (`KNOTREL_MODE=source_managed`) an external feeder publishes a snapshot and
  then ordered events, and readiness reports `uninitialized` until it does.
- **The reference feeder is an example, not a production connector.**
  [`external_feeder`](crates/knotrel-server/examples/external_feeder.rs) and the
  [example container](examples/container/) demonstrate the protocol; adapting it
  to your source, retention and ownership rules is your responsibility.

See the [integration contract](docs/integration-contract.md) and the
[deployment runbook](docs/deployment.md) for sessions, leases, readiness states
and the restart/rebuild procedure.

> **Security boundary:** Knotrel has **no built-in authentication, authorization
> or TLS**. Anyone who can reach the listener can read and mutate the graph. It
> listens on loopback by default and refuses a non-loopback address unless
> `KNOTREL_ALLOW_NON_LOOPBACK=true`. Run it inside a trusted deployment boundary,
> or behind an external gateway that you configure separately; no gateway is
> included or configured by Knotrel. A private network alone is not sufficient;
> see the [trust model](docs/http-api.md#trust-model).

## Choosing an engine

Select an engine with `KNOTREL_ENGINE` (server) or
[`EngineConfig`](crates/knotrel-core/src/configuration.rs) (library). No engine
is fastest for every topology and operation mix; measure your own workload.

| Engine | Status | Costs (logical) | Consider it for | Design |
| --- | --- | --- | --- | --- |
| `compact-bfs` / `EngineConfig::CompactBfs` | **Default** | Queries $O(V + E)$ worst case; link/cut $O(\deg u + \deg v)$ amortized; memory $O(V + E)$ | General and mutation-heavy workloads; simplest, lowest memory | [ADR 0004](docs/architecture/0004-compact-core.md) |
| `ett` / `EngineConfig::EulerTour` | Experimental, opt-in | Queries $O(\log V)$; tree cuts may scan non-tree edges of the smaller side | Query-heavy workloads on tree-like topologies | [ADR 0005](docs/architecture/0005-experimental-euler-tour-forest.md), [0006](docs/architecture/0006-candidate-pruning.md) |
| `hdt` / `EngineConfig::Hdt` | Experimental, opt-in | Queries $O(\log V)$; updates $O(\log^2 V)$ amortized; memory $O(E + V \log V)$ | Dense graphs with frequent bridge cuts | [ADR 0008](docs/architecture/0008-experimental-hdt.md) |

Library callers can also reuse traversal scratch space with the opt-in
`BfsWorkspace`, and compare against the original ordered-adjacency
`ReferenceGraph`. Bounds describe internal structures: external `u64` IDs use
hash maps with expected, not worst-case, $O(1)$ lookups, and logical memory does
not bound heap or RSS because capacity is retained after cuts. Amortized bounds
are not per-request latency guarantees. Only `compact-bfs` and its workspace
variant were measured in v0.3.0 release qualification. See
[startup engine configuration](docs/architecture/0007-startup-engine-configuration.md).

## Performance evidence

**[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks)** holds
the reproducible workloads, methodology and published reports: release
regression qualification, engine/competitor comparisons and algorithm
experiments, each labelled with the version and host it actually measured.
For the v0.3.0 candidate, the release qualification outcome and its
limitations are summarized in the
[v0.3.0 release notes](docs/releases/v0.3.0-release-notes.md#performance-qualification);
no speedup is claimed.

## Installing a release

Download the archive and checksum for your platform from the
[GitHub Releases page](https://github.com/knotrel/knotrel/releases) and verify
the SHA-256 checksum before running. The commands below use v0.3.0 file names
and apply only after v0.3.0 is published; substitute `v0.2.0` for the latest
published preview.

```sh
# Verify checksum on Linux:
sha256sum -c knotrel-server-v0.3.0-x86_64-unknown-linux-gnu.tar.gz.sha256
# Or on macOS:
# shasum -a 256 -c knotrel-server-v0.3.0-aarch64-apple-darwin.tar.gz.sha256

# Extract and start on Linux (x86_64):
tar -xzf knotrel-server-v0.3.0-x86_64-unknown-linux-gnu.tar.gz
cd knotrel-server-v0.3.0-x86_64-unknown-linux-gnu
./knotrel-server

# Or on macOS (Apple Silicon):
# tar -xzf knotrel-server-v0.3.0-aarch64-apple-darwin.tar.gz
# cd knotrel-server-v0.3.0-aarch64-apple-darwin
# ./knotrel-server
```

On Windows (PowerShell):
```powershell
# Compare the displayed hash with the downloaded .sha256 file before extracting.
Get-FileHash -Algorithm SHA256 knotrel-server-v0.3.0-x86_64-pc-windows-msvc.zip
Get-Content knotrel-server-v0.3.0-x86_64-pc-windows-msvc.zip.sha256
Expand-Archive knotrel-server-v0.3.0-x86_64-pc-windows-msvc.zip -DestinationPath .
cd knotrel-server-v0.3.0-x86_64-pc-windows-msvc
.\knotrel-server.exe
```

## Configuration and operation

Configuration is read once at startup from environment variables; it cannot be
changed through requests. Invalid values and unknown `KNOTREL_` variables stop
startup.

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_ADDR` | `127.0.0.1:8080` | IP address and port; non-loopback needs `KNOTREL_ALLOW_NON_LOOPBACK=true` |
| `KNOTREL_ALLOW_NON_LOOPBACK` | `false` | `true` permits a non-loopback `KNOTREL_ADDR` (no auth/TLS; see [trust model](docs/http-api.md#trust-model)) |
| `KNOTREL_MODE` | `standalone` | `standalone` or `source_managed`; the latter requires an external feeder |
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

Further timeouts and size limits are listed under
[startup configuration](docs/http-api.md#startup-configuration-and-engine-selection).

- **Probes:** `GET /health` is liveness only. `GET /ready` reports `ready`, or
  503 `starting`, `draining` or `unavailable`; in source-managed mode also
  `uninitialized`, `loading`, `catching_up`, `stale` or `desynchronized`.
- **Shutdown:** on Unix, SIGINT or SIGTERM makes the server unready, rejects new
  graph work with 503 `shutting_down`, lets admitted requests finish and exits
  0; it exits 2 if the drain deadline expires, and a second signal forces exit
  130. On Windows, shutdown uses Ctrl+C or platform termination. See
  [lifecycle details](docs/http-api.md#lifecycle-health-and-shutdown).
- **Cardinality limits:** a rejected operation changes nothing, including
  implicitly created endpoints. Duplicate vertices and edges remain successful
  no-ops; cuts free edge capacity but retain vertices. Limits are not RAM
  limits. Embedded callers use `ConnectivityGraph::with_limits`; see
  [ADR 0009](docs/architecture/0009-graph-limits.md) for embedder and migration
  notes.
- **Inspection:** `GET /v1/info` reports the fixed engine and limits
  without entering the graph queue. `GET /v1/stats` returns a coherent snapshot
  of counts, remaining capacity and a decimal-string `state_version` that
  advances once per effective operation and resets on restart; it can return
  503 `busy`. See the
  [snapshot contract](docs/http-api.md#live-graph-snapshot-versus-instance-information).

## Documentation

- [HTTP API](docs/http-api.md) and [OpenAPI specification](docs/openapi.yaml): operations, batch semantics, limits and errors.
- [Power-grid tutorial](docs/tutorials/power-grid.md): runnable topology demonstration of connectivity loss and restoration over the HTTP API.
- [Integration contract](docs/integration-contract.md): source-managed mode, feeder sessions, imports, events and readiness.
- [Deployment runbook](docs/deployment.md): example container, reference feeder, restart and rebuild.
- [Architecture decisions](docs/architecture/0001-foundation.md): contracts, costs and scope, starting with ADR 0001.
- [v0.3.0 release notes](docs/releases/v0.3.0-release-notes.md): candidate scope, qualification outcomes, platform coverage and limitations.
- [v0.2.0](docs/releases/v0.2.0-release-notes.md) and [v0.1.0](docs/releases/v0.1.0-release-notes.md) release notes, and the [v0.1.0 scope](docs/releases/v0.1.0-scope.md).
- [Release procedure](docs/releases/release-procedure.md): verification, packaging and authorized publication.
- [Contributing](CONTRIBUTING.md): quality and documentation requirements.
- [knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks): workloads, methodology and reports. Keep its clone beside this repository to run it locally.

Authentication, persistence, multiple graph namespaces and distributed execution
are not implemented.

## Development and quality gates

| Crate | Responsibility |
| --- | --- |
| `knotrel-core` | Synchronous graph operations and exact connectivity |
| `knotrel-server` | Axum HTTP API, validation and bounded blocking graph work |

Rust **1.100.0**, edition **2024**, and Cargo resolver **3** are pinned for the
workspace and server. The `knotrel-core` minimum supported Rust version (MSRV)
is **1.98.1**. The toolchain file installs Clippy and rustfmt when using rustup.
Cargo.lock records the resolved dependency versions.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --features knotrel-server/test-support -- -D warnings
cargo test --workspace --locked
cargo test --workspace --locked --features knotrel-server/test-support
cargo +1.98.1 test --locked -p knotrel-core
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
# Run in a disposable checkout: this script removes target/package.
./scripts/verify-source-packages.sh
./scripts/smoke-test-release-binary.sh
```

Public items require English Rustdoc. Nontrivial private algorithms also require
documentation of invariants, correctness and complexity. Workspace lints enforce
missing docs and forbid unsafe code; each crate inherits them explicitly.
CI builds and tests on Linux, Windows and macOS; formatting, Clippy and Rustdoc
gates run on Linux.
