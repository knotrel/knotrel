# Inspection API cost check

Targeted check of the read-only inspection routes added for the console data
browser (`docs/http-api.md`, "Inspection (opt-in)"). It is **not** a release
qualification. It answers whether a page stays cheap on a significant graph,
whether browsing disturbs concurrent queries and mutations, and what the node
index costs on the mutation path.

The protocol and pass criteria below were fixed **before** any measurement.

## Protocol

- **Build:** `cargo build -p knotrel-server --release --locked` on the
  candidate commit. Only the default allocator and features.
- **Machine:** one developer machine (recorded with the results), loopback
  HTTP, no other benchmark running (checked with `ps` before starting).
- **Server:**
  - `KNOTREL_ENGINE=compact-bfs`, `KNOTREL_TOPOLOGY_LIFECYCLE=true`;
  - `KNOTREL_MAX_NODES=unlimited`, `KNOTREL_MAX_EDGES=unlimited`;
  - `KNOTREL_METRICS_ENABLED=true`;
  - `KNOTREL_INSPECTION=true`, or `false` for the comparison runs.
- **Graph:**
  - 500 000 nodes and 1 500 000 random edges from a fixed seed, loaded with
    `POST /v1/batch` (1024 operations per batch);
  - 1 % of nodes deactivated;
  - one hub with 20 000 neighbors.
- **Driver:** `scripts/measure-inspection-costs.py`, which uses only the Python
  standard library. Clients use persistent HTTP/1.1 connections; latency is
  measured by the client, and server lock-hold time is read from
  `knotrel_graph_job_execution_seconds{kind="inspect"}`.

Measurements:

| ID | What | How |
| --- | --- | --- |
| M1 | Page cost | 300 pages each of: `GET /v1/nodes?limit=1000` at random cursors; `GET /v1/edges?limit=1000` at random cursors; the hub's `GET /v1/nodes/{hub}/edges?limit=1000` at random cursors; `GET /v1/nodes?state=inactive&limit=1000` (budget-bound worst case). Client p50/p95/max, plus the server execution histogram. |
| M2 | Interference | Mutation client: `set_node_active` toggles and `link`/`cut` between existing nodes, 3 000 operations. Query client: `node_state` lookups, 3 000 operations. Each runs alone, then together with two clients that page `GET /v1/edges?state=disabled` continuously (worst case: every page uses the full scan budget). Compare p50/p95. |
| M3 | Mutation-path cost of the index | 100 000 `add_node` of new IDs and 100 000 `link` creating one new endpoint, in batches of 1024, with inspection on and off (3 alternating runs each). Compare throughput medians. |
| M4 | Memory | RSS after loading the graph, inspection on and off. |

## Pass criteria (fixed before measuring)

| ID | Criterion |
| --- | --- |
| C1 | p95 server execution time of every M1 page type ≤ 10 ms; client p95 ≤ 25 ms |
| C2 | Under concurrent worst-case browsing, mutation and query p95 latency ≤ alone p95 + 5 ms |
| C3 | Node-creating mutation throughput with inspection on ≥ 85 % of inspection off |
| C4 | RSS difference (on − off) ≤ 40 bytes per node |

A failed criterion is reported as such, with its numbers. It is not
reinterpreted.

## Results (2026-11-20)

**Environment.**
- Machine: macOS 26.5.2, arm64, 8 CPUs; one developer laptop.
- Build: release, from the working tree that became commit `b85e22f`
  (`feature/console-data-browser`). Between the measurement and that commit
  only two lint fixes were made: query-parameter errors became messages, and a
  loop counter became `enumerate`. There was no algorithmic change. Re-measure
  on the release candidate before relying on these numbers.
- Graph: 500 000 nodes, 1 500 000 random edges plus a hub of 20 000
  neighbors, loaded in 3.5 s (inspection off) and 3.6 s (on).

Raw output:
[`console-inspection-costs.json`](console-inspection-costs.json)
(SHA-256 `86800915f34cccd80ae6f0ae819efd5d867532475b56081c834948c6fa55f3e4`).

**M1 — page cost** (300 pages each; client latency over loopback HTTP; server
execution is the upper bound of the histogram bucket containing p95 and the
maximum):

