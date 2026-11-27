# Bidirectional BFS per-layer fix: impact estimate protocol

**Status: FIXED BEFORE MEASUREMENT (2026-11-27).** Written and committed before any
observation of this change. Authorized by the maintainer: "implement the fix", after the
[#182 BFS regression diagnosis](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.2.0-qualification/diagnosis/REPORT.md).

This is an **impact estimate** under the per-release measurement policy (#186). It decides
whether the fix replaces the current bidirectional search on `main`. It does **not**
qualify v1.2.0: candidate 9faf168 stays not qualified, attempt 1 of #182 stays valid, and
the next full qualification runs on a defined candidate.

## Change

`knotrel_core::Graph::connected` keeps the bidirectional breadth-first search of #196, with
the same answers, errors and O(V + E) bound. The diagnosis isolated three costs, which the
fix removes:

| Cost isolated by the diagnosis | Fix |
|---|---|
| two V-bit sets, two loads per neighbor | **one mark array of two bits per vertex**: one allocation, one load per neighbor tells unmarked, ours or theirs |
| clearing and swapping frontier vectors on every layer | **one FIFO queue per side** with a cursor and a layer end; a layer is a scan of a contiguous slice |
| the degree read of every newly marked vertex (`pending`) | the side with **fewer vertices in its current layer** expands next; ties alternate |

Plus: an isolated endpoint answers `false` at once, keeping the zero-work disconnected
queries of #196. `fixedbitset` moves to `knotrel-core`'s dev-dependencies (tests only).
`connected_with_workspace` is not changed.

## Arms

| Arm | knotrel | Role |
|---|---|---|
| `prev` | 9faf168 (current bidirectional search) | base of the adoption decision |
| `fix` | this branch: main d79438e plus this change | candidate |
| `v110` | v1.1.0 57c35e8 | descriptive reference, against the frozen K1 tolerance |

- The fix arm differs from `prev` only in `knotrel-core`: main after 9faf168 adds
  documentation only.
- Tooling: knotrel-benchmarks main 72473c30, whose harness and probe sources are
  unchanged since 4cf9b103. Each arm is built against its own knotrel checkout with
  `cargo +1.100.0 build --release --offline`, `--locked` where the lockfile serves it.
- For `fix`, the downstream lockfile's `knotrel-core` entry drops `fixedbitset`. The
  derivation diff is recorded, as for the v1.0.0 A harness.

## Cells

### A. K1 workloads (deciding)

All 16 K1 cells of the #182 matrix, with the same arguments: `knotrel-benchmarks` harness,
`--engine connectivity-compact-bfs`, seed 42, `--warmup 1 --repetitions 1`, rounds as in the
matrix. Statistic: `repetitions[0].workload_wall_ns`.

| Set | Cells |
|---|---|
| **Path-like** (the regressed regime) | chain-split-rejoin, cycle-alternatives, components-join-split and sustained-churn-path, each at 10k and 100k |
| **Low-diameter** (the gain regime) | hub-alternatives 10k and 100k; dense-bridge-churn 512 and 2048; redundant-bridge-churn 512 and 2048 |
| **Blocks** | sustained-churn-blocks 10k and 100k |

- **Runs.** 12 blocks. Each block runs every cell once per arm. The order of the three arms
  rotates by block (`prev fix v110`, `fix v110 prev`, `v110 prev fix`, …); cells keep the
  matrix order. The harness's trace fingerprint must match across arms.
- **Estimand per pair of arms:** the median of the 12 paired ratios.
- **Interval:** [r(3), r(10)] of the sorted ratios, a distribution-free interval with
  coverage 1 − 2·P(Bin(12, ½) ≤ 2) = 96.1 %.
- **Verdict, fix against prev:**
  - FASTER if r(10) < 0.90;
  - SLOWER if r(3) > 1.10;
  - NEUTRAL if the interval lies in [0.90, 1.10];
  - INCONCLUSIVE otherwise.

### B. The #181 A cells (deciding guard for the gain regime)

- `tools/lifecycle-probes` `core-lifecycle pairs --exp ops --mix query`, engine
  `compact-bfs`.
- Topologies `sparse`, `ring` and `hub` at 10,000 and 100,000 nodes; window 4,000; seed
  5,000,001; `--pairs 2 --warmup 1`.
- Arms `fix` and `prev` only (v1.1.0 is already measured in #181).
- 6 blocks with alternating arm order, statistic and interval as in #181: the per-process
  median `candidate_ns`, then [min, max] of the 6 paired ratios (96.9 %), with the same
  verdict thresholds.
- Every probe answer check must agree.

### C. Feeder under query load (guard, descriptive)

- The load tool of `integrations/postgres-feeder` (`examples/load`), built at each arm's
  checkout, `LOAD_ENGINE=compact-bfs`, default window and backlog.
- Cells L2 G100k `C` = 0 and 16. Arms `fix` and `prev`, 3 observations each, alternating.
- Reported: catch-up rate and the C=16 share of the same arm's C=0 median.
- **No Q verdict is issued.**

## Decision rule (fix replaces prev)

Adopt only if **all** hold:

1. **Correctness.**
   - Workspace tests (including the new random-history differential test), Clippy,
     Rustdoc and the CI matrix pass.
   - Every harness run exits 0 with matching trace fingerprints, and every probe answer
     check agrees.
2. **Path-like benefit (A).** At least 6 of the 8 path-like cells are FASTER against
   `prev`, and none is SLOWER.
3. **No loss in the gain regime.** No low-diameter or blocks cell of A, and no cell of B,
   is SLOWER against `prev`. At most two cells in total are INCONCLUSIVE, and they are
   disclosed.
4. **Feeder guard (C).** The fix's median L2 C=16 share is at least 0.90 times `prev`'s.

Otherwise the fix is rejected and recorded as "no change"; `main` keeps the #196 search.
Thresholds are not changed after collection, and every observation is kept.

**Reported but not deciding:** `fix` against `v110` per A cell, with its interval, next to
the frozen K1 tolerance (time margin 0.10). This shows which #182 regressions the next
qualification can be expected to clear. It is not a verdict.

## Budget and host

- At most **60 minutes of host time**: A about 15 min, B about 10 min, C about 20 min
  including Docker start.
- Docker runs only during C.
- AC power, `caffeinate`, no other benchmark or build running during A and B.
- Swap counters are recorded before and after; any swap activity is reported.

## Outputs

knotrel-benchmarks `results/v1.2.0-bfs-layer-fix/`: raw records, build hashes, the
lockfile derivation diff, host notes, the analysis and `REPORT.md`. Comments on #182 and
#87.
