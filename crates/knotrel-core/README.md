# knotrel-core

Exact, in-memory dynamic connectivity for undirected simple graphs.

[![License: ELv2](https://img.shields.io/badge/License-Elastic%202.0-blue.svg)](LICENSE.md)

`knotrel-core` is a specialized, synchronous Rust library that maintains exact reachability
queries ("is vertex $u$ connected to vertex $v$ via some path?") as edges are dynamically
inserted (`link`) and deleted (`cut`) over time.

## Quick Start

Add `knotrel-core` to your `Cargo.toml`:

```toml
[dependencies]
knotrel-core = "0.1.0"
```

Use `ConnectivityGraph` to create and query a dynamic graph:

```rust
use knotrel_core::{ConnectivityGraph, EngineConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize a graph with the default compact BFS engine.
    let mut graph = ConnectivityGraph::new(EngineConfig::CompactBfs);

    // Register nodes and link edges.
    graph.add_node(1)?;
    graph.add_node(2)?;
    graph.link(1, 2)?;
    graph.link(2, 3)?;

    // Exact reachability queries.
    assert!(graph.connected(1, 3)?);

    // Dynamic bridge cut.
    assert!(graph.cut(2, 3)?);
    assert!(!graph.connected(1, 3)?);

    // Existing nodes remain registered after cuts.
    assert_eq!(graph.node_count(), 3);

    Ok(())
}
```

## Graph Model

- **Undirected simple graphs**: No directed edges, self-loops, or multi-edges.
- **Node identifiers**: Vertices are identified by `NodeId` (`u64`).
- **Dynamic registration**: `add_node` adds isolated vertices; `link` implicitly creates endpoints.
- **Cuts retain vertices**: `cut` removes an edge while preserving its endpoint nodes.
- **Atomic validation**: Invalid operations (e.g. self-loops, capacity violations) leave state completely unchanged.

## Bulk Initialization

Use `ConnectivityGraph::try_from_edges` to initialize a graph from existing slices of nodes and edges:

```rust
use knotrel_core::{ConnectivityGraph, EngineConfig, GraphLimits};

let nodes = [99]; // isolated standby node
let edges = [(1, 2), (2, 3), (3, 4)];

let mut graph = ConnectivityGraph::try_from_edges(
    EngineConfig::CompactBfs,
    GraphLimits::default(),
    &nodes,
    &edges,
)?;

assert!(graph.connected(1, 4)?);
assert!(!graph.connected(1, 99)?);
# Ok::<(), knotrel_core::GraphError>(())
```

## Engine Selection

Knotrel provides three dynamic connectivity engines via `EngineConfig`:

- **Compact BFS (`EngineConfig::CompactBfs`) — Default**: Sorted adjacency vectors of stable internal indices; $O(V + E)$ worst-case BFS queries; $O(\text{deg}(u) + \text{deg}(v))$ amortized link and cut; $O(V + E)$ logical memory. Best for general use or mutation-heavy workloads.
- **Euler Tour Trees (`EngineConfig::EulerTour`) — Experimental**: Spanning forest using AVL Euler tour sequences. $O(\log V)$ algorithmic queries and link operations. Non-tree cuts cost $O(\log V + \log E)$; tree cuts scan the smaller side ($O(\log V + \log E + s + c \log V)$ amortized) using candidate-pruned subtrees. Effective for query-dominated workloads on tree-like topologies.
- **HDT (`EngineConfig::Hdt`) — Experimental**: Deterministic Holm–de Lichtenberg–Thorup dynamic connectivity with an append-only growth policy for expanding vertices. Maintains a hierarchy of $O(\log V)$ levels materialized lazily as promotions require them. Bounds amortized update costs to $O(\log^2 V)$, eliminating repeated scan storms in dense bridge-churn workloads.

### Complexity & Memory Notes

- **Algorithmic vs. API costs**: Algorithmic complexity bounds apply to internal index structures. The public API maps external `NodeId` (`u64`) keys via hash maps (`IntMap` / `FxHashMap`), which rely on expected $O(1)$ lookups and do not offer deterministic worst-case constant bounds for arbitrary ID distributions.
- **Logical memory vs. heap/RSS**: Stated memory bounds ($O(V + E)$ and $O(E + V \log V)$) describe logical graph entity storage. Allocated vector capacities are retained after cuts and do not bound process heap memory or RSS.
- **Amortized bounds**: HDT update bounds are amortized sequence averages, not per-request latency guarantees; large promotion searches or vector growths can produce latency spikes, and it may have higher constant factors on small or sparse graphs; verify against your actual workload.

### Recommendation

Start with **Compact BFS** (`EngineConfig::CompactBfs`); evaluate **ETT** (`EngineConfig::EulerTour`) for query-dominated workloads or **HDT** (`EngineConfig::Hdt`) for workloads with repeated bridge replacement searches, empirically measuring actual topology, scale, and mutation mix.

## Examples

Run the included Cargo examples:

```sh
# Basic dynamic connectivity
cargo run -p knotrel-core --example basic

# Bulk initialization and error handling
cargo run -p knotrel-core --example bulk_import

# Engine selection and equivalence checks
cargo run -p knotrel-core --example engine_selection
```

## License

Licensed under the [Elastic License 2.0 (ELv2)](LICENSE.md).
