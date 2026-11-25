# Performance by version

This page summarizes, for users, what the release qualification measured when
comparing the current release with the latest qualified release. It is a
brief extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks). The
release notes remain the authoritative statement.

Earlier comparisons are kept in the [performance history](performance-history.md).
Each was a separate campaign with its own rules, references and binaries, so
those results cannot be compared with this page or chained into a trend.

## v1.0.0 compared with v0.9.0

v1.0.0 was released on 2026-11-24. These results were measured on candidate
`0c49b2a` (version `1.0.0-rc.1`, never published) against the published
v0.9.0 on 2026-11-23, and transferred to 1.0.0 under a maintainer waiver: the
release differs from the candidate only by its version lines. Binary identity
with the measured build is not claimed, and 1.0.0 was not measured again
([waiver](releases/v1.0.0-text-identity-waiver.md)).

**v1.0.0 is not demonstrated to be faster in general than v0.9.0.** No
deciding result is a regression; the exceptions are listed below.

Two statements, kept separate:
- **The campaign is valid.** Part 1 conformed to its time budget (77.6 of
  105 min) and the feeder block completed (27.5 of 30 min).
- **The release is accepted with exceptions, for v1.0.0 only.** This is not
  "all gates PASS":
  - four deciding results are not PASS (one unresolved, three
    inconclusive) and keep their classification;
  - two lifecycle requirements on a hub topology are not qualified by
    comparison;
  - feeder catch-up under query load (Q3) stays FAIL, a declared limitation
    with an operational envelope;
  - six RSS-retention cells exceed their budget, a disclosure category.

The same comparison, with every verdict as measured, is on the
[performance site](https://knotrel.github.io/knotrel-benchmarks/releases/1.0.0.html).

## How to read these numbers

- **Paired runs.** Every pair launches both versions back to back in fresh
  processes, in balanced order.
- **The figure** is the median paired change `exp(median(ln(new/old))) − 1`.
  Positive means v1.0.0 is slower or uses more memory.
- **The interval** is an order-statistic interval on the paired log ratios
  (97.73 % with 24 pairs, 96.14 % with 12). It is not a significance
  test.
- **PASS** means the interval's upper bound is within the tolerance. It is a
  regression decision, **not proof of a speedup or of no change**.
- **INCONCLUSIVE** and **UNRESOLVED** mean the data cannot rule out a
  regression beyond the tolerance.
- **Limits:** one Apple M3 host (16 GB) on macOS, with the server,
  PostgreSQL 18.7 and the load generator on the same machine, locally built
  release binaries and the default `compact-bfs` engine. Other hosts,
  engines, configurations and CI-built binaries were not measured.

## Results

- **PASS:** release cells 8 of 9; core rules B1–B3 37/37; lifecycle
  deciding cells 21 of 22; B5 2/2; lifecycle resource rules C1–C3 105/105;
  compaction pauses T5 6/6; flagged imports and churn 9 of 11 (one PASS
  (IMPROVED)); lifecycle memory 20/20 metrics; concurrency 6/6; feeder Q1, Q2,
  Q4 and Q5.
- **Accepted inconclusive results** (verdicts unchanged):
  - HTTP batch, size 256: **UNRESOLVED**. The primary metric is within
    tolerance; the sampled RSS interval [−7.1%, +22.4%] crosses the 20%
    tolerance. No RSS claim for this cell.
  - Metrics overhead on batch operations: **INCONCLUSIVE**. Enabling metrics
    increased batch-operation time by 5.4%, interval [+1.5%, +6.6%] against a
    6% budget.
  - Core imports with lifecycle, sparse and hub 10k: **INCONCLUSIVE**,
    neutral medians (−1.9%, −0.7%) with wide intervals. No equivalence claim
    at 10k.
- **Not qualified:** lifecycle batches and feeder events on the `hub` 10k
  graph have no equivalent reference; no performance claim.
- **Disclosed:** RSS retention under churn in six cells (C4); imports
  dominated by inactive entries (+402% within the candidate build, as in
  v0.9.0); lifecycle operations on sparse 100k, descriptive INCONCLUSIVE
  (+19.9%, interval up to +25.1% against 25%).

## Feeder under load

[Protocol](integrations/feeder-under-load.md), PostgreSQL 18.7, graph
G100k, loopback.

| Criterion | Outcome |
|---|---|
| Q1 freshness, Q2 no growing lag, Q4 time to `ready` | PASS |
| Q3 catch-up with 16 / 64 query clients, share of the no-query rate | 17.7 % / 9.4 % (required 50 % / 25 %): **FAIL**, declared limitation with an [operational envelope](integrations/feeder-under-load.md#v100-qualification-result-and-operational-envelope-2026-11-23) |
| Q5 query throughput against v0.9.0, 16 / 64 clients | ×1.07 / ×1.01, PASS |

No cause is claimed for Q3. Follow-up:
[knotrel/knotrel#87](https://github.com/knotrel/knotrel/issues/87).

## What this means for you

| If you… | What the evidence supports |
|---|---|
| Upgrade from v0.9.0 | The measured release, concurrency and lifecycle workloads stayed within their tolerances, except the accepted exceptions above. No speedup is claimed |
| Keep lifecycle off (the default) | Eight of nine release cells PASS. The size-256 batch's memory is not bounded at its tolerance |
| Enable `/metrics` on batch-heavy traffic | Overhead measured at about 5%; it is not bounded at the 6% budget |
| Send large lifecycle batches or feeder events on hub-like graphs | Not qualified against v0.9.0 |
| Run the PostgreSQL feeder with heavy query load | Freshness, lag and recovery passed. Catching up a large backlog is slow with many query clients (Q3 FAIL) |

## Engines in the 1.1.0 candidate (not a release comparison)

1.1.0 is not released. Its candidate adds the experimental `cluster-forest`
engine; `compact-bfs` stays the default. The CF5 qualification compared
`cluster-forest` with `compact-bfs`, ETT and HDT **on one build**, not one
release with another, so it is not part of this page. See
[Choosing an engine](choosing-an-engine.md) and the
[engine comparison](https://knotrel.github.io/knotrel-benchmarks/engines/cluster-forest-cf5-02cba68.html).

## Sources

- [v1.0.0 release notes](releases/v1.0.0-release-notes.md#performance-qualification).
- knotrel-benchmarks [`results/v1.0.0-qualification/DECISION.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.0.0-qualification/DECISION.md),
  the decision record.
- Earlier releases: [performance history](performance-history.md).
