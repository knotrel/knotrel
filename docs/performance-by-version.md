# Performance by version

This page summarizes, for users, what the release qualification campaigns
measured when comparing each Knotrel release with the release before it. It is
a brief extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks) and the
release qualification bundles. The release notes remain the authoritative
statement for each release.

**Short version:**
- **v0.5.0 (unreleased candidate) is not demonstrated to be faster than
  v0.4.0.** With lifecycle off, no workload regressed; two results are
  unresolved and accepted by maintainer waivers. See
  [v0.5.0 compared with v0.4.0](#v050-candidate-compared-with-v040).
- **The opt-in v0.5.0 lifecycle has measured costs,** including two flagged
  import regressions and heap growth under churn with fresh node IDs, accepted
  by maintainer waivers for v0.5.0 only. See
  [lifecycle cost](#v050-lifecycle-cost-opt-in-not-release-to-release).
- **v0.4.0 is not demonstrated to be faster than v0.3.0.**
- Most measured server and core workloads stayed within the approved
  regression tolerances.
- One embedded-library workload regressed: it was 28.9% slower. See
  [v0.4.0 compared with v0.3.0](#v040-compared-with-v030).
- Two tail-latency results are unresolved.
- **v0.3.0 is not demonstrated to be faster than v0.2.0** either. It had no
  regression beyond tolerance and two inconclusive cells.

## How to read these numbers

- **Each release is compared with its immediate predecessor,** in one paired
  campaign. Every pair launches both versions back to back in fresh processes,
  in balanced order.
- **The figure** is the median paired change `exp(median(ln(new/old))) − 1`.
  Positive means the new release is slower or uses more memory.
- **The interval** is an order-statistic interval on the paired log ratios.
  It is not a significance test.
- **Tolerances:** the primary metric is 10%, p99 latency 25% and memory 20%.
- **PASS** means the interval's upper bound is within the tolerance. It is a
  regression decision, **not proof of a speedup or of no change**.
- **INCONCLUSIVE** and **UNRESOLVED** mean the data cannot rule out a
  regression beyond the tolerance.
- **Do not chain releases.** Campaigns ran on different days with different
  binaries, so v0.2.0 → v0.4.0 cannot be computed by multiplying the two
  columns.
- **Limits of every number here:**
  - one Apple M3 host on macOS, with Rust 1.100.0 and locally built release
    binaries;
  - the default server configuration with the `compact-bfs` engine, logs to
    `/dev/null`, and `/metrics` off;
  - nine fixed workloads.

  Other hosts, engines, configurations and CI-built binaries were not
  measured.
- **v0.5.0 is a candidate.** Its column describes the measured candidate
  `dcd8ccc`; the page will be revised if the released commit's executed code
  differs.

## Release-to-release results

Primary metric per cell, median paired change [interval] and outcome:

| Workload | v0.3.0 vs v0.2.0 | v0.4.0 vs v0.3.0 | v0.5.0 (candidate) vs v0.4.0 |
|---|---|---|---|
| Core sparse traversal, `compact-bfs` | +0.10% [−21.95%, +6.14%] PASS | +8.71% [−2.34%, +31.89%] **UNRESOLVED**¹ | −7.23% [−33.47%, +2.83%] PASS |
| Core sparse traversal, `compact-workspace` | −2.26% [−11.13%, +9.41%] PASS | **+28.90% [+15.44%, +35.73%] REGRESSION**² | −1.94% [−9.87%, +6.00%] PASS⁴ |
| Core sustained churn, `compact-bfs` | −0.01% [−1.76%, +8.01%] PASS | +1.08% [−1.72%, +5.18%] PASS | −1.02% [−3.67%, +0.64%] PASS |
| Core sustained churn, `compact-workspace` | −0.46% [−5.30%, +0.76%] PASS | +0.57% [−0.40%, +2.52%] PASS | +0.19% [−1.38%, +2.28%] PASS |
| HTTP batch, size 1 | −0.71% [−7.37%, +7.69%] PASS | +5.89% [−3.34%, +8.52%] **UNRESOLVED**³ | +0.04% [−5.57%, +5.32%] **UNRESOLVED**⁵ |
| HTTP batch, size 8 | −1.42% [−15.63%, +12.09%] INCONCLUSIVE | +1.47% [−7.64%, +14.12%] INCONCLUSIVE | −0.42% [−5.05%, +8.17%] PASS |
| HTTP batch, size 64 | −0.83% [−7.56%, +5.08%] PASS | +1.10% [−3.83%, +9.68%] PASS | −2.11% [−4.01%, +1.96%] **UNRESOLVED**⁶ |
| HTTP batch, size 256 | +0.08% [−1.55%, +6.98%] PASS | −2.29% [−8.98%, +4.93%] PASS | −1.72% [−3.13%, +6.84%] PASS |
| HTTP lifecycle, time to ready | +8.02% [−11.46%, +21.97%] INCONCLUSIVE | −12.86% [−21.83%, +23.21%] INCONCLUSIVE | +5.14% [−17.35%, +22.13%] INCONCLUSIVE |

¹ The primary median is within tolerance, but the connectivity-query p99 is
unresolved: +2.02% [−4.97%, +28.53%].

² Library API `Graph::connected_with_workspace` only. The server does not use
it. See [What this means for you](#what-this-means-for-you).

³ The primary metric passes. The request p99 is unresolved: −0.11% [−33.54%,
+57.62%].

⁴ A PASS against v0.4.0 does not mean that the v0.4.0 regression against
v0.3.0 was recovered: releases cannot be chained (see above).

⁵ The primary metric passes. The request p99 is unresolved: +6.04% [−27.84%,
+50.04%] (119.4 → 132.6 µs).

⁶ The primary metric passes. The sampled maximum RSS is unresolved: +0.11%
[−3.50%, +25.14%] against 20% (6.94 → 7.03 MB).

**The decision rules differ between the columns:**
- **v0.3.0:** classified only the primary metric. A p99 or memory metric could
  block only by proving a regression.
- **v0.4.0:** requires every gated p99 and memory metric to PASS as well. That
  is why two v0.4.0 cells are UNRESOLVED even though their primary metric
  median is within tolerance.
- **v0.5.0:** uses the v0.4.0 design and rules on the same nine workloads.

Outcomes in the v0.3.0 column are therefore less strict than in the other
two.

**Other differences between the campaigns:**
- **v0.3.0:** measured candidate `6a1267f` against a Rust 1.100.0 rebuild of
  v0.2.0.
- **v0.4.0:** measured candidate `62c2cea` against v0.3.0 rebuilt from its tag.
  The v0.4.0 release commit's artifacts were verified to contain the same
  executed code.
- **v0.5.0:** measured candidate `dcd8ccc` against v0.4.0 rebuilt from its tag
  (`556683c`), with lifecycle disabled. The `main` commits checked since then
  contain the same executed code in release builds.

### v0.5.0 (candidate) compared with v0.4.0

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

### v0.4.0 compared with v0.3.0

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

### v0.3.0 compared with v0.2.0

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

## What this means for you

**v0.5.0 (unreleased candidate):**

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

- **v0.5.0 (candidate):** [draft release notes](releases/v0.5.0-release-notes.md#performance-qualification)
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
