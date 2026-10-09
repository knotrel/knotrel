# Experimental deterministic HDT connectivity

Status: Implemented, 2026-09-28. Opt-in `HdtGraph`, server `hdt`, benchmark `hdt`
(`knotrel-core/hdt-sparse-levels-v3`). Compact BFS remains the default. Extends ADR 0007;
the single-forest ETT remains available and unchanged for comparisons.

## Representation and repair

The implementation follows [Holm, de Lichtenberg and Thorup (2001), section 3](https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf).
Each edge has a level. Forest F_i contains tree edges of level at least i.
Exact-level tree/non-tree incidences are stored at both endpoints in separate
ordered sets. Each level has its own AVL Euler-tour forest, whose vertex tokens
carry two independent incidence bits and whose subtrees aggregate those bits.
A marked search descends one AVL path; it never materializes a whole component.
Tree records own directed tour handles at every level through their edge level.
Non-tree records have no tour handles.

Insertions start at level zero. Removing a non-tree edge only updates its
incidences. A tree cut removes its tour occurrences from every containing forest,
then searches downward from its level. At each level, select the smaller side,
first check whether that side has any exact-level non-tree incidence. If not,
skip directly to the next lower level without promoting tree edges. Otherwise,
promote all its exact-level tree edges, then consider exact-level non-tree edges.
An internal candidate is promoted; a crossing candidate becomes a tree edge in
all forests from zero through that level. Exhausting level zero proves a split.
All changes complete synchronously before another operation can observe state.

HDT uses a dedicated marked variant of our existing AVL implementation. The
single-forest implementation and its measured identity remain unchanged. The
intentional sequence-code duplication isolates this experiment; consolidation
would need invariant tests and a new measured comparison of representation costs.
No third-party algorithm implementation or new dependency was added.

## Growing vertices and bounds

Let U be the smallest power of two at least the registered vertex count V.
Every component of F_i has at most U / 2^i vertices. Splitting a tree leaves a
smaller side of at most U / 2^(i+1), permitting its promotion. Levels never decrease
during an edge's lifetime; deleting and reinserting an edge starts a new lifetime.

When V crosses a power of two, U doubles and existing bounds relax; existing
edges stay at their levels. New vertices are registered only in F_0. Upper levels
are appended when promotions require them and materialize only touched vertices.
An absent upper-level vertex represents an isolated singleton with no incidences.
No graph reconstruction is required, including after previous promotions.
The source paper assumes a fixed vertex set; this growth policy is our extension.

For a history whose largest vertex count is N, an edge has O(log N) promotions.
Each promotion and marked lookup costs O(log V); ordered simple-graph edge maps
also cost O(log V), since E <= V(V-1)/2. This gives O(log² N) amortized edge
updates. Queries cost O(log V), including external ID lookup, with no scratch
allocation or answer cache. This binary AVL implementation does not claim the
paper's faster query bound using higher-arity trees.

Logical storage remains worst-case O(E + V log V). Adding vertices costs
O(log V) amortized, including ID indexing and base-forest vector growth. Each
upper record is created by an edge promotion, charging its O(log V) mapping
work to that promotion. No level initialization traverses the vertex universe.
Vector reallocations and a single large promotion search can cause latency spikes. Allocated storage can retain previous
arena/vector high-water capacities. Amortized complexity is not a p99 guarantee,
and this milestone does not provide a hard memory limit or a production SLO.

## Diagnostics and verification

HdtStats reports saturating tree-cut, candidate-edge, replacement, tree-promotion,
non-tree-promotion and visited-level counters. Counters reset after benchmark
setup and are reported separately from the ETT's ForestStats. A candidate visit
is an actual non-tree edge check, not a vertex count or elapsed CPU time.

Tests compare growing histories with matrix closure and the retained reference,
check full-width IDs, self-loops and shared queries, and validate every level's
spanning connectivity, component bounds, symmetric incidences, arc counts and
AVL aggregates. Targeted histories exercise deletion of promoted tree edges,
lower-level repairs, growth after promotions and stable dense bridge cuts.
An independent code review checked the algorithm and growth argument.

