# Performance by version

This page summarizes, for users, what the release qualification measured when
comparing the current release with the latest qualified release. It is a
brief extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks). The
release notes remain the authoritative statement.

Earlier comparisons are kept in the [performance history](performance-history.md).
Each was a separate campaign with its own rules, references and binaries, so
those results cannot be compared with this page or chained into a trend.

## v0.9.0 compared with v0.7.0

**v0.9.0 is not released yet.** These results were measured on its release
candidate; the release notes become authoritative at the tag.

**v0.9.0 is not demonstrated to be faster in general than v0.7.0 or v0.8.0.**
The measured workloads stayed within their tolerances, with the exceptions
below.

Two statements, kept separate:
- **The campaign is valid.** Attempt 2 conformed to the frozen protocol in
  its deciding and descriptive parts, and the feeder measurement completed.
  An earlier attempt did not conform (time budget exhausted) and is not used.
- **The release is accepted with exceptions, for v0.9.0 only.** This is not
  "all gates PASS":
  - five deciding results are not PASS (two release cells, one metrics
    budget, two lifecycle imports) and keep their classification;
  - two lifecycle requirements on a hub topology are not qualified by
    comparison;
  - feeder catch-up under query load (Q3) stays FAIL, a declared limitation;
  - six RSS-retention cells exceed their budget, a disclosure category.

Candidate `e827b2a` against published v0.7.0 (`9f933a7`), 2026-11-22, with
the v0.7.0 qualification-3 method and thresholds unchanged. The comparison
against published v0.8.0 (`e3fc736`) is descriptive. The release commit's
`crates/`, `Cargo.toml` and `Cargo.lock` must be identical to the measured
candidate; checked on `main` at `cd803e8`, and to be checked again on the
final commit.

v0.8.0 (released 2026-11-21) was released without a performance
qualification, by maintainer decision; see the
[v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification).

## How to read these numbers

- **Paired runs.** Every pair launches both versions back to back in fresh
  processes, in balanced order.
- **The figure** is the median paired change `exp(median(ln(new/old))) − 1`.
  Positive means v0.9.0 is slower or uses more memory.
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

| Workload | v0.7.0 → v0.9.0 |
|---|---|
| Core sparse traversal, `compact-bfs` | +5.06% [−2.65%, +13.80%] **INCONCLUSIVE**, accepted |
| Core sparse traversal, `compact-workspace` | −4.18% [−10.33%, +1.33%] PASS |
| Core sustained churn, `compact-bfs` | +1.98% [−3.24%, +7.08%] PASS |
| Core sustained churn, `compact-workspace` | +2.02% [−6.34%, +6.51%] PASS |
| HTTP batch, size 1 | +1.52% [−0.03%, +2.86%] PASS |
| HTTP batch, size 8 | −0.72% [−3.01%, +1.85%] PASS |
| HTTP batch, size 64 | +0.42% [−3.84%, +5.63%] PASS |
| HTTP batch, size 256 | −1.14% [−6.58%, +4.50%]; sampled RSS +1.2% [−2.3%, +23.0%] against 20%: **UNRESOLVED**, accepted |
| HTTP lifecycle, time to ready | −1.79% [−10.61%, +2.67%] PASS |

Both accepted cells were PASS in v0.7.0. Neither is a measured regression;
the data cannot rule one out.

## Concurrency

Contention cells: 10% throughput tolerance, 25% p99 tolerance.

| Workload | Throughput | p99 | Outcome |
|---|---|---|---|
| Read-heavy (95% queries), size 1, 4 clients, 10k | 30,966 → 31,019 req/s | 0.27 → 0.27 ms | PASS |
| Read-heavy, size 8, 16 clients, 100k | 349 → 350 req/s | 94.7 → 95.3 ms | PASS |
| Mixed 50%, size 64, 16 clients, 100k | 51 → 51 req/s | 346 → 346 ms | PASS |
| Lifecycle 50%, size 8, 16 clients, 100k | 397 → 395 req/s | 55.0 → 55.6 ms | PASS |
| Queries during a source import, 100k | 966 → 979 req/s | 82.9 → 82.3 ms | PASS |
| Queries during feeder events, 100k | 868 → 878 req/s | 58.2 → 53.0 ms | PASS, improved (+1.2% throughput) |

