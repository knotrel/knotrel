# HDT storage profiling and sparse levels

Goal: profile current HDT v2 on 100k-node paths/cycle blocks, then reduce owned storage without changing exact/growing graph semantics or the compact default. Existing staged and unstaged work stays intact; no commits, pushes or dependency additions.

1. Add read-only HdtStorageStats/HdtLevelStorage snapshots: Vec capacity bytes (not allocator size), live ordered-container payload lower bound (not B-tree node overhead), per-level vertex/edge/incidence counts. Never label these as complete engine heap or subtract harness RSS to invent heap usage. Snapshot collection is outside timed calls and itself allocates output memory.
2. Freeze v2 with identical algorithm/layout and the same snapshot accounting as a benchmark-only hdt-v2 adapter. Add a standalone diagnostic replay that records vertex registration, initial edges and final storage, plus timings/counter deltas grouped by promoting/non-promoting cuts. Diagnostic timing has instrumentation overhead and is separate from the normal benchmark.
3. Profile before optimizing. If empty-level duplication dominates, implement lazy sparse per-level vertices: F_0 uses dense direct global indices; higher forests map touched global IDs to stable local indices. Absent higher-level vertices are implicit isolated components (size 1, no marks). Materialize endpoints on incidence/link insertion only. Lazy forests are appended on promotion, never initialized over the whole universe. Growing vertices populate only F_0. This retains O(log² N) amortized edge updates: each higher vertex registration is charged to a promotion, with O(log V) mapping overhead. Memory remains worst-case O(E + V log V), retaining historical touched vertices.
4. Verify implicit-isolated semantics, growth after promotions, deletion/replacement at all levels, identical work counters and differential histories. Compare current hdt (v3 if changed) with frozen hdt-v2, unchanged ETT, compact and petgraph. Capture profiles and normal fresh/warmed/repeated-query measurements separately with source/trace hashes, setup, worst cuts and RSS. Include sparse 10k/100k, dense 512 and long cyclic controls; avoid claiming general speedups.
5. Both repo quality gates, independent implementation/report review, artifact audit and a report explicitly listing uncovered B-tree/allocator overhead and any speed/memory tradeoff. Default/server API unchanged except the experimental implementation behind hdt.

Review focus: sparse global/local ID translation in incidence sets and tour markers; absent vs known isolated vertices; no full-V allocation on first high-level promotion; F_0 direct mapping avoids query regression; capacity accounting avoids counting headers twice or treating live payload as actual heap; profiling does not pollute normal timing.

## Completed outcome

All five steps completed. Structural profiling identified empty-level duplication;
sparse upper vertices, frozen v2, diagnostic replay and source-preserving
collectors are implemented. Core/server and benchmark quality gates passed,
with independent algorithm and measurement review. 284 normal plus 8 diagnostic
processes share source inputs; 943 recorded artifact hashes, 14 trace groups
and 14 HDT counter groups were checked. The report is in the sibling benchmark
repository at results/2026-09-28-hdt-v3-sparse/README.md.

Memory improves substantially, but cyclic repair time regresses; v3 remains
experimental. Function-level CPU/allocator profiling and another promotion-path
optimization are follow-up work, not completed claims. No complete heap estimate
or default change is justified by this milestone.
