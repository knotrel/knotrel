# Performance by version

This page summarizes, for users, what the release qualification measured when
comparing the current release with the previous minor release. It is a brief
extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks). The
release notes remain the authoritative statement.

Earlier comparisons are kept in the [performance history](performance-history.md).
Each was a separate campaign with its own rules, references and binaries, so
those results cannot be compared with this page or chained into a trend.

## v0.8.0: not qualified

v0.8.0 (unreleased) is released **without** a performance qualification
against v0.7.0, by maintainer decision. No performance statement is made for
it. Its new server features are opt-in; the targeted cost check of the
inspection routes is in [console-inspection-costs.md](console-inspection-costs.md)
and is not a release qualification. Details and limits:
[v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification).
The comparison below remains the latest qualified one.

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

## How to read these numbers

- **Paired runs.** Every pair launches both versions back to back in fresh
  processes, in balanced order.
- **The figure** is the median paired change `exp(median(ln(new/old))) − 1`.
  Positive means v0.7.0 is slower or uses more memory.
- **The interval** is an order-statistic interval on the paired log ratios.
  It is not a significance test.
- **PASS** means the interval's upper bound is within the tolerance. It is a
  regression decision, **not proof of a speedup or of no change**.
- **INCONCLUSIVE** and **UNRESOLVED** mean the data cannot rule out a
  regression beyond the tolerance.
- **Limits:**
  - one Apple M3 host on macOS, with Rust 1.100.0 and locally built release
    binaries;
  - the default server configuration with the `compact-bfs` engine, logs to
    `/dev/null`, and `/metrics` off.

  Other hosts, engines, configurations and CI-built binaries were not
  measured.

## Release cells, lifecycle off

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

## Concurrency

Contention cells: 10% throughput tolerance, 25% p99 tolerance.

| Workload | Throughput | p99 | Outcome |
|---|---|---|---|
| Read-heavy (95% queries), size 1, 4 clients, 10k | 15,289 → 32,232 req/s | 0.53 → 0.26 ms | PASS, improved |
| Read-heavy, size 8, 16 clients, 100k | 213 → 350 req/s | 91.1 → 90.7 ms | PASS, improved |
| Mixed 50%, size 64, 16 clients, 100k | 51 → 51 req/s | 346 → 353 ms | PASS |
| Lifecycle 50%, size 8, 16 clients, 100k | 399 → 399 req/s | 54.7 → 54.1 ms | PASS |
| Queries during a source import, 100k | 204 → 1,053 req/s | 92.2 → 77.0 ms | PASS, improved |
| Queries during feeder events, 100k | 206 → 938 req/s | 91.3 → 50.5 ms | PASS, improved |

## Lifecycle on

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

## What this means for you

| If you… | What the evidence supports |
|---|---|
| Run many concurrent read-heavy clients | Query-only requests now run in parallel: about 2× the throughput with 4 clients at 10k nodes, and 4.5–5× for queries during imports or feeder events at 100k nodes, with lower p99 |
| Keep lifecycle off (the default) | No workload regressed against v0.6.0 in the nine release cells |
| Enable lifecycle with fresh-ID churn | Heap growth between compactions is within its rule, and compaction pauses are shorter. RSS can still grow through allocator retention |
| Send large lifecycle batches or feeder events on hub-like graphs | Not qualified against v0.6.0. Indicative medians are within ±7% of non-atomic references |
| Import graphs dominated by inactive entries | Still about 5× the cost of importing only the effective graph, a few ms per 10k-node import on the measured host |
| Expect v0.7.0 to be faster overall | Not supported for write-heavy or mixed workloads, which are unchanged within tolerance |

## Sources

- [v0.7.0 release notes](releases/v0.7.0-release-notes.md#performance-qualification).
- knotrel-benchmarks `results/v0.7.0-qualification-3/DECISIONS.md`, the
  single decision record, with the analyses in
  `results/v0.7.0-qualification-3/analysis/q3-3/`.
- Earlier releases: [performance history](performance-history.md).