## Measurement scope

The new dense workloads keep two cliques fixed while toggling one or two cross
bridges. Clique membership and active bridge bits supply the independent oracle.
They target repeated failed searches and crossing replacements, not an arbitrary
production distribution. Sparse sustained workloads remain controls. Comparisons
include compact BFS, the unchanged pruned ETT and petgraph, the primary adopted
library baseline. No product superiority follows from these synthetic traces.

Results are recorded in the sibling benchmark repository with same-build sources,
separate setup costs, original/repeated queries, fresh/warmed process regimes,
update tails and whole-process peak RSS. Fresh means no deliberate warmup,
not flushed hardware caches. HDT remains opt-in even if it wins a selected case.


## Candidate-free level shortcut (v2)

The initial v1 prototype promoted tree edges even on a pure path, where no
replacement candidate exists. Measurements exposed unnecessary repair time and
extra tour copies. Version v2 first checks the smaller component's non-tree
aggregate. No exact-level incidence means no crossing replacement at that level;
higher-level replacements have already been excluded by the downward search.
Leaving tree edges unpromoted preserves nesting and size bounds because a cut
only splits components. The additional O(log V) lookup per level keeps the same
amortized bound. Candidate-bearing levels still promote tree edges before
considering non-tree edges.

The pure-path regression failed against v1 with 511 tree promotions instead of
zero, then passed with the shortcut. Further tests cover candidates only on the
larger side and a candidate-free higher level followed by a successful lower
replacement. The multi-level promotion fixture uses connected four-vertex
cliques so it still exercises promotions instead of requiring useless pure-tree
work. The algorithm and the shortcut both received independent review.

Raw v1 collections remain separately versioned. Version v2 comparisons run all
four engines in the same binary; v1/v2 timing observations come from separate
builds and are not a controlled same-binary speedup claim.


## Observed decision

The [recorded comparison](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-28-hdt-v2-dense/README.md)
contains 400 isolated processes across initial and optimized versions. In the
512-node one-bridge dense workload, final HDT takes 8.40 ms for 2,000 operations
(50% queries) versus 393.68 ms for ETT and 19.87 ms for compact BFS, excluding
setup. At 100,000 sparse cycle-block vertices the same query share instead takes
462.96 ms for HDT versus 7.96 ms for ETT. The pure-path shortcut removes useless
promotions, but per-level storage still gives about 390 MiB whole-process RSS
versus 82 MiB for ETT in that path case. These cells differ in trial counts as
documented in the report. Keep HDT experimental; optimize memory and promotion
cost before considering any default change.


## Sparse upper-level storage (v3)

Structural profiling found 358,616,832 bytes of vector capacity for 100,000
isolated vertices in v2: all 18 levels materialized every vertex. Version v3
keeps direct global indices in F_0 and stable global-to-local ordered maps plus
reverse vectors in higher levels. Exact-level adjacency sets still contain
global indices. Marked searches translate local tour vertices back to global
indices before consulting graph edge records. Removing edges retains mappings
and arena capacity; this does not provide memory compaction.

An implicit upper vertex is connected only to itself, has component size one
and has no marks. Incidence insertion and forest linking materialize their
endpoints. The promotion order, component bounds and synchronous repair remain
unchanged. Sparse mappings introduce extra lookups on upper-level repairs;
less storage is not a promise of faster cuts.

`HdtGraph::storage_stats()` returns vector capacity bytes, live ordered-container
payload and per-level counts. It excludes B-tree node overhead/spare slots,
allocator overhead and its own returned snapshot. Collect it outside timers;
it is neither total heap usage nor RSS. A separate benchmark `hdt-profile`
binary groups instrumented cuts with/without promotions. Normal benchmark
timing remains uninstrumented. Frozen `hdt-v2` and v3 share a comparison build.

