---
layout: default
title: Choosing an engine
---
# Choosing an engine

Knotrel answers the same exact connectivity questions with four engines. They
differ only in speed, memory and the operations they make cheap. This page is
the canonical guide for choosing one. Keep the default, `compact-bfs`, unless
measurements on your own workload show a reason to change.

> **Which version you need.** All four engines are in the latest release,
> [v1.1.0](https://github.com/knotrel/knotrel/releases/tag/v1.1.0).
> **`cluster-forest` is new in 1.1.0** (see [Getting 1.1.0](#getting-110)).
> The v1.0.0 server refuses `KNOTREL_ENGINE=cluster-forest` at startup ("must
> be compact-bfs, ett or hdt"), and `knotrel-core` 1.0 has no
> `EngineConfig::ClusterForest`.

## At a glance

| Engine | Status | In v1.0.0 | Lifecycle | Consider it for |
| --- | --- | --- | --- | --- |
| `compact-bfs` | **Default**, stable | Yes | Yes | General and update-heavy workloads; lowest memory |
| `ett` | Experimental engine | Yes | No | Query-heavy work on tree-like graphs |
| `hdt` | Experimental engine | Yes | No | Dense graphs with frequent bridge cuts |
| `cluster-forest` | Experimental engine | **No: new in 1.1.0** | **Yes** | Query-heavy work where `compact-bfs` searches; dense bridge churn; see [when to try it](#when-to-try-cluster-forest) |

How to select each engine:

| Engine | Library (`knotrel-core`) | Server (`knotrel-server`) |
| --- | --- | --- |
| `compact-bfs` | `EngineConfig::CompactBfs` (the default) | `KNOTREL_ENGINE` unset or `compact-bfs` |
| `ett` | `EngineConfig::EulerTour` | `KNOTREL_ENGINE=ett` |
| `hdt` | `EngineConfig::Hdt` | `KNOTREL_ENGINE=hdt` |
| `cluster-forest` | `EngineConfig::ClusterForest`; with lifecycle, `LifecycleGraph::new(EngineConfig::ClusterForest)` | `KNOTREL_ENGINE=cluster-forest`; with lifecycle, also `KNOTREL_TOPOLOGY_LIFECYCLE=true` |

"Experimental engine" means a stable interface with an experimental engine:
the selector, the enum variant and the API stay compatible across 1.x, while
performance, maturity and the usage recommendation carry no promise
([compatibility policy](compatibility.md#22-stable-interface-experimental-engine)).
The asymptotic costs of every engine are in the "Choosing a Backend Engine"
section of the `knotrel-core` crate documentation, on
[docs.rs](https://docs.rs/knotrel-core/latest/knotrel_core/#choosing-a-backend-engine).

## How `cluster-forest` works

`cluster-forest` is a sequential cluster forest after Wulff-Nilsen (SODA 2013,
§3) with the path compression of De Man et al. (PVLDB 2025, §4)
([ADR 0014](https://github.com/knotrel/knotrel/blob/main/docs/architecture/0014-cluster-forest-backend.md)):

- every edge has a level, and the clusters of a level are the connected
  components formed by edges of that level or higher;
- a query walks both vertices up to their root clusters, `O(log N)`, with no
  graph traversal (`N` is the highest vertex count reached);
- deleting an edge runs two alternating searches for a replacement edge from
  the endpoints' clusters, pushes the explored edges of the smaller side one
  level down, and splits clusters when no replacement exists;
- updates cost `O(log² N)` amortized by **this project's derivation** for this
  variant, not by a published theorem. A single deletion can still be
  expensive;
- capacity: at most 2²⁸ vertices and 2²⁸ edges (2²⁷ − 512 nodes with lifecycle
  support), whatever larger limits are configured.

## When to try `cluster-forest`

The advice below comes from the v1.1.0 CF5 qualification: `cluster-forest`
compared with `compact-bfs`, ETT and HDT on candidate `02cba68`, on one host
(Apple M3, 16 GB, macOS), with the classes computed by the campaign analyzer.
**Every statement holds only for the workloads, sizes, host and build
measured.** Results and limitations:
[engine comparison page](https://knotrel.github.io/knotrel-benchmarks/engines/cluster-forest-cf5-02cba68.html) ·
[full report](https://github.com/knotrel/knotrel-benchmarks/blob/e0c5114af55096ffa758527be3b835307cf0e1b6/results/v1.1.0-cf5/REPORT.md).

**Try it when:**

- **queries dominate and `compact-bfs` answers them by searching.** On the HTTP
  server's mixed read and write traffic, `cluster-forest` had about 2.4 to
  250 times the throughput of `compact-bfs`, with lower p99, with lifecycle
  off and on (S1). The core families `components-join-split-v1` and
  `hub-alternatives-v1` (10⁴ and 10⁵ vertices) also favour it over
  `compact-bfs`;
- **a dense graph keeps losing and regaining a bridge.** On two cliques joined
  by one or two toggled bridges (512 and 2 048 vertices) it was faster than
  all three other engines;
- **you rebuild graphs often and would otherwise use ETT or HDT.** Construction
  from the measured family graphs (10⁵ vertices) was faster than ETT and HDT,
  and it used less memory than HDT in every memory cell.

**Keep `compact-bfs`, ETT or HDT when:**

- **updates, construction or memory matter most against `compact-bfs`.**
  `cluster-forest` was slower on staged random updates (K2), on the random
  K2x workload and on construction, and it used more memory (all 24 K4
  metrics, 19 of 20 lifecycle memory metrics);
- **the graph is path- or ring-like with frequent bridge cuts.** On
  `chain-split-rejoin-v1`, `cycle-alternatives-v1` and
  `sustained-churn-path-v1` it was slower than all three other engines, by up to
  about 330 times against ETT;
- **you already use ETT on sparse graphs.** ETT was faster on every sparse core
  family and on staged random updates.

**Measure first** on staged random insert and delete workloads (K2, mixed
results against every engine), and against ETT and HDT on the server, where
results were equivalent except one configuration (`mixed50-b64-c16`, about
30 % slower).

**Known limitations from CF5:**

- **Lifecycle memory on small sparse graphs.** Under sustained lifecycle churn
  on the sparse 1 000-node graph, the heap-growth rule (C3) failed for the
  *reuse same* and *toggle* workloads: +20.2 % and +14.6 % of growth over 30
  cycles, **projected from the slope**, against a 10 % budget. The maintainer
  accepted them as a documented limitation; memory stability for those two
  workloads remains open. Nine RSS runs exceeded the disclosure budget (C4,
  allocator retention). At 10 000 and 100 000 nodes the heap rule passed.
- **Host swap activity was not verifiable** during the campaign (only swap
  occupancy was recorded); the maintainer accepted the results with this
  limitation.
- `compact-bfs` stays the default and `cluster-forest` stays experimental in
  1.1.0. Neither changes without new evidence and an explicit decision.

## Library and server: what each offers

| Capability | Library (`knotrel-core`) | Server (`knotrel-server`) |
| --- | --- | --- |
| Select the engine | `ConnectivityGraph::new(EngineConfig::ClusterForest)`, `with_limits`, `try_from_edges` | `KNOTREL_ENGINE=cluster-forest` at startup; reported by `GET /v1/info` (`"engine": "cluster-forest"`, `"experimental": true`) |
| Add nodes, link, cut, query | Yes | Yes: `/v1/operations`, `/v1/batch`, feeder events and imports |
| Node and edge lifecycle (activate, enable, remove) | Yes: `LifecycleGraph::new(EngineConfig::ClusterForest)` | Yes: `KNOTREL_TOPOLOGY_LIFECYCLE=true` |
| Neighbor listing | Only through `LifecycleGraph::incident_after`; `ConnectivityGraph::enable_neighbor_listing` returns `false` and `as_compact` returns `None` | Node listing and detail always; incident and edge listing (`KNOTREL_INSPECTION=true`) only with lifecycle support, otherwise 409 `inspection_unsupported` |
| Engine statistics | None: the backend type is internal | None beyond the common stats |
| Switch engine at runtime | Build a new graph | No: restart with another `KNOTREL_ENGINE`; the in-memory graph is lost and must be rebuilt or re-imported |

The engine never changes answers: every engine returns the same exact results
for the same operations.

## Examples

### Library

Plain connectivity:

```rust
use knotrel_core::{ConnectivityGraph, EngineConfig, GraphError};

fn main() -> Result<(), GraphError> {
    let mut graph = ConnectivityGraph::new(EngineConfig::ClusterForest);
    graph.link(1, 2)?;
    graph.link(2, 3)?;
    assert!(graph.connected(1, 3)?);
    graph.cut(2, 3)?;
    assert!(!graph.connected(1, 3)?);
    assert!(graph.engine().is_experimental());
    Ok(())
}
```

Node and edge lifecycle: take a node offline and restore it without
recreating its edges.

```rust
use knotrel_core::{EngineConfig, LifecycleGraph};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut graph = LifecycleGraph::new(EngineConfig::ClusterForest)?;
    for (u, v) in [(1, 2), (2, 4), (1, 3), (3, 4)] {
        graph.link(u, v)?;
    }
    graph.set_node_active(2, false)?;
    assert!(graph.connected(1, 4)?); // through 3
    graph.set_node_active(3, false)?;
    assert!(!graph.connected(1, 4)?);
    graph.set_node_active(2, true)?;
    assert!(graph.connected(1, 4)?); // the edges of 2 are back
    Ok(())
}
```

The same two examples are doctests of the
[`knotrel-core` crate documentation](https://github.com/knotrel/knotrel/blob/main/crates/knotrel-core/src/lib.rs)
(section "Choosing a Backend Engine"), run by `cargo test --doc -p knotrel-core`.

### Server

Plain mode:

```sh
KNOTREL_ENGINE=cluster-forest ./target/release/knotrel-server
curl -s http://127.0.0.1:8080/v1/info
# {"engine":"cluster-forest","experimental":true,...}
curl -s -H 'Content-Type: application/json' -d '{"operations":[
  {"op":"link","source":"1","target":"2"},
  {"op":"link","source":"2","target":"3"},
  {"op":"connected","source":"1","target":"3"}]}' http://127.0.0.1:8080/v1/batch
# {"results":[{"result":"changed","changed":true},{"result":"changed","changed":true},{"result":"connected","connected":true}]}
```

With node and edge lifecycle:

```sh
KNOTREL_ENGINE=cluster-forest KNOTREL_TOPOLOGY_LIFECYCLE=true ./target/release/knotrel-server
curl -s -H 'Content-Type: application/json' -d '{"operations":[
  {"op":"link","source":"1","target":"2"},{"op":"link","source":"2","target":"4"},
  {"op":"link","source":"1","target":"3"},{"op":"link","source":"3","target":"4"},
  {"op":"set_node_active","node":"2","active":false},
  {"op":"connected","source":"1","target":"4"}]}' http://127.0.0.1:8080/v1/batch
# the last result is {"result":"connected","connected":true}: node 3 still connects 1 and 4
```

Without `KNOTREL_TOPOLOGY_LIFECYCLE=true`, lifecycle operations answer 409
`topology_lifecycle_disabled`. With `ett` or `hdt`, lifecycle support makes
startup fail: `KNOTREL_TOPOLOGY_LIFECYCLE requires KNOTREL_ENGINE=compact-bfs or
cluster-forest`.

### Going back to `compact-bfs`

- **Library:** construct the graph with `EngineConfig::CompactBfs` (or
  `ConnectivityGraph::default()`); `LifecycleGraph::new(EngineConfig::CompactBfs)`
  keeps lifecycle support. Rebuild the graph from your source data: there is no
  in-place conversion.
- **Server:** remove `KNOTREL_ENGINE` (or set it to `compact-bfs`) and restart.
  The graph lives in memory, so a restart loses it: rebuild it from your source
  as the [deployment guide](deployment.md) describes, or replay your writes. `GET /v1/info` then reports `"engine": "compact-bfs"` and
  `"experimental": false`.

## Getting 1.1.0

- **Server:** download and verify the v1.1.0 server for your platform from the
  [release assets](https://github.com/knotrel/knotrel/releases/tag/v1.1.0)
  ([installing a release](https://github.com/knotrel/knotrel#installing-a-release)).
- **Library:**

  ```toml
  [dependencies]
  knotrel-core = "1.1"
  ```

With v1.0.0 (server or `knotrel-core` 1.0), use the selectors in the first
three rows of the table only.

## See also

- [Node and edge lifecycle contract](topology-lifecycle.md)
- [HTTP API: startup configuration and engine selection](http-api.md#startup-configuration-and-engine-selection)
- [ADR 0014: cluster forest backend](https://github.com/knotrel/knotrel/blob/main/docs/architecture/0014-cluster-forest-backend.md)
- [CF5 qualification plan](releases/v1.1.0-cf5-qualification-plan.md)
- [Benchmarks](benchmarks.md)