## Lifecycle on

- **Flagged imports and churn** (10% tolerance): 9 of 11 PASS. Two core
  imports at 10k (sparse −2.4%, hub −3.3%) are **INCONCLUSIVE**, accepted
  for v0.9.0 only, as in v0.7.0.
- **Memory:** import and churn memory 10/10 PASS.
- **Heap growth under churn (C3):** 15/15 PASS.
- **Compaction pauses (T5)**, against v0.7.0: 6/6 PASS, within ±2.4% (P_max
  at 100k sparse 15.58 → 15.59 ms).
- **Lifecycle cells against capability-equivalent references:** 58 deciding
  timing cells PASS. One is **INCONCLUSIVE**, accepted: metrics overhead on
  lifecycle batches, sparse 10k, +5.3% [+0.9%, +7.8%] against a 6% budget
  (also INCONCLUSIVE in v0.7.0).

**Not qualified, accepted for v0.9.0:** lifecycle batches and feeder events
on the `hub` 10k topology.
- v0.7.0 cannot offer the same atomicity there, so no equivalent reference
  exists.
- Comparisons against references that are not atomic are indicative only:
  +10.8% or −3.5% (batches), +5.0% or −5.9% (events).
- Roadmap: v1.2.0 (H2).

**Disclosed:**
- **RSS retention under churn:** 6 of 15 cells exceed the RSS budget, a
  disclosure category (the same six as in v0.7.0).
- **Imports dominated by inactive entries:**
  - about 5× an import of the effective graph only (+400%, recurring since
    v0.5.0);
  - +55% against a capability-equivalent reference, PASS at 100%.

## Compared with v0.8.0 (descriptive)

No verdict. This covers what changed after v0.8.0, the reserved feeder
admission lanes.
- **Feeder events (six lifecycle cells):** v0.9.0/v0.8.0 median ratios
  between 0.94 and 1.00. The two versions ran unpaired in the same session.
- **Concurrency (six cells):** throughput −1.3% to +0.5%, p99 −5.2% to
  +1.3%, within the tolerances.

On this host, for these cells, the measurements show no change beyond the
tolerances after v0.8.0. They do not cover other workloads, scales or
hosts.

## Feeder under load

[Protocol](integrations/feeder-under-load.md), PostgreSQL 18.7, graph
G100k, loopback.

| Criterion | Measured | Outcome |
|---|---|---|
| Q1 freshness, 16 query clients | p99 148 ms, max 247 ms | PASS |
| Q2 no growing lag, bounded WAL | 12/12 observations | PASS |
| Q3 catch-up with 16 / 64 query clients, share of the no-query rate | 18.0 % / 10.1 % (required 50 % / 25 %) | **FAIL**, declared limitation |
| Q4 time to `ready` with 64 query clients | 5.8 s | PASS |
| Q5 query cost against v0.8.0 | throughput 1.02 / 0.94, p99 0.98 / 0.86 | PASS |

No cause is claimed for Q3. Follow-up:
[knotrel/knotrel#87](https://github.com/knotrel/knotrel/issues/87).

## What this means for you

| If you… | What the evidence supports |
|---|---|
| Upgrade from v0.7.0 or v0.8.0 | The measured release, concurrency and lifecycle workloads stayed within their tolerances, except the accepted exceptions above. No speedup is claimed |
| Keep lifecycle off (the default) | Seven of nine release cells PASS. Core sparse traversal (`compact-bfs`) and the size-256 batch's memory are not bounded at their tolerances |
| Send large lifecycle batches or feeder events on hub-like graphs | Not qualified against v0.7.0. Indicative medians are between −6% and +11% against non-atomic references |
| Run the PostgreSQL feeder with heavy query load | Freshness, lag and recovery passed. Catching up a large backlog is slow with many query clients (Q3 FAIL) |
| Import graphs dominated by inactive entries | Still about 5× the cost of importing only the effective graph |

## Sources

- [v0.9.0 release notes](releases/v0.9.0-release-notes.md#performance-qualification).
- knotrel-benchmarks `results/v0.9.0-qualification/DECISION.md`, the single
  decision record, with the analyses in
  `results/v0.9.0-qualification/analysis/`.
- Earlier releases: [performance history](performance-history.md).
