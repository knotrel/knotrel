# Performance history (archive)

This page keeps the earlier release-to-release qualifications for the record.
For the current release, see [Performance by version](performance-by-version.md).
The release notes remain the authoritative statement for each release.

**These comparisons cannot be compared with each other.** Each section is one
campaign: one release against its predecessor, on its own day, with its own
binaries, rules and cells. Reading across sections, or chaining them (for
example v0.3.0 → v0.5.0 by multiplying two changes), compares different
things:
- **Decision rules differ.** v0.3.0 classified only the primary metric. From
  v0.4.0 on, every gated p99 and memory metric must PASS as well, so the same
  data can be PASS under one rule and UNRESOLVED under the other.
- **References differ.** v0.3.0 measured against a Rust 1.100.0 rebuild of
  v0.2.0; v0.4.0 and v0.5.0 against the predecessor rebuilt from its tag;
  v0.6.0 against published v0.5.0; v0.7.0 against published v0.6.0.
- **Binaries and days differ.** Host noise, toolchain and code layout change
  between campaigns. v0.4.0's `compact-workspace` regression is a measured
  example of layout sensitivity.
- **Scope differs.** Lifecycle cells exist from v0.5.0 and are measured
  against references of different capability; contention cells exist from
  v0.6.0.

**Common method.** Paired runs in fresh processes, in balanced order. The
figure is the median paired change `exp(median(ln(new/old))) − 1` (positive
is slower or larger), with an order-statistic interval on the paired log
ratios. PASS means the upper bound is within tolerance: a regression
decision, not proof of a speedup. One Apple M3 host on macOS, Rust 1.100.0,
locally built release binaries, default `compact-bfs` server configuration.

