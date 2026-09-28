# Compact adjacency in the default core

Status: Implemented, 2026-09-27. Refines ADR 0003's first milestone; this is still
BFS, not a maintained dynamic-connectivity index.

## Design and compatibility

`Graph` maps arbitrary u64 IDs through a BTreeMap to append-only indices.
Adjacency is a vector of sorted vectors of indices. Binary search detects
redundant edges; insert/remove preserves ordering and symmetric adjacency.
Vertices are never deleted or renumbered. `link` creates endpoints, `cut` retains
isolated vertices, self-loops fail before mutation, and queries validate source
before target. Counts and public method signatures are unchanged.

`connected(&self)` allocates visited bits and a queue per call. This preserves
shared-reference queries and Send/Sync without interior locks or stale answer
caches. The benchmark-only dense control reuses scratch and uses unsorted
adjacency; it is not the same algorithm implementation as this default core.
The server uses the new Graph automatically; its admission and locking policy
remain unchanged. No dependencies or unsafe code were added.

The old algorithm is retained as the public `ReferenceGraph`, sharing NodeId and
GraphError. The benchmark selector `reference-bfs` continues to mean that original
algorithm; `compact-bfs` measures the current default. Historical results remain
historical rather than being relabeled as measurements of the new core.

## Costs and tradeoffs

Endpoint lookups cost O(log V). BFS costs O(V + E) worst case with O(V) local
scratch, including its initialization and allocation. Reflexive queries return
before allocating scratch. No results are memoized.

Sorted-vector updates cost O(log V + deg(source) + deg(target)) amortized because
of shifts/allocation. Adding a vertex costs O(log V) amortized but may trigger
O(V) outer-vector growth in one call. This intentionally gives up the original
ordered-set O(log V) update bound. High-degree mutation-heavy workloads can lose;
query improvements do not establish a universal win.

Logical storage is O(V + E). Allocated capacity is O(V + H), where H sums the
historical maximum degree of every vertex. Deletions do not shrink capacity.
Concurrent queries allocate independent scratch. Memory and production latency
budgets still need dedicated measurement.

## Validation and next gate

Existing HTTP and matrix-oracle tests remain applicable. Additional tests cover
257 growing full-width IDs, cut/rejoin after growth, isolated vertices, shared
queries, and 1,500 mixed operations compared with the original engine including
errors, counts and repeated queries. The registered-universe benchmark also
checks the new engine against its matrix oracle.

See the [recorded compact-core comparison](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-27-compact-core/README.md)
for same-run reference and petgraph comparisons, setup and cache protocol.
Before selecting HDT, extend beyond these sparse restoring traces to sustained
churn, high-degree update stress, larger sizes, query/update ratios and measured
memory. No claim about Differential Dataflow or Memgraph performance follows.
