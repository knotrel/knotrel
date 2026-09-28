# First optimized connectivity engine

Status: **Proposed**, 2026-09-26. This ADR recommends a next milestone; it does not
approve or implement a new engine. Keep the synchronous BFS as a reference.

## Evidence and objective

The [local benchmark baseline](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-26-macos/README.md)
measures four sparse synthetic families, 128–4096 vertices, 200 rounds, one warmup
and three repetitions on an Apple M3. Queries consume 98.29%–99.95% of summed
operation time; setup is excluded. At 4096 vertices, median per-run query p50s
span 115–277 µs. This motivates avoiding graph traversal per query. It does not
establish an optimized engine's crossover point or commercial advantage.

## Options

Here n is stored vertices (including isolated ones) and m is live edges.
Bounds describe candidate designs, not measured implementations.

| Option | Updates and queries | Replacement search, memory and implementation cost |
| --- | --- | --- |
| Existing ordered-adjacency BFS | O(log n) worst-case link/cut; O((n+m) log n) worst-case query | No maintained forest; O(n+m) graph storage and O(n) query scratch. Lowest complexity; retain as oracle/reference. |
| Eager component relabeling | Rebuild labels after each changed update: O((n+m) log n) worst case with ordered maps; O(log n) query by ID lookup | A full traversal discovers replacement paths implicitly. O(n+m) storage; low complexity, but shifts almost all cost onto updates in these 50%-query traces. These bounds follow directly from the proposed rebuild procedure. |
| Single Euler-tour forest plus exhaustive replacement search | Balanced-tree forest primitives and queries O(log n) worst case; a tree-edge deletion can cost O((n+m) log n) worst case | Inspect incident non-tree edges of the smaller split tree until a crossing edge is found. O(n+m) storage; medium complexity. Simple exact stepping stone, but repeated failed scans lack a polylogarithmic amortized guarantee. |
| Holm–de Lichtenberg–Thorup (HDT) | O(log² n) amortized updates; binary balanced Euler-tour trees give O(log n) worst-case queries | Edge levels and smaller-side promotion charge repeated searches to level increases. O(m+n log n) storage; highest implementation/invariant burden among these candidates. |

HDT's bounds and level-search structure come from [Holm, de Lichtenberg and
Thorup, JACM 2001, §§2–3](https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf).
The paper also obtains O(log n/log log n) worst-case queries using a higher-arity
forest representation; that refinement is not part of this initial proposal.
Euler-tour forest operations alone do not solve general-graph replacement search.
Deterministic balanced trees matter: substituting randomized treaps changes the
relevant forest bounds to expected bounds. Amortized updates do not bound each
individual deletion or p99 latency.

## Recommendation

Prototype **deterministic HDT with binary balanced Euler-tour forests** as the
first optimized candidate. This is an engineering recommendation: unlike a single
forest scan, it addresses repeated replacement searches; unlike eager relabeling,
it avoids a full traversal after every change. Accept the extra memory and
implementation complexity only if subsequent measurements justify them.

Build and test the forest kernel first; an exhaustive exact replacement scan can
serve as a temporary integration step. Do not label that step HDT or attach HDT
bounds until level invariants and promotion accounting are implemented. Preserve
simple undirected/idempotent semantics, all u64 IDs, isolated vertices and errors.
Map IDs to stable internal handles. The paper assumes a fixed vertex universe;
choose and document a capacity-growth/rebuilding strategy before claiming its
bounds for Knotrel's dynamically added vertices. Do not silently renumber or drop
isolated nodes.

Randomized Las Vegas structures are an alternative, not inherently inexact:
[Huang et al.](https://arxiv.org/abs/1609.05867) give expected amortized
O(log n(log log n)²) updates. Defer them because implementing and validating their
additional machinery is not justified by this small baseline. This is a scope
choice, not a claim of measured inferiority.

## Acceptance evidence for the next milestone

Differential-test arbitrary update histories against the matrix oracle and BFS,
including bridges, replacement paths, repeated deletes, isolated/max-u64 IDs and
vertex-capacity growth. Check forest/level invariants after every test mutation.
Replay the saved traces unchanged and report per-operation latency, full loading
cost, memory (separating harness overhead), and replacement-scan/promotion counts.
Add dense, long-lived churn and adversarial traces before interpreting scaling;
this suite mainly restores a small local change each round.

Keep results even if the candidate loses at small sizes. GraphScope requires a
real adapter with identical exact visibility and boolean-query semantics, plus
loading, update and state-maintenance costs. No comparison or market claim is
supported by beating our BFS alone.
