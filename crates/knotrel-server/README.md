# knotrel-server

HTTP API for the Knotrel dynamic connectivity engine.

[![License: ELv2](https://img.shields.io/badge/License-Elastic%202.0-blue.svg)](LICENSE.md)

`knotrel-server` provides an Axum-based HTTP transport and standalone executable for
the Knotrel in-memory dynamic graph connectivity engine ([`knotrel-core`](https://crates.io/crates/knotrel-core)).

## Features

- **Dynamic graph mutations**: Insert (`link`) and delete (`cut`) edges dynamically.
- **Exact reachability queries**: Query whether two vertices are connected (`connected`).
- **Batch execution**: Isolated batch processing via `/v1/batch` (1–1024 operations). Batch operations execute sequentially in input order under an exclusive graph lock without concurrent interleaving. If an operation fails, previously completed operations are retained in graph state (no transactional rollback is provided).
- **Engine selection**: Built on `knotrel-core`, configurable with Compact BFS (default), experimental Euler Tour Trees (`ett`), or experimental Holm–de Lichtenberg–Thorup (`hdt`).
- **Telemetry and lifecycle**: Clean liveness (`/health`), readiness (`/ready`), instance configuration (`/v1/info`), and live graph snapshot stats (`/v1/stats`).
- **Safe defaults**: Loopback-only binding by default, bounded request timeout, configurable queue limits, and graceful drain.

## Installation and Usage

### Standalone executable

#### 1. From local source checkout

```sh
cargo run --release --locked -p knotrel-server
```

#### 2. From crates.io (available after release publication)

```sh
cargo install knotrel-server --version 0.1.0 --locked
knotrel-server
```

By default, the server binds to `127.0.0.1:8080`.

#### Environment Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_ADDR` | `127.0.0.1:8080` | IP address and port to bind to |
| `KNOTREL_ALLOW_NON_LOOPBACK` | `false` | Set to `true` to permit non-loopback bind |
| `KNOTREL_ENGINE` | `compact-bfs` | Dynamic connectivity engine (`compact-bfs`, `ett`, or `hdt`) |
| `KNOTREL_MAX_PENDING_JOBS` | `32` | Maximum queued and concurrent graph operations |
| `KNOTREL_MAX_NODES` | `100000` | Maximum registered node count (`unlimited` to remove ceiling) |
| `KNOTREL_MAX_EDGES` | `500000` | Maximum active edge count (`unlimited` to remove ceiling) |

### Library Usage

You can also embed `knotrel-server`'s Axum router into your own application:

```rust
use knotrel_core::{ConnectivityGraph, EngineConfig};
use knotrel_server::{ServerConfig, router_with_config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig::default();
    let app = router_with_config(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

## Security Notice

Knotrel v0.1.0 does not include authentication, authorization, or TLS. The service is intended
for trusted internal networks or local process communication. The standalone server enforces
loopback-only binding (`127.0.0.1`) by default and will refuse to start on non-loopback addresses
unless `KNOTREL_ALLOW_NON_LOOPBACK=true` is explicitly configured.

## License

Licensed under the [Elastic License 2.0 (ELv2)](LICENSE.md).
