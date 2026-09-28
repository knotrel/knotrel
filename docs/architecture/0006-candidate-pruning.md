# Pruning replacement searches in Euler-tour trees

Status: Implemented in experimental ForestGraph, 2026-09-27. The default compact
Graph and the HTTP service are unchanged. Refines ADR 0005; still not HDT.

## Change and invariants

Every permanent vertex token has a boolean indicating whether its non-tree
adjacency is nonempty. Every AVL token stores the OR of that flag over its
subtree. Directed edge tokens never carry a local flag. Structural pull repairs
the aggregate after join/split/rotation; changes in non-tree membership repair
ancestors without altering parent pointers and stop when the aggregate stops
changing. Insertions, non-tree deletions and promotion to forest edges update
both endpoints. Recycled edge tokens start with both flags false.

After a tree cut, select the smaller side by vertex count, as before. If its
aggregate is false, skip enumeration without allocating a scan stack. Otherwise,
a borrowing iterator descends only subtrees with candidates, yields marked
vertices in v1's node/right/left order and stops when a crossing edge is found.
An iterator borrow prevents topology/membership changes during the scan. Promotion
occurs after the iterator is dropped. The first chosen replacement is therefore
the same as v1, despite skipping empty vertices and stopping enumeration early.

If a side has no candidate, no non-tree edge can cross it. If candidates exist,
all their incidences are still checked until success or exhaustion; pruning does
not change exactness or graph visibility. Existing random matrix/reference tests
and AVL invariants remain applicable. New tests first failed against v1 and then
passed: candidate-free bridge cuts enumerate zero vertices; a large cycle stops
at one candidate; removal/promotion of the last non-tree edge clears the flags.

## Costs

Aggregate maintenance takes O(log V) worst case per changed endpoint. Link and
non-tree-cut asymptotic bounds remain O(log V + log E), with the previously
stated allocation caveats. Enumerating k candidate-bearing vertices in a smaller
side of size s visits O(min(s, (k+1) log V)) AVL tokens and uses O(log V) stack
space. Candidate checks still cost O(c log V). Empty candidate sides need only
root/size/aggregate checks; no O(s) enumeration or scratch remains.

Extra per-token flags and aggregate maintenance add space and CPU work, including
during initial construction. This is not an asymptotic replacement-search bound:
dense internal non-tree edges can still be repeatedly inspected after cuts. HDT
levels and Cluster Forest remain separate candidates if these costs dominate.

## Comparable versions and counter meanings

The benchmark freezes the former implementation in `src/ett_baseline/`, used
only by `--engine ett-scan-v1` (identity `knotrel-core/ett-scan-v1`). Imports differ
and count accessors are test-only; timed methods and original token layout are
preserved. Current `--engine ett-scan` is `knotrel-core/ett-pruned-v2`.

`scanned_vertices` has an intentionally versioned meaning: v1 counts every
vertex eagerly enumerated before any candidate test, whereas v2 counts only
candidate-bearing vertices actually yielded before success/exhaustion. Neither
is a count of AVL tokens touched. Do not interpret their ratio as a CPU-work
speedup. `tree_cuts`, `candidate_edges` and `replacements` retain their meanings;
they should match between versions on the same ordered trace. Timing captures
both pruning benefits and aggregate-maintenance costs.

See [same-build measurements](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-27-forest-pruned/README.md)
for setup, memory, update tails and complete-operation comparisons. Previous
result directories remain immutable observations of their recorded source.