| Page | Client p50 | Client p95 | Client max | Server exec p95 | Server exec max |
| --- | --- | --- | --- | --- | --- |
| `GET /v1/nodes?limit=1000` | 0.19 ms | 0.22 ms | 0.31 ms | ≤ 50 µs | ≤ 50 µs |
| `GET /v1/edges?limit=1000` | 0.28 ms | 0.44 ms | 0.48 ms | ≤ 0.5 ms | ≤ 0.5 ms |
| Hub `GET /v1/nodes/{hub}/edges?limit=1000` | 0.17 ms | 0.18 ms | 0.26 ms | ≤ 50 µs | ≤ 50 µs |
| `GET /v1/nodes?state=inactive&limit=1000` (budget-bound) | 0.29 ms | 0.32 ms | 0.38 ms | ≤ 0.5 ms | ≤ 0.5 ms |

**M2 — interference** (3 000 operations each; two concurrent clients paging
`GET /v1/edges?state=disabled`, every page using the full scan budget):

| Workload | Alone p50 / p95 | With browsing p50 / p95 | Browse pages |
| --- | --- | --- | --- |
| Mutations (`set_node_active`, `link`) | 0.065 / 0.074 ms | 0.357 / 0.376 ms | 7 015 |
| Queries (`node_state`) | 0.060 / 0.072 ms | 0.067 / 0.145 ms | 2 526 |

Continuous worst-case browsing adds about 0.3 ms to each mutation: writers
wait for in-flight shared-lock readers. Queries share the lock, so they are
barely affected.

**M3 — mutation-path cost of the node index** (medians of 3 alternating runs,
100 000 operations each, batches of 1024):

| Operation | Inspection off | Inspection on | On / off |
| --- | --- | --- | --- |
| `add_node` (new IDs) | 2 003 680 ops/s | 1 923 504 ops/s | 0.96 |
| `link` with one new endpoint | 634 839 ops/s | 621 847 ops/s | 0.98 |

**M4 — memory** at 500 000 nodes: RSS 224.3 MB (off) and 229.8 MB (on),
about 11 bytes per node.

**Criteria.**

| ID | Result |
| --- | --- |
| C1 | **PASS**: worst server p95 ≤ 0.5 ms and worst client p95 0.44 ms, against 10 ms and 25 ms |
| C2 | **PASS**: mutation p95 0.376 ms against 0.074 + 5 ms; query p95 0.145 ms against 0.072 + 5 ms |
| C3 | **PASS**: ratios 0.96 and 0.98, against ≥ 0.85 |
| C4 | **PASS**: 11 bytes per node, against ≤ 40 |

## Run 2 (2026-11-20, after making the index→ID vector opt-in)

The vector that plain `compact-bfs` uses to list neighbors is now kept only
when inspection is enabled. The same protocol was re-run on that code. Raw
output: [`console-inspection-costs-run2.json`](console-inspection-costs-run2.json)
(SHA-256 `a0e4dabd74cc668be76839e94683ccb76a4c5a1b7e3d4d51eae389b2ecf71137`).

| Measurement | Run 2 |
| --- | --- |
| M1 worst client p95 / server p95 | 0.47 ms / ≤ 0.5 ms (budget-bound page: max ≤ 1 ms) |
| M2 mutations alone → with browsing (p95) | 0.081 → 0.453 ms |
| M2 queries alone → with browsing (p95) | 0.084 → 0.144 ms |
| M3 on/off (`add_node`, `link` with new endpoint) | 0.95, 0.98 |
| M4 RSS difference | 5.6 bytes per node |

All four criteria **PASS** again.

**Reading M4 correctly.**
- The protocol runs in lifecycle mode, where neighbors come from the lifecycle
  catalog, so the opt-in vector is never allocated. M4 therefore measures only
  the ordered node index.
- RSS differences of a few bytes per node are within run-to-run variation (11
  and 5.6 bytes in the two runs).
- In plain `compact-bfs` mode the vector adds 8 bytes per node by construction.
- The user documentation states "up to about 20 bytes per node".

**Limits of this check.**
- One machine, one run of each measurement, loopback only.
- The bucket bounds of the execution histogram are coarse: 50 µs, then
  0.5 ms.
- The index→ID vector that plain `compact-bfs` always keeps is present in both
  the "off" and "on" runs, so M3 does not isolate it. Its effect on
  published v0.7.0 evidence needs the re-qualification described in the
  [design plan](superpowers/plans/2026-11-20-console-data-browser.md#re-qualification-impact).
