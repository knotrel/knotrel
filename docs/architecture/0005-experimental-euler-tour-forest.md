# Experimental Euler-tour spanning forest

Status: First prototype implemented, 2026-09-27. The current experimental
implementation incorporates [candidate pruning](0006-candidate-pruning.md); this
ADR describes the original v1 design and measurements. The default Graph and
HTTP service still use compact BFS. This is not HDT and has no HDT update bound.

## Representation and exactness

`ForestGraph` has the same graph-operation contract as Graph, including growing
u64 IDs, isolated vertices, idempotent/reversed edges and source-first errors.
An ordered ID map gives stable indices. Each component's Euler tour is an implicit
AVL sequence: one permanent vertex token, plus two directed tokens for each tree
edge. Parent pointers support roots/ranks, subtree sizes support splitting, and
vertex counts select the smaller side after a cut. Link rotates both tours and
concatenates them with the directed edge tokens. Cut splits at both occurrences.
Tokens for removed tree edges are recycled; vertices never disappear.

Every live graph edge is either a forest edge or a non-tree edge. The latter is
stored at both endpoints. Removing a non-tree edge cannot split the forest.
Removing a forest edge partitions its tree. Scanning non-tree incidences of the
smaller resulting tree finds a crossing edge if one exists; promoting one restores
the spanning-tree invariant. If none crosses, the original component really split.
All repair finishes before the mutation returns. Queries compare tour roots,
allocate no scratch, mutate no state and acquire no internal locks.

The Euler-tour forest reduction follows [Holm, de Lichtenberg and Thorup,
JACM 2001, section 2](https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf).
The sequence implementation here uses deterministic AVL joins/splits. No source
was copied and no random balance or new core dependency was introduced.

## Bounds and limits

Query: O(log V) worst case including ID translation. Link: O(log V + log E)
amortized, allowing growing arenas. Non-tree cut: O(log V + log E). Tree cut:
O(log V + log E + s + c log V), amortized for free-list growth, where s is the
smaller side's vertex count and c is examined non-tree incidences. One arena or
free-list growth can add O(V) time. Smaller-side enumeration allocates O(s) scratch.
Even if the first candidate reconnects the graph, enumeration has already visited
all s vertices. Repeated failed searches are unbounded by any HDT level invariant.

Logical memory is O(V + E); arenas retain O(V) high-water capacity. Counters report
tree cuts, enumerated vertices, examined non-tree incidences and replacements.
Counters saturate rather than wrapping, and the harness resets them after setup.

## Evidence and next decision

Tests validate AVL heights, parent pointers, ranks via cut/relink behavior, subtree
sizes, token partition/recycling and edge classification. Random growing histories
are checked against matrix closure and the original BFS; shared queries and
10,001-node paths exercise growth and thread-safe reads. Benchmark adapters also
include the prototype in independent registered-universe checks.

Scale workloads use an independent missing-boundary oracle. Path edges toggle
without restoration; blocks remain internally connected while their cycle edges
and connecting bridges change. These are intentionally constrained sparse models,
not arbitrary dense churn or real datasets. Fixed 2,000 operations mean only 20
updates at 99% queries; large-graph results can reflect initial fragmentation.
The separate longer run investigates more updates, not a universal steady state.

See the sibling benchmark [scale report](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-27-forest-scale/README.md).
Only promote a maintained index to the default after representative update-tail,
memory and setup budgets are met. Repeated replacement work motivates HDT levels;
space pressure motivates Cluster Forest evaluation. Neither is implemented here.

## Observed bottleneck

The scale run makes smaller-side enumeration a concrete optimization target:
path deletions inspect no non-tree candidates yet enumerate many vertices.
Before adding HDT levels, consider non-tree-incidence aggregates to skip empty
regions and an iterator that stops enumerating once a replacement is found.
That would reduce unnecessary scan work without claiming an HDT amortized bound.
Measure added metadata/update costs and arena memory; do not assume the change
wins on every query/update mix. Cluster Forest remains a research alternative,
not an implemented or commercially validated dependency.

## Packed token indices (2026-10-09)

The measured packed-index candidate is integrated into the experimental ETT
forest. Four private optional indices use checked `index + 1` encoding in
`Option<NonZeroUsize>`. Absence uses zero; `usize::MAX` cannot name a valid arena
element and is rejected without wrapping. Encoding and decoding take constant
time. Public graph IDs remain arbitrary `u64`. Algorithms, APIs, asymptotic
bounds, configuration, default engine and experimental status are unchanged.
No unsafe code or dependency is introduced. Tokens occupy 64 rather than 96
bytes on the measured 64-bit target; unit tests cover layout, index boundaries,
clearing and overflow, alongside existing invariant and token-reuse tests.

The [38-cell paired matrix](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-10-09-ett-packed-tokens-matrix/README.md)
uses the exact candidate from the isolated pilot. All 304 processes succeeded;
all 38 runtime medians improved and 141/152 pairs were faster. Median cell runtime
changes were -18.69% sparse, -12.53% dense and -8.60% Cogentco. Sparse peak process
RSS decreased in all 24 cells, with median change -18.25%. These are unweighted
cell summaries, not aggregate throughput or graph-only allocation measurements.

The sparse setup tradeoff remains: median cell change +4.56%, maximum +8.73%.
Individual adverse pairs and short-path timing variability remain in the report.
Four pairs per cell do not establish universal speedups or tail guarantees.
This integration preserves all historical results and combines no other isolated
algorithm candidate.
