# Bidirectional BFS for `compact-bfs` queries: impact estimate protocol (#181)

**Status: FIXED BEFORE MEASUREMENT (2026-11-26).** Written and committed before
any before/after observation of this change. Criteria were set by Claude under
the maintainer's delegation for the v1.2.0 plan; the maintainer can reject the
adoption whatever the outcome. Under the
[per-release measurement policy](https://github.com/knotrel/knotrel-benchmarks/blob/main/docs/methodology.md#per-release-measurement-policy)
this is an **impact estimate**: it decides adoption only and does not qualify
v1.2.0. If adopted, the core query path enters the scope of the v1.2.0 release
qualification ([#182](https://github.com/knotrel/knotrel/issues/182)).

## Change

`knotrel_core::Graph::connected` (the `compact-bfs` engine query, also used by
`LifecycleGraph` with that engine) changes from a one-directional BFS to a
bidirectional BFS: one frontier per endpoint, expanded layer by layer, the side
with fewer pending incidences first, stopping when the sides touch or either is
exhausted. Answers, errors, the O(V + E) worst-case bound and the per-call
private scratch are unchanged; scratch grows from one to two V-bit sets.
`Graph::connected_with_workspace` is not changed.

Selected by the H1 profiling
([knotrel-benchmarks `results/v1.2.0-h1-profiling/REPORT.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.2.0-h1-profiling/REPORT.md),
rank 1: `Graph::connected` is 84–96 % of on-CPU server samples under query load).

**Known risk, stated in advance.** For a disconnected pair the search stops when
the smaller side is exhausted, but it may first explore up to about as many
vertices on the other side, so a negative query from a small component into a
large one can cost up to about twice what the one-directional search spent when
started from the small side. A positive query on a long path costs about the same.

## Arms

- **Base:** `origin/main` at the commit this branch starts from (`cd57816`).
- **Candidate:** this branch, the same commit plus only this change.
- Both built with `cargo build --release --locked` (release profile, no debug
  information overrides), on the same host, in the same session.

## Cells

**A. Core query cost (deciding for regression and benefit).**
`tools/lifecycle-probes` `core-lifecycle pairs --exp ops --mix query` from
knotrel-benchmarks `8a47e5ca` (the B1 query-mix cells), built once against each
arm's `knotrel-core`. Engine `compact-bfs`; topologies `sparse`, `ring`, `hub`;
nodes 10,000 and 100,000; window 4,000; seed 5,000,001; `--pairs 2 --warmup 1`
per process. Six cells.

- **Statistic per process run:** the median over its pairs of the lifecycle
  arm's window time (`candidate_ns` in the probe's records), which executes
  `LifecycleGraph::connected` on `compact-bfs`.
- **Runs:** 6 runs per arm per cell, as 6 blocks; within block *k* the arm
  order is base-candidate for odd *k*, candidate-base for even *k*; cells in a
  fixed order per block.
- **Estimand:** the median over blocks of the paired ratio candidate / base.
- **Interval:** [min, max] of the 6 paired ratios, a distribution-free interval
  for the median ratio with coverage 1 − 2 / 2⁶ = 96.9 %.
- **Verdict per cell:** FASTER if max < 0.90; SLOWER if min > 1.10; NEUTRAL if
  the interval lies inside [0.90, 1.10]; INCONCLUSIVE otherwise.
- The probe's untimed answer check must agree in every run (any disagreement is
  a correctness failure, not a performance result).

**B. Feeder under query load (deciding for benefit, descriptive otherwise).**
The load tool of `integrations/postgres-feeder` (`examples/load.rs`) at the base
commit, unchanged protocol settings, `LOAD_ENGINE=compact-bfs`, default
window (60 s) and backlog (100,000). Cells L2 G100k `C` = 0, 16, 64 and L1
G100k `C` = 16. 3 observations per arm per cell, arms alternating per
repetition (base first in repetitions 1 and 3). Reported: catch-up rate and its
share of the same arm's `C`=0 median, time to `ready`, marker lag p50/p99, query
throughput and p50/p99, query batch execution and feeder lock wait from the
phase histograms. Q1–Q5 are reported descriptively against their unchanged
thresholds; **no Q verdict is issued here** (the v1.2.0 Q3 verdict belongs to
#182).

**Host conditions.** As in previous campaigns: AC power, no build or other
benchmark running, swap counters (`vm_stat` Swapins/Swapouts) recorded before
the first and after the last observation. Uncontrolled background activity is
recorded, not removed.

## Decision rule (adoption)

Adopt the candidate only if **all** hold:

1. **Correctness:** workspace tests (including the all-pairs differential test
   added with the change), Clippy, Rustdoc and the core MSRV job pass; every
   probe answer check agrees; the load tool reports no query error and Q2-style
   final-state agreement in every observation.
2. **No regression in A:** no cell is SLOWER; at most one cell is INCONCLUSIVE,
   and it is disclosed.
3. **Benefit:** at least one of
   - two or more of the 100,000-node cells in A are FASTER, or
   - in B, the candidate's median L2 G100k `C`=16 catch-up share is at least
     1.2 times the base's, with its median query batch execution lower.

Otherwise the change is rejected, and the result is recorded as "no change"
(a valid H3 outcome). Thresholds are not changed after collection. Failed,
invalid and inconclusive observations are kept.

## Outputs

Raw records, build hashes, host notes and the analysis go to knotrel-benchmarks
`results/v1.2.0-h3-bidirectional-bfs/`, with checksums. The decision is recorded
in #181.
