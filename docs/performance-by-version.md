# Performance by version

This page summarizes, for users, what the release qualification campaigns
measured when comparing each Knotrel release with the release before it. It is
a brief extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks) and the
release qualification bundles. The release notes remain the authoritative
statement for each release.

**Short version:**
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

## Release-to-release results

Primary metric per cell, median paired change [interval] and outcome:

| Workload | v0.3.0 vs v0.2.0 | v0.4.0 vs v0.3.0 |
|---|---|---|
| Core sparse traversal, `compact-bfs` | +0.10% [−21.95%, +6.14%] PASS | +8.71% [−2.34%, +31.89%] **UNRESOLVED**¹ |
| Core sparse traversal, `compact-workspace` | −2.26% [−11.13%, +9.41%] PASS | **+28.90% [+15.44%, +35.73%] REGRESSION**² |
| Core sustained churn, `compact-bfs` | −0.01% [−1.76%, +8.01%] PASS | +1.08% [−1.72%, +5.18%] PASS |
| Core sustained churn, `compact-workspace` | −0.46% [−5.30%, +0.76%] PASS | +0.57% [−0.40%, +2.52%] PASS |
| HTTP batch, size 1 | −0.71% [−7.37%, +7.69%] PASS | +5.89% [−3.34%, +8.52%] **UNRESOLVED**³ |
| HTTP batch, size 8 | −1.42% [−15.63%, +12.09%] INCONCLUSIVE | +1.47% [−7.64%, +14.12%] INCONCLUSIVE |
| HTTP batch, size 64 | −0.83% [−7.56%, +5.08%] PASS | +1.10% [−3.83%, +9.68%] PASS |
| HTTP batch, size 256 | +0.08% [−1.55%, +6.98%] PASS | −2.29% [−8.98%, +4.93%] PASS |
| HTTP lifecycle, time to ready | +8.02% [−11.46%, +21.97%] INCONCLUSIVE | −12.86% [−21.83%, +23.21%] INCONCLUSIVE |

¹ The primary median is within tolerance, but the connectivity-query p99 is
unresolved: +2.02% [−4.97%, +28.53%].

² Library API `Graph::connected_with_workspace` only. The server does not use
it. See [What this means for you](#what-this-means-for-you).

³ The primary metric passes. The request p99 is unresolved: −0.11% [−33.54%,
+57.62%].

**The decision rules differ between the two columns:**
- **v0.3.0:** classified only the primary metric. A p99 or memory metric could
  block only by proving a regression.
- **v0.4.0:** requires every gated p99 and memory metric to PASS as well. That
  is why two v0.4.0 cells are UNRESOLVED even though their primary metric
  median is within tolerance.

Outcomes in the two columns are therefore not equally strict.

**Other differences between the campaigns:**
- **v0.3.0:** measured candidate `6a1267f` against a Rust 1.100.0 rebuild of
  v0.2.0.
- **v0.4.0:** measured candidate `62c2cea` against v0.3.0 rebuilt from its tag.
  The v0.4.0 release commit's artifacts were verified to contain the same
  executed code.

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

| If you… | What the evidence supports |
|---|---|
| Run the server with defaults (`compact-bfs`) | Batch throughput windows at sizes 1, 64 and 256 stayed within the 10% tolerance against v0.3.0. Batch size 8 and startup readiness are inconclusive. The p99 for size-1 batches, and the core connectivity-query p99, are not bounded at 25% |
| Embed `knotrel-core` and call `Graph::connected_with_workspace` | A 28.9% slower result was measured in the benchmark binary. The size and direction in your binary are unknown. Measure your own workload before upgrading |
| Embed `knotrel-core` and call `Graph::connected` / `ConnectivityGraph` with the default engine | Throughput is within tolerance in sustained churn. Sparse traversal and its tail latency are unresolved |
| Enable `/metrics` | Overhead stayed within the approved budget in-process |
| Use ETT or HDT | Not qualified release to release |
| Expect v0.4.0 to be faster overall | Not supported by the evidence |

## Sources

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