**Measured candidates.** v0.4.0 `62c2cea` (release artifacts verified to
contain the same executed code); v0.5.0 `dcd8ccc`; v0.6.0 `48bc82f` (`main`
at `8d21f38` has the same executed code); v0.7.0 `fa5f3a9` (the release
commit's `crates/`, `Cargo.toml` and `Cargo.lock` are identical).

## v0.7.0 compared with v0.6.0

**v0.7.0 (released 2026-11-20; qualification accepted with exceptions)
is not demonstrated to be faster in general than v0.6.0.**
- **Faster:**
  - read-heavy concurrent requests: about 2× the throughput with 4 clients,
    and 4.5–5.2× for queries during imports or feeder events at 100k nodes;
  - lifecycle HTTP imports: 16–20%;
  - compaction pauses: 6–11% shorter.
- **Fixed:** the v0.6.0 heap-growth failures under churn are resolved.
- **Not every gate passed.** Three inconclusive results and two unqualified
  lifecycle requirements on a hub topology are accepted by the maintainer
  for v0.7.0 only.

Candidate `fa5f3a9` against published v0.6.0 (`5db9ec4`), 2026-11-20,
qualification 3 attempt q3-3 (conforming). The release commit's `crates/`,
`Cargo.toml` and `Cargo.lock` are identical to the measured candidate.
**Outcome: accepted with exceptions.** This is not "all gates PASS".

The paired figure is `exp(median(ln(v0.7.0/v0.6.0))) − 1`; positive means v0.7.0 is slower or uses more memory.

### Release cells, lifecycle off

The primary metric is 10%; p99 latency 25% and memory 20% must also pass.

| Workload | v0.6.0 → v0.7.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | +3.35% [−4.18%, +6.49%] PASS |
| Core sparse traversal, `compact-workspace` | +2.30% [−16.32%, +8.84%] PASS |
| Core sustained churn, `compact-bfs` | −1.94% [−4.98%, +4.25%] PASS |
| Core sustained churn, `compact-workspace` | −3.46% [−8.34%, +5.21%] PASS |
| HTTP batch, size 1 | −0.24% [−2.59%, +3.21%] PASS |
| HTTP batch, size 8 | +3.62% [−1.95%, +8.12%] PASS |
| HTTP batch, size 64 | −0.11% [−4.80%, +5.53%] PASS |
| HTTP batch, size 256 | +0.28% [−3.82%, +4.53%] PASS |
| HTTP lifecycle, time to ready | −3.08% [−7.24%, +1.17%] PASS |

### Concurrency

Contention cells: 10% throughput tolerance, 25% p99 tolerance.

| Workload | Throughput | p99 | Outcome |
|---|---|---|---|
| Read-heavy (95% queries), size 1, 4 clients, 10k | 15,289 → 32,232 req/s | 0.53 → 0.26 ms | PASS, improved |
| Read-heavy, size 8, 16 clients, 100k | 213 → 350 req/s | 91.1 → 90.7 ms | PASS, improved |
| Mixed 50%, size 64, 16 clients, 100k | 51 → 51 req/s | 346 → 353 ms | PASS |
| Lifecycle 50%, size 8, 16 clients, 100k | 399 → 399 req/s | 54.7 → 54.1 ms | PASS |
| Queries during a source import, 100k | 204 → 1,053 req/s | 92.2 → 77.0 ms | PASS, improved |
| Queries during feeder events, 100k | 206 → 938 req/s | 91.3 → 50.5 ms | PASS, improved |

### Lifecycle on

- **Flagged imports and churn** (10% tolerance):
  - HTTP imports: −16% to −20%, PASS, improved;
  - core imports at 100k: PASS;
  - fresh-ID churn: −4% to −5%, PASS, improved;
  - two core imports at 10k (sparse, hub) are **INCONCLUSIVE**, accepted
    for v0.7.0 only.
- **Memory:** import and churn memory 10/10 PASS; churn heap peaks 12–17%
  lower.
- **Heap growth under churn (C3):** 15/15 PASS. The three v0.6.0 failures
  are resolved by the catalog tombstone handling.
- **Compaction pauses (T5)** are new, with a 25% tolerance, measured against
  v0.7.0 without the tombstone handling. All 6/6 PASS, 6–11% shorter; P_max
  at 100k sparse is 17.3 → 15.6 ms.
- **Lifecycle cells against capability-equivalent references:** 58 deciding
  timing cells PASS. One is **INCONCLUSIVE**, accepted: metrics overhead on
  lifecycle batches, sparse 10k, +6.5% [+1.7%, +7.4%] against a 6% budget.

**Not qualified, accepted exception:** lifecycle batches and feeder events
on the `hub` 10k topology.
- v0.6.0 cannot offer the same atomicity there, so no equivalent reference
  exists.
- Comparisons against references that are not atomic are indicative only:
  +4.0% or −6.8% (batches), +5.0% or −6.4% (events).

**Disclosed:**
- **RSS retention under churn:** 6 of 15 cells exceed the RSS budget, a
  disclosure category.
- **Imports dominated by inactive entries:**
  - about 5× an import of the effective graph only (+408%, recurring since
    v0.5.0);
  - +58% against a capability-equivalent reference, PASS at 100%.

### What the evidence supported for v0.7.0

| If you… | What the evidence supports |
|---|---|
| Run many concurrent read-heavy clients | Query-only requests now run in parallel: about 2× the throughput with 4 clients at 10k nodes, and 4.5–5× for queries during imports or feeder events at 100k nodes, with lower p99 |
| Keep lifecycle off (the default) | No workload regressed against v0.6.0 in the nine release cells |
| Enable lifecycle with fresh-ID churn | Heap growth between compactions is within its rule, and compaction pauses are shorter. RSS can still grow through allocator retention |
| Send large lifecycle batches or feeder events on hub-like graphs | Not qualified against v0.6.0. Indicative medians are within ±7% of non-atomic references |
| Import graphs dominated by inactive entries | Still about 5× the cost of importing only the effective graph, a few ms per 10k-node import on the measured host |
| Expect v0.7.0 to be faster overall | Not supported for write-heavy or mixed workloads, which are unchanged within tolerance |

Sources: [v0.7.0 release notes](releases/v0.7.0-release-notes.md#performance-qualification); knotrel-benchmarks `results/v0.7.0-qualification-3/DECISIONS.md` and `results/v0.7.0-qualification-3/analysis/q3-3/`.

## v0.6.0 compared with v0.5.0

| Workload | v0.5.0 → v0.6.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | −0.95% [−3.78%, +2.92%] PASS |
| Core sparse traversal, `compact-workspace` | +6.20% [−1.72%, +19.43%] INCONCLUSIVE, disclosed |
| Core sustained churn, `compact-bfs` | −0.34% [−1.17%, +0.09%] PASS |
| Core sustained churn, `compact-workspace` | −0.27% [−4.75%, +1.71%] PASS |
| HTTP batch, size 1 | +0.30% [−5.15%, +4.15%] **UNRESOLVED**¹ |
| HTTP batch, size 8 | +0.40% [−9.43%, +2.05%] PASS |
| HTTP batch, size 64 | −0.73% [−7.81%, +6.96%] PASS |
| HTTP batch, size 256 | −0.93% [−2.75%, +0.60%] **UNRESOLVED**² |
| HTTP lifecycle, time to ready | +7.18% [−18.30%, +24.15%] INCONCLUSIVE, disclosed |

¹ The primary metric passes. The request p99 is unresolved: −2.29% [−26.62%,
+52.03%] (120.4 → 131.2 µs).

² The primary metric passes. The sampled maximum RSS is unresolved: +1.15%
[+0.23%, +22.16%] against 20% (6.82 → 6.96 MB).

Candidate `48bc82f` against published v0.5.0 (`51e3b4d`), 2026-11-16. Full
report: `results/v0.6.0-qualification/COMPARISON-v0.5.0-v0.6.0.md` in
knotrel-benchmarks. The maintainer accepted the non-PASS results as
disclosed limitations ([v0.6.0 release notes](releases/v0.6.0-release-notes.md#performance-qualification)).

**Lifecycle off**: 5 PASS, 2 INCONCLUSIVE (disclosed),
2 UNRESOLVED, no REGRESSION.

**Lifecycle on, release to release** (new in v0.6.0; median change
[interval], 10% tolerance):

| Workload | v0.5.0 → v0.6.0 | Outcome |
|---|---|---|
| Core flagged import, 100k nodes, 20% inactive / 10% disabled | 15.60 → 13.53 ms, −13.6% [−14.9%, −12.0%] | PASS, improved |
| Core flagged import, 100k, 60/30 | −0.3% [−3.9%, +1.9%] | PASS |
| Core flagged import, 10k sparse / hub | −11.7% [−15.4%, −8.2%] / −5.6% [−10.6%, −4.1%] | PASS, improved |
| HTTP import, 100k, 20/10 | +0.7% [−0.3%, +1.7%] | PASS |
| HTTP import, 100k, 60/30 | 25.13 → 28.48 ms, **+13.4% [+12.6%, +14.0%]** | **REGRESSION** |
| HTTP import, 10k, 20/10 | +7.7% [+6.1%, +8.7%] | PASS |
| Fresh-ID churn, 100k sparse / hub | −5.8% [−7.3%, −3.2%] / −5.7% [−6.7%, −3.3%] | PASS, improved |
| Same-ID and toggle churn, 100k | −3.4% / +0.6% | PASS |
| HTTP import peak live heap, 100k (create 20/10, 60/30; replace) | 111 / 129 / 130 → 33 / 31 / 50 MiB | PASS (−61% to −76%) |
| Fresh-churn heap max / compaction peak, 100k sparse | 37.3 / 49.2 → 33.5 / 40.7 MiB | PASS |

**Recurring lifecycle failures** (cost against references, v0.5.0 rules):
- the import with 60% inactive nodes is still a REGRESSION (+420%);
- heap growth under fresh-ID churn still FAILs its check in three cells;
- metrics overhead on lifecycle batches is still INCONCLUSIVE.

**Contention:** no regression against v0.5.0 on the cells that measured what
they declared. With read-heavy work, the single graph lock dominates latency
from four clients on, and throughput does not grow with clients. v0.7.0
addresses this. Feeder catch-up overlapping queries was not measured at 100k
nodes (tooling defect, disclosed in the report).

## v0.5.0 compared with v0.4.0

| Workload | v0.4.0 → v0.5.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | −7.23% [−33.47%, +2.83%] PASS |
| Core sparse traversal, `compact-workspace` | −1.94% [−9.87%, +6.00%] PASS¹ |
| Core sustained churn, `compact-bfs` | −1.02% [−3.67%, +0.64%] PASS |
| Core sustained churn, `compact-workspace` | +0.19% [−1.38%, +2.28%] PASS |
| HTTP batch, size 1 | +0.04% [−5.57%, +5.32%] **UNRESOLVED**² |
| HTTP batch, size 8 | −0.42% [−5.05%, +8.17%] PASS |
| HTTP batch, size 64 | −2.11% [−4.01%, +1.96%] **UNRESOLVED**³ |
| HTTP batch, size 256 | −1.72% [−3.13%, +6.84%] PASS |
| HTTP lifecycle, time to ready | +5.14% [−17.35%, +22.13%] INCONCLUSIVE |

¹ A PASS against v0.4.0 does not mean that the v0.4.0 regression against
v0.3.0 was recovered: releases cannot be chained (see the top of this page).

² The primary metric passes. The request p99 is unresolved: +6.04% [−27.84%,
+50.04%] (119.4 → 132.6 µs).

³ The primary metric passes. The sampled maximum RSS is unresolved: +0.11%
[−3.50%, +25.14%] against 20% (6.94 → 7.03 MB).

| Outcome | Cells |
|---|---|
| PASS | 6 of 9 |
| INCONCLUSIVE, disclosed | 1 (lifecycle, time to ready) |
| UNRESOLVED | 2 (HTTP batch size 1 p99, HTTP batch size 64 sampled RSS) |
| REGRESSION | none |

**Maintainer waivers:** the maintainer accepted both UNRESOLVED results for
v0.5.0 only. These waivers are not a pass of the regression gate, and the
v0.4.0 waivers did not carry forward. No improvement is claimed: no cell met
the criterion (a passing primary metric with an interval entirely below zero).

## v0.4.0 compared with v0.3.0

| Workload | v0.3.0 → v0.4.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | +8.71% [−2.34%, +31.89%] **UNRESOLVED**¹ |
| Core sparse traversal, `compact-workspace` | **+28.90% [+15.44%, +35.73%] REGRESSION**² |
| Core sustained churn, `compact-bfs` | +1.08% [−1.72%, +5.18%] PASS |
| Core sustained churn, `compact-workspace` | +0.57% [−0.40%, +2.52%] PASS |
| HTTP batch, size 1 | +5.89% [−3.34%, +8.52%] **UNRESOLVED**³ |
| HTTP batch, size 8 | +1.47% [−7.64%, +14.12%] INCONCLUSIVE |
| HTTP batch, size 64 | +1.10% [−3.83%, +9.68%] PASS |
| HTTP batch, size 256 | −2.29% [−8.98%, +4.93%] PASS |
| HTTP lifecycle, time to ready | −12.86% [−21.83%, +23.21%] INCONCLUSIVE |

¹ The primary median is within tolerance, but the connectivity-query p99 is
unresolved: +2.02% [−4.97%, +28.53%].

² Library API `Graph::connected_with_workspace` only. The server does not use
it. See [what the evidence supported](#what-the-evidence-supported-at-each-release).

³ The primary metric passes. The request p99 is unresolved: −0.11% [−33.54%,
+57.62%].

| Outcome | Cells |
|---|---|
| PASS | 4 of 9 |
| INCONCLUSIVE, disclosed | 2 (HTTP batch size 8, lifecycle) |
| UNRESOLVED | 2 (`compact-bfs` sparse p99, HTTP batch size 1 p99) |
| REGRESSION | 1 (`compact-workspace` sparse traversal) |

**Maintainer waivers:** the maintainer accepted the REGRESSION and both
UNRESOLVED results for v0.4.0, with disclosure. These waivers are not a pass
of the regression gate. No v0.4.0 cell met the criterion for citing an
improvement, which is a passing primary metric with an interval entirely below
zero.

**Diagnosis of the regression,** which is diagnostic context, not acceptance
evidence:
- the measured function has identical machine instructions in both versions;
- the extra time is extra CPU cycles for the same instruction count;
- diagnostic builds that changed code alignment removed the difference, by
  slowing v0.3.0.

**What the diagnosis shows and does not show:**
- **It shows** sensitivity to binary layout.
- **It does not** identify the mechanism.
- **It does not** make the regression harmless.
- **It does not** predict the effect in other binaries.

## v0.3.0 compared with v0.2.0

| Workload | v0.2.0 → v0.3.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | +0.10% [−21.95%, +6.14%] PASS |
| Core sparse traversal, `compact-workspace` | −2.26% [−11.13%, +9.41%] PASS |
| Core sustained churn, `compact-bfs` | −0.01% [−1.76%, +8.01%] PASS |
| Core sustained churn, `compact-workspace` | −0.46% [−5.30%, +0.76%] PASS |
| HTTP batch, size 1 | −0.71% [−7.37%, +7.69%] PASS |
| HTTP batch, size 8 | −1.42% [−15.63%, +12.09%] INCONCLUSIVE |
| HTTP batch, size 64 | −0.83% [−7.56%, +5.08%] PASS |
| HTTP batch, size 256 | +0.08% [−1.55%, +6.98%] PASS |
| HTTP lifecycle, time to ready | +8.02% [−11.46%, +21.97%] INCONCLUSIVE |

- **Outcome:** 7 PASS and 2 INCONCLUSIVE (HTTP batch size 8 and lifecycle),
  accepted with disclosure.
- **Improvement:** none claimed.

## v0.5.0 lifecycle cost (opt-in, not release-to-release)

These cells measure the cost of the opt-in node and edge lifecycle on
`compact-bfs` against semantically equivalent reference implementations. They
apply only when `KNOTREL_TOPOLOGY_LIFECYCLE=true` or `LifecycleGraph` is used,
and are **not** v0.5.0 vs v0.4.0 results. Tolerances are per cell, as stated
in the [release notes](releases/v0.5.0-release-notes.md#performance-qualification).

- **Timing, 61 cells:** 56 PASS, **2 REGRESSION**, 3 INCONCLUSIVE.
  - Core lifecycle operations, high-degree deactivation and removal, lifecycle
    HTTP batches and feeder events passed.
  - **REGRESSION:** importing with lifecycle flags, 100,000 nodes: +109.9%
    [+101.9%, +115.9%] (14.12 → 29.41 ms); 10,000 nodes with 60% inactive nodes
    and 30% disabled edges: +444.8% [+427.3%, +464.6%] (0.43 → 2.35 ms); both
    against a 100% tolerance. The reference imports only the effective graph and
    skips validation and atomic publication, which favors it.
  - **INCONCLUSIVE:** flagged import of 10,000 nodes (+100.8% against 100%),
    lifecycle batches with `/metrics` on (+6.5% against 6%) and `/metrics` on a
    lifecycle-disabled `ett` server (+4.0% against 6%).
- **Memory:** 2.45× the plain engine's heap after building 100,000 nodes
  (budget 3×). Under churn with fresh node IDs, **3 heap-growth checks failed**
  at 100,000 nodes (heap grew by 11.5 to 12.4 MiB, to 31.8–34.5 MiB, as
  retired slots accumulate until compaction). Engine slots stay within `2 × nodes + 1024`, but total
  heap, rebuild peaks and RSS have no hard bound.
- **Maintainer waivers:** both REGRESSIONs, the three heap-growth failures and
  the three INCONCLUSIVE cells are accepted for v0.5.0 only.
- **ETT and HDT:** lifecycle is not supported on them in v0.5.0; earlier
  candidates measured it there and their failures remain recorded.

## Other v0.4.0 measurements (not release-to-release)

These campaigns use different baselines or questions. They are **not** v0.4.0
vs v0.3.0 results and must not be read as such.

**Hot-path audit, against development commit `e1e1181`.** It measured the
v0.4.0 hot-path changes against `e1e1181` (a v0.3.0-era development commit,
not the v0.3.0 release), with a same-binary A/A arm
([campaign](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/hot-path-audit-20261114/README.md)):
- **Allocations:** heap allocations per request fell by 16.7% (single query)
  to 75.0% (100,000-node import).
- **In-process router, no network:**
  - import −21%;
  - feeder batch of 1,024 events −14% to −19%;
  - batches of 1,024 queries or cut/link −6% to −9%;
  - single-request latency unchanged within noise.
- **Core bulk initialization:** `compact-bfs` −6% to −7%; `ett-scan` and
  `hdt` sustained churn −4%.
- **End-to-end peak RSS during import:** −4.7%, marginal.

**These numbers do not transfer to v0.3.0.** The release-to-release HTTP
batch results above did not show a corresponding end-to-end improvement
beyond noise.

**Prometheus metrics overhead (`/metrics` on vs off, v0.4.0 candidate).** In
six in-process router scenarios, the largest median cost was +1.24% [+0.63%,
+2.24%], and all six passed the pre-approved budget (median ≤ 3%, upper
bound ≤ 6%).
- **`single_connected`:** it measured about 11% faster with metrics on. Its
  cause is not established, and it is not a speedup claim.
- **Scope:** network and log-sink costs are not included.

**ETT and HDT engines.** The audit above covers some ETT/HDT core paths
against `e1e1181`. **No release-to-release comparison exists for ETT or HDT,
and the HTTP server with `KNOTREL_ENGINE=ett` or `hdt` has no performance
qualification.**

## What the evidence supported at each release

**v0.6.0 (released 2026-11-17; non-PASS results accepted as disclosed limitations):**

| If you… | What the evidence supports |
|---|---|
| Keep lifecycle off (the default) | No workload regressed against v0.5.0. Startup readiness and `compact-workspace` traversal are inconclusive; the size-1 batch p99 and the size-256 sampled RSS are not bounded at their tolerances |
| Enable lifecycle and import large graphs over HTTP | Peak import memory is much lower than in v0.5.0 (about the body, then the request plus the new graph, beside the old graph). Imports dominated by inactive entries take about 13% longer than in v0.5.0 |
| Remove nodes with fresh IDs under lifecycle | About 6% faster than v0.5.0, with lower heap peaks and shorter compaction pauses (about 18 ms at 100k nodes on the measured host). Heap still grows between compactions |
| Run many concurrent read-heavy clients | Throughput does not grow with clients: every query waits for the single graph lock |
| Expect v0.6.0 to be faster overall | Not supported by the evidence |

**v0.5.0:**

| If you… | What the evidence supports |
|---|---|
| Keep lifecycle off (the default) | No workload regressed against v0.4.0. Startup readiness is inconclusive; the size-1 batch p99 and the size-64 sampled RSS are not bounded at their tolerances |
| Enable lifecycle on `compact-bfs` | Operations and batches stayed within their budgets. Flagged imports can take about twice as long as importing only the effective graph, more with many inactive entries. Heap grows under churn with fresh IDs until compaction; plan memory headroom |
| Expect v0.5.0 to be faster overall | Not supported by the evidence |

**v0.4.0:**

| If you… | What the evidence supports |
|---|---|
| Run the server with defaults (`compact-bfs`) | Batch throughput windows at sizes 1, 64 and 256 stayed within the 10% tolerance against v0.3.0. Batch size 8 and startup readiness are inconclusive. The p99 for size-1 batches, and the core connectivity-query p99, are not bounded at 25% |
| Embed `knotrel-core` and call `Graph::connected_with_workspace` | A 28.9% slower result was measured in the benchmark binary. The size and direction in your binary are unknown. Measure your own workload before upgrading |
| Embed `knotrel-core` and call `Graph::connected` / `ConnectivityGraph` with the default engine | Throughput is within tolerance in sustained churn. Sparse traversal and its tail latency are unresolved |
| Enable `/metrics` | Overhead stayed within the approved budget in-process |
| Use ETT or HDT | Not qualified release to release |
| Expect v0.4.0 to be faster overall | Not supported by the evidence |

## Sources

- **v0.7.0:** [release notes](releases/v0.7.0-release-notes.md#performance-qualification)
  and knotrel-benchmarks `results/v0.7.0-qualification-3/DECISIONS.md`, with
  the analyses in `results/v0.7.0-qualification-3/analysis/q3-3/`.
- **v0.6.0:** [release notes](releases/v0.6.0-release-notes.md#performance-qualification)
  and [knotrel-benchmarks `results/v0.6.0-qualification/COMPARISON-v0.5.0-v0.6.0.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v0.6.0-qualification/COMPARISON-v0.5.0-v0.6.0.md).
- **v0.5.0:** [release notes](releases/v0.5.0-release-notes.md#performance-qualification)
  and [knotrel-benchmarks `results/lifecycle-qualification-v0.5.0`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/lifecycle-qualification-v0.5.0/README.md)
  (protocol, run reports, experiment A on `dcd8ccc` and the maintainer
  decision). Its raw evidence is kept in a private repository; see that
  directory's `evidence/INVENTORY.md`.
- **v0.4.0:** [release notes](releases/v0.4.0-release-notes.md#regression-qualification-against-v030).
  The qualification bundle is
  `v0.4.0-qualification-20261115T183151Z-62c2cea`, with its decision report
  and maintainer dispositions.
- **v0.3.0:** [release notes](releases/v0.3.0-release-notes.md#performance-qualification).
  The qualification bundle is `v0.3.0-qualification-20261112T224213Z-6a1267f`.
- **Hot-path audit:** [knotrel-benchmarks `results/hot-path-audit-20261114`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/hot-path-audit-20261114/README.md)
  and its [re-check](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/hot-path-recheck-20261115/README.md).
- **Metrics overhead:** [knotrel-benchmarks `results/metrics-overhead-20261115`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/metrics-overhead-20261115/README.md)
  (M6.5). The release-candidate re-run is
  `results/metrics-overhead-rc-62c2cea-r2`.

**Publication status:** the qualification bundles, the release-candidate
metrics re-run and the decision-analysis tooling are kept as local evidence.
They are not yet published in knotrel-benchmarks.
