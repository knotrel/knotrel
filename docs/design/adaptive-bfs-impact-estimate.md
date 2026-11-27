# Adaptive bidirectional BFS: impact estimate protocol (single candidate)

**Status: APPROVED BY THE MAINTAINER (2026-11-27), FIXED BEFORE IMPLEMENTATION AND
MEASUREMENT.**

- Approved with **R10 deciding** (C2 covers R1–R10), a single candidate, H = 2, the
  60-minute cap and criteria C1–C4.
- The maintainer's precisions are folded in: the list of native non-conclusive classes
  (section 4), the R10 reporting (section 3.1), the outcome when the budget runs out
  (section 5), and the C4 reporting (section 4).
- The rule, thresholds, cells, criteria, block order and budget below were fixed before
  the candidate was implemented or measured.

**Kind.** Impact estimate under the per-release measurement policy (#186). It decides
whether one candidate replaces the #196 search on `main`. The v1.2.0 release verdict stays
with #182: candidate 9faf168 stays not qualified, and attempt 1 stays valid.

**Background.** Three earlier results set the problem:

- **[#182 diagnosis](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.2.0-qualification/diagnosis/REPORT.md).**
  On path-like graphs the #196 search does the same work as v1.1.0 but pays 1.5–3× per
  adjacency entry for per-layer bookkeeping.
- **[#203 estimate](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.2.0-bfs-layer-fix/REPORT.md),
  REJECT.** Leaner bookkeeping helped. But two things failed:
  - dropping degree-based balancing made hub queries 42–55× slower;
  - alternating on ties doubled disconnected work on path pieces.
  - Even the improved path-like cells stayed 1.2–1.7× v1.1.0.
- **This candidate:** one search that behaves like v1.1.0 while the two frontiers stay
  balanced, and like #196 when they are not.

## 1. The rule

### 1.1 Pseudocode

```text
connected(a, b):                              # a, b: vertex indices of existing nodes
  if a == b: return true
  if deg(a) == 0 or deg(b) == 0: return false # an isolated endpoint, distinct from the other
  marks := 2-bit array over all vertices, UNMARKED; marks[a] := S; marks[b] := T
  side[S] := { queue: [a], cursor: 0, pending: deg(a) }
  side[T] := { queue: [b], cursor: 0, pending: deg(b) }
  active  := S if deg(a) <= deg(b) else T     # start rule; tie -> source
  loop:
    A := side[active]; O := side[other(active)]
    v := A.queue[A.cursor]; A.cursor += 1     # expand ONE vertex (no layers)
    A.pending -= deg(v)
    for n in adj[v]:
      m := marks[n]
      if m == mark(other(active)): return true    # the sides touch
      if m == UNMARKED:
        marks[n] := mark(active); A.queue.push(n); A.pending += deg(n)
    if A.cursor == len(A.queue): return false     # active side exhausted its component
    if A.pending > H * O.pending: active := other(active)   # H = 2, strict
```

### 1.2 Definitions and fixed constants

**Work estimate of a frontier.**
- A side's frontier is the unexpanded part of its queue, `queue[cursor..]`.
- Its estimated work is `pending`: the sum of the degrees of those vertices, which is
  exactly the number of adjacency entries their expansion will examine.
- `pending` is kept incrementally: + the degree of each newly marked vertex, − the degree
  of each expanded vertex. That is one degree read per marked vertex, the read #203 showed
  to be load-bearing.

**Narrow frontier.** The active frontier counts as narrow, and the active side keeps
expanding, while `A.pending ≤ H · O.pending`, with **H = 2**.
- There is no width threshold in vertices and no layer bookkeeping.
- The test runs once per expanded vertex: one comparison.

**Switching.**
- The side changes only when `A.pending > 2 · O.pending`.
- After a switch, the new active side has at most half the cost of the old one, and it
  switches back only when its cost exceeds twice the other's. So consecutive switch
  thresholds are a factor 4 apart, which rules out ping-pong.

**Equal costs (ties).**
- At the start, `deg(a) = deg(b)` gives the source, as in v1.1.0 and #196.
- During the search, equality, or anything within the factor 2, keeps the current side
  ("sticky"). Sides never alternate on ties.

**Other fixed choices.**
- **Marks:** two bits per vertex in one allocation, from #203: one load tells unmarked,
  ours or theirs.
- **Queues:** FIFO with a cursor.

**Unchanged:**
- answers, errors, and the O(V + E) worst-case bound (each vertex is marked at most once);
- per-call private scratch;
- `connected_with_workspace`.

**Correctness argument.**
- An edge between a source-marked and a target-marked vertex is detected by whichever side
  scans it second.
- An exhausted side has scanned every vertex it marked. So its component contains no
  target-marked vertex: the first such vertex on a path would have been detected from its
  scanned predecessor.
- The other side's frontier cost is always ≥ 1: every queued vertex was reached by an edge,
  or is a non-isolated endpoint. So the comparison never divides by zero or compares
  against an empty side.

### 1.3 Expected behavior (fixed before measurement; checked by the diagnostic block D)

| Case | Order | What the rule does | Expected work |
|---|---|---|---|
| **Chain** 0…N−1, query (0, N−1), connected | (0, N−1) | Degrees 1 and 1: start at the source. Active cost stays 2 against 1, and 2 > 2 is false, so it **never switches**. It walks the chain until it scans N−2 and sees N−1 marked T. | ≈ 2N entries, as v1.1.0 (W = 1); 0 switches |
| | (N−1, 0) | Symmetric: starts at N−1. | same |
| **Cycle**, query (v, v+N/2) | either | Degrees ≤ 2: start at the lower degree, else the source. Active cost ≤ 4 against 2, never > 4, so no switch. | as v1.1.0 |
| **Two disconnected segments** (chain cut at k), query (0, N−1) | (0, N−1) | Start at the source; cost 2 against 1, no switch. The source segment [0…k] is exhausted: `false`. | k+1 vertices, as v1.1.0 and as #196 (W = 1). The #203 alternation (≈ 2·min) is avoided |
| | (N−1, 0) | Explores [k+1…N−1]: `false`. | N−1−k vertices, as v1.1.0 reversed |
| **Two equal halves** (components-join-split), disconnected | either | As above: one half explored. | as v1.1.0 |
| **Hub h** (degree d) and **leaf ℓ** (degree 1–3), spoke present | (h, ℓ) | deg ℓ < deg h, so start at the **leaf** (target side). Its adjacency contains h, marked S: `true`. | ≤ 3 entries; v1.1.0 scans h's list until ℓ, up to d |
| | (ℓ, h) | Start at the leaf (source side): `true`. | ≤ 3 entries |
| **Hub and leaf, spoke cut**, leaf on the path | (h, ℓ) | Start at the leaf; it marks its path neighbors (cost about 4–6, against d, no switch). Expanding a neighbor sees h: `true`. | ≈ 6–9 entries. #203 expanded the hub first (≈ d) |
| | (ℓ, h) | Same, from the source side. | same |
| **Hub and isolated leaf** | either | Degree 0: `false` at once. | 0 entries |
| **Low-diameter random graph** (#181 sparse, ring with chords, hub) | either | Frontier costs grow about 3–4× per layer, so the side switches whenever one cost exceeds twice the other: balanced bidirectional growth as in #196, within a factor 2. | close to #196 (possibly up to about 2× its work); far below v1.1.0 |

**Known limit, stated in advance.** When both endpoints sit on narrow pieces and the
source's piece is the large one (disconnected), the rule explores the large piece, as v1.1.0
and #196 do on ties. The ideal of exploring only the smaller piece is not attempted.

**Known risk.** Path-like recovery depends on per-entry cost. The candidate adds, per
marked vertex, one degree read and one `pending` update, plus one comparison per expanded
vertex. If that keeps chains more than 10 % above v1.1.0, recovery fails and the candidate
is rejected.

## 2. Arms and comparisons

| Arm | knotrel | Role |
|---|---|---|
| `v110` | v1.1.0 57c35e8 | **Comparison 1 (recovery):** does the candidate clear the #182 deciding regressions within the approved margins? |
| `bfs196` | 9faf168 (`main`'s search, #196) | **Comparison 2 (gains):** how much of the #181 gain does the candidate keep? |
| `cand` | the frozen candidate commit | candidate |

- Tooling: knotrel-benchmarks main at freeze time.
- Each arm is built against its own knotrel checkout: `cargo +1.100.0 build --release
  --offline`, `--locked` where the lockfile serves it. Otherwise the lockfile derivation
  diff is recorded.
- The V1 harness is the v1.0.0 A harness df635a1e, built against `v110` (B) and `cand` (C)
  through the same lockfile derivation as #182.
- V1 runs `run_p0_aa.py` on two plans from `prepare_bc_qualification.py`, each restricted
  to the 4 core cells (`--phase extension --cells …`). Each plan has 12 pairs per cell
  (3 sessions, 2 B→C and 2 C→B pairs per session). Session seeds: 204110–204112 for the
  first plan, 204120–204122 for the second.
- The frozen per-cell rule is applied to the 24 pairs: `analyze_bc_qualification.
  evaluate_cell` with `analyze_v100q_a.RULES`, `final=True`.
- A core-only plan is used because the full v1.0.0 A plan also runs the five HTTP cells,
  which the change does not reach and which do not fit the budget. The pairing,
  measurement and per-cell rule are the frozen ones.

## 3. Cells, and the load each one protects

### 3.1 Recovery set (comparison 1, against v1.1.0, frozen #182 rules)

| # | Cell | Graph and queries | Load it protects | #182 attempt 1 |
|---|---|---|---|---|
| R1 | K1 `chain-split-rejoin-v1` 10 000 | path; (0, N−1) after cut and after relink | long narrow connected and disconnected queries | REGRESSION [+132, +171] % |
| R2 | K1 `chain-split-rejoin-v1` 100 000 | same | same, large | REGRESSION [+190, +213] % |
| R3 | K1 `cycle-alternatives-v1` 10 000 | cycle; (v, v+N/2) around cuts of v | narrow two-sided frontiers | REGRESSION [+59, +65] % |
| R4 | K1 `cycle-alternatives-v1` 100 000 | same | same, large | REGRESSION [+88, +94] % |
| R5 | K1 `components-join-split-v1` 10 000 | two half-chains joined by one random link; (0, N−1) | disconnected halves of equal size; long connected path through a link | REGRESSION [+45, +53] % |
| R6 | K1 `components-join-split-v1` 100 000 | same | same, large | REGRESSION [+69, +78] % |
| R7 | K1 `sustained-churn-path-v1` 100 000 | toggled path edges; random endpoints | many short path components, mostly disconnected queries | REGRESSION [+52, +55] % |
| R8 | V1 `core_sparse_traversal_10k_compact-bfs_warmed` | chain-split-rejoin 10k through `Graph::connected` directly | as R1, without the `ConnectivityGraph` wrapper | REGRESSION [+92, +121] % |
| R9 | V1 `core_sustained_churn_10k_compact-bfs_warmed` | sustained-churn-path 10k, direct | as R7, small | REGRESSION [+43, +48] % |
| R10 | V1 `core_sparse_traversal_10k_compact-workspace_warmed` | as R8 through `connected_with_workspace` | **not executed by any BFS change** (identical machine code, #182 D3); protects against code-placement effects of re-linking `knotrel-core` | REGRESSION [+7, +18] %; D4 unresolved. **Deciding** (maintainer). Reported with absolute times, ratio and interval against v1.1.0. Block D compares its machine code and address in the two V1 builds, and any layout difference is documented as a diagnostic element. The same adoption rule applies in that case too. |

Also checked against v1.1.0, not part of the recovery count:
- the other 9 K1 cells (hub, dense, redundant, blocks, and sustained-path 10k, which was
  TIMER_LIMITED in attempt 1);
- the V1 cell `core_sustained_churn_10k_compact-workspace_warmed`.

**Frozen rules applied.**

- **K1:** the CF5 regression rule of the #182 matrix.
  - 24 pairs plus 2 warm-up pairs; interval [r(7), r(18)] of the paired log ratios
    (97.73 %); time margin t = 0.10.
  - PASS if hi ≤ ln 1.10; REGRESSION if lo > ln 1.10; INCONCLUSIVE otherwise.
  - TIMER_LIMITED from the matrix's per-cell `limit_ns`.
  - Implemented by calling `analyze_cf5.py`'s classification functions.
- **V1:** the frozen v1.0.0 A rule.
  - n = 24 in two blocks of 12, interval r(7), r(18) of ln(C/B), primary tolerance 0.10.
  - Implemented by calling `analyze_v100q_a.py`'s per-cell functions.
- **Validation of both analyzers before the freeze:** recomputing the attempt-1 classes of
  these cells from the attempt-1 raw data must reproduce them exactly.

### 3.2 Gain set (comparison 2, against #196)

The two hub families are distinct, and #203 moved them in opposite directions.

| # | Cell | Graph and queries | Load it protects | #203 against #196 |
|---|---|---|---|---|
| G1 | K1 `hub-alternatives-v1` 10 000 | **hub fan**: vertex 0 adjacent to all, plus a path 1…N−1; queries (0, v) while v's ≤ 3 edges are cut and relinked | **hub-endpoint queries**: one endpoint of degree N−1, the other of degree ≤ 3; connected through the path, or disconnected (isolated v) | **42× slower** (expanded the hub endpoint) |
| G2 | K1 `hub-alternatives-v1` 100 000 | same | same, large | **55× slower** |
| G3 | #181 `hub` 10 000 | **ring with hubs**: a ring plus ⌈N/2000⌉ hubs of 2000 random spokes; uniform random pairs, query-heavy lifecycle trace | **random-pair queries in a small-world graph whose hubs are interior**, rarely endpoints | 15 % faster |
| G4 | #181 `hub` 100 000 | same | same, large | 36 % faster |
| G5 | #181 `sparse` 10 000 | random graph with 2N edges; random pairs | giant component (meeting in the middle), plus small components (small-to-large disconnected) | neutral |
| G6 | #181 `sparse` 100 000 | same | same, large | neutral |
| G7 | #181 `ring` 10 000 | ring plus one random chord per 64 vertices; random pairs | small-world, degree ≤ 3 | neutral |
| G8 | #181 `ring` 100 000 | same | same, large | neutral |
| G9 | K1 `dense-bridge-churn-v1` 512 | two cliques, one toggled bridge | dense frontiers; disconnected across cliques | 16 % faster |
| G10 | K1 `dense-bridge-churn-v1` 2048 | same | same, large | 20 % faster |
| G11 | K1 `redundant-bridge-churn-v1` 512 | two cliques, two bridges | dense frontiers; connected through redundancy | 15 % faster |
| G12 | K1 `redundant-bridge-churn-v1` 2048 | same | same, large | 20 % faster |

- **Guard only:** K1 `sustained-churn-blocks-v1` 10 000 and 100 000 (blocks of ≤ 16
  vertices on a backbone: medium-width frontiers). #203 was 1.13–1.20× slower there.
- **#181 cells:** `tools/lifecycle-probes` `core-lifecycle pairs --exp ops --mix query`,
  engine `compact-bfs`, window 4,000, seed 5,000,001, `--pairs 2 --warmup 1`, as in #181.

**Reference absolute times** (medians from #203 and #181, for orientation only; not used in
any criterion):

| Cell | v1.1.0 | #196 |
|---|---|---|
| R2 chain 100k | 38.7 ms | 103.4 ms |
| R4 cycle 100k | 62.2 ms | 115.5 ms |
| G2 K1 hub 100k | 208.7 ms | 1.6 ms |
| G6 #181 sparse 100k (window) | 2,340 ms | 18.0 ms |
| G4 #181 hub 100k (window) | 1,469 ms | 15.5 ms |

## 4. Adoption criteria (fixed now)

**ADOPT** only if C1 to C4 all hold.

**C1. Correctness.**
- Before the freeze:
  - the workspace tests pass, including the all-pairs and random-history differential
    tests of #196 and #203, plus a new test of the rule's cases in section 1.3, both
    endpoint orders, against `ReferenceGraph`;
  - Clippy, Rustdoc, core MSRV and the full CI matrix pass.
- During the runs:
  - every harness run exits 0, with matching trace fingerprints across arms;
  - every probe verify record matches;
  - the feeder reports no query error.

**C2. Recovery against v1.1.0** (comparison 1, section 3.1 rules).
- **R1–R10: none is REGRESSION, at most one cell in total is non-conclusive (disclosed),
  and the rest are PASS.**
- The 9 other K1 cells: none is REGRESSION.

**Native non-conclusive classes**, fixed before the freeze. Every verdict is reported under
its analyzer's original name.

- **K1** (`analyze_cf5.py`, the regression rule of `classify_paired`):
  - `PASS` and `REGRESSION` are conclusive;
  - **non-conclusive:** `INCONCLUSIVE`, `TIMER_LIMITED` (reported with
    `class_without_limit`), and `INCOMPLETE` (fewer pairs than the interval needs).
  - A cell also counts as non-conclusive under the analyzer's other no-evidence states,
    `NOT_RUN`, `FAILED` and `UNDEFINED`.
- **V1** (`analyze_bc_qualification.evaluate_cell`, final, n = 24):
  - `PASS` and `REGRESSION` are conclusive. An `INCONCLUSIVE` primary whose median exceeds
    the tolerance is `REGRESSION` by the frozen rule;
  - **non-conclusive:** `INCONCLUSIVE_DISCLOSED` (primary inconclusive, median within the
    tolerance, every gated supporting metric `PASS`) and `UNRESOLVED` (an inconclusive
    gated supporting metric).
  - A plain `INCONCLUSIVE` cannot be a final class at n = 24: the analyzer refuses it.
- **Missing data** (a cell not run, or an analyzer error) counts as non-conclusive. If it
  stems from the budget, the outcome is `INCOMPLETE` (section 5).

**C3. Gains kept against #196** (comparison 2).
- For each of G1–G12, from the medians, compute the **gain retained**
  ρ = ln(t_v110 / t_cand) / ln(t_v110 / t_bfs196).
  - 1 means the whole #196 gain; 0 means v1.1.0's time.
- **Each of G1–G12 needs ρ ≥ 0.80, and `cand` FASTER than v1.1.0** (r-interval upper bound
  of cand/v110 < 0.90).
- The count of cells meeting it is reported.
- Guard cells (blocks): not REGRESSION against v1.1.0, as in C2.

**C4. Feeder guard** (block F, only if F runs).
- **Criterion:** the candidate's median L2 G100k C=16 catch-up share is ≥ 0.90 ×
  `bfs196`'s.
- **Reported separately** (not deciding):
  1. **Feeder recovery kept against #196:**
     (share_cand − share_v110) / (share_bfs196 − share_v110), where each C=16 share uses
     its own arm's C=0 median.
  2. **Distance from the Q3 threshold:** share_cand − 50 % at C=16, in percentage points.
     The C=64 share is also reported against 25 %.
- **No Q verdict is issued here.** The release verdict is #182's.

**Always reported** (not deciding):
- absolute medians for all three arms;
- every paired ratio's median and interval: cand/v110, cand/bfs196, bfs196/v110;
- the per-query-kind totals (`query_true`, `query_false`).

## 5. Blocks, order, stop rule and budget (fixed now)

| Order | Block | Content | Arms | Host-time budget |
|---|---|---|---|---|
| 0 | Pre-freeze (no timing) | implementation; C1 tests and CI; analyzer validation (section 3.1); freeze commit (candidate SHA, tooling SHA, SHA-256 of this file) | — | — |
| 1 | **K** (core, decisive) | all 16 K1 cells; 24 + 2 pairs; arm order rotating per pair (`v110 bfs196 cand`, `bfs196 cand v110`, `cand v110 bfs196`) | 3 | 20 min |
| 2 | **V** (core, decisive) | the 4 V1 core cells (R8, R9, R10 and sustained-churn compact-workspace); 24 pairs in 2 blocks of 12; alternating order | v110 (B), cand (C) | 5 min |
| 3 | **L** (core, decisive) | G3–G8; 6 blocks, arm order rotating | 3 | 10 min |
| — | **Stop check** | Evaluate C1–C3. If REJECT is already certain, block F is **skipped by protocol**, and that is recorded. | — | — |
| 4 | **F** (feeder) | load tool (from the `bfs196` checkout, the same for every arm), `LOAD_ENGINE=compact-bfs`, cells L2 G100k C=0, 16, 64; 3 observations per arm, arm order rotating; arms differ only in `KNOTREL_BIN`; Docker only in this block | v110, bfs196, cand | 20 min |
| 5 | **D** (diagnostic, untimed, not deciding) | work counts with a line-for-line model of the frozen candidate on the K1 traces, plus hub-leaf traces in both endpoint orders: vertices expanded, entries examined, switches per query, checked against section 1.3. Also the machine code and addresses of `connected_with_workspace` in the two V1 builds (R10 layout) | — | 5 min |

**Total cap: 60 minutes of host time** for blocks 1–5, excluding builds.
- If the cap is reached before the data needed to adopt are available, the outcome is
  **INCOMPLETE**: no adoption, and no second session. Needed data: blocks K, V and L, and
  F unless the core blocks already decide REJECT.
- If the core blocks already determine REJECT, the attempt is closed as REJECT, and F is
  recorded as skipped by protocol.
- **Host conditions:** AC power, `caffeinate`, no other build or benchmark, Docker stopped
  outside F. Swap counters are recorded before and after; any swap activity makes the
  affected block nonconforming, and it is reported.
- Thresholds are not changed after the freeze. Every observation, failed, invalid or
  inconclusive, is kept.

## 6. Outcomes and what follows

- **ADOPT.** The candidate is merged to `main` and its core path enters the scope of the
  next #182 qualification, on a defined candidate. The release verdict remains #182's.
- **REJECT** (or INCOMPLETE).
  - The candidate is closed as "no change".
  - **This line of BFS experiments is closed.** The maintainer gets a choice between:
    1. restoring the v1.1.0 BFS. This clears the K1 and V1 path regressions by
       construction. It gives up the #181 gains: the feeder C=16 share returns to about
       15 %, and the K1 hub and bridge gains go away.
    2. reducing the v1.2.0 scope, to release without the BFS change or under a disposition.
  - B2 on #87 stays open in both cases.

## 7. Outputs

knotrel-benchmarks `results/v1.2.0-adaptive-bfs/`:
- raw records, build hashes and lockfile derivation diffs;
- host notes and the run log;
- analysis files and `REPORT.md`, with absolute times, ratios and intervals for every cell.

Comments on #182 and #87.
