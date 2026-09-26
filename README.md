# Knotrel

Dynamic connectivity for undirected graphs: insert edges, delete edges, and ask
whether two vertices are connected.

This repository is an initial, in-memory reference implementation. Connectivity
queries use breadth-first traversal; no optimized dynamic-connectivity or
production scalability claims have been established yet.

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

The server listens on `127.0.0.1:8080`. Set `KNOTREL_ADDR` to an IP address and
port, and `RUST_LOG` to configure logging. Ctrl-C and Unix SIGTERM drain active
requests. All graph data is lost when the process exits.

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
multiple graph namespaces, distributed execution and an optimized connectivity
engine are outside this initial foundation.