The [v3 same-build comparison](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-28-hdt-v3-sparse/README.md)
records 284 normal and 8 diagnostic processes. At 100k vertices and 50% queries,
mean warmed path process RSS falls from 390.141 to 107.781 MiB; cyclic-block RSS
falls from 547.211 to 293.961 MiB. Cyclic-block base-call time increases from
448.397 to 633.770 ms with identical repair counters. Keep this representation
for its memory saving, but retain experimental status and target promotion-path
lookup/allocation overhead next. No default or runtime configuration change.

The [2026-10-04 endpoint-reuse experiment](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-04-hdt-localids/README.md)
removes two redundant upper-level endpoint lookups per tree promotion by reusing
the local indices returned by incidence insertion. It preserves all level and
forest invariants, work counters and public contracts. The 272-process paired
reference replay shows mixed runtime results: large cyclic-block warmed cells
improve by 3.30–11.15%, while 17 of 24 dense cells regress. Keep experimental
status; do not interpret this local reduction as a universal speedup. The report
preserves all outcomes and discloses uncaptured build-environment overrides.

The [2026-10-05 direct-join experiment](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-05-hdt-joins/README.md)
uses the two new directed edge tokens as AVL join pivots, avoiding the three
concatenations' pivot-extraction splits. Tour order, aggregates, stable handles
and logarithmic worst-case link cost are preserved; AVL shape may differ.
The full 272-process paired matrix shows 14.75–35.79% lower warmed 100k cyclic-block
runtime and 9.65–12.25% lower Cogentco runtime. A separate 48-process adaptive
confirmation still finds an 11.62% warmed 100k path / 50% query regression.
Retain experimental status and both reports; this is not a universal speedup.

The subsequent [fixed path investigation](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-05-hdt-path-investigation/README.md)
adds 12 paired trials per cache regime using the same executables and trace.
The warmed median changes from +11.62% to −8.98%, with 5/12 pairs still slower;
the fresh median is −11.30%. Therefore the earlier regression is an observation,
not a demonstrated stable magnitude or an isolated algorithmic cause. Preserve
all collections separately. No production change follows from this data-only
investigation; deterministic structural counters would be the next diagnostic.

The [isolated structural probe](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-05-hdt-structural-probe/README.md)
subsequently measures identical aggregate split/join/pull and root/rank traversal
counts for all 498 cuts and 500 queries of this trace. Its two replay links need
less structural work after the optimization. This rules out an increase in these
aggregate counts as the explanation for the observed timing gap; memory-access
effects remain unmeasured. No production instrumentation or path fallback is
introduced. Keep the original timing observations and experimental status.

## Packed token indices (2026-10-09)

Integrate the separately measured packed-index candidate into the experimental
HDT engine. Four private optional indices use `Option<NonZeroUsize>` with
`index + 1` encoding. Zero denotes absence; valid arena indices cannot equal
`usize::MAX`, and checked encoding rejects that invalid value. Decoding is
constant time. Graph IDs remain arbitrary `u64`; public APIs, configuration,
forest operations and asymptotic bounds are unchanged. No unsafe code or new
dependency is needed. On the measured 64-bit target, tokens shrink from 96 to
64 bytes; storage counters continue to use the actual token size.

The [full paired matrix](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-08-hdt-packed-tokens-matrix/README.md)
reports lower peak RSS in all 24 sparse cells (6.51–22.31%) and lower runtime in
all 12 cyclic-block cells (6.57–10.48%). It also retains one flagged fresh
million-node path regression (+119.85%). The subsequent
[resource diagnosis and separate confirmation](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-09-hdt-packed-resources/README.md)
do not reproduce that median slowdown: -21.88% with instrumentation and -16.51%
with the original binaries, each faster in 6/8 pairs. Adverse pairs remain;
neither the cause nor improved tail latency is established. Integrate for the
memory saving, without claiming a universal speedup or changing the default
engine or experimental status. Other isolated candidate patches remain separate.

Unit tests cover optional-index boundaries, clearing, overflow rejection, the
64-bit token budget and storage accounting. Existing forest/HDT differential
and invariant tests continue to cover graph behavior and token reuse.
