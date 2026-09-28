# Dynamic forest and scale benchmark implementation plan

Goal: preserve the compact default while introducing an exact experimental
Euler-tour forest and sustained, independently checkable scale workloads.
Architecture: stable arena handles, deterministic implicit AVL sequences with
one vertex token and two directed tokens per forest edge. Link concatenates
rerooted tours; cut splits at edge occurrences. Scan non-tree adjacency of the
smaller resulting tree for a replacement. No HDT levels or HDT bounds.
Tech stack: Rust 1.98.1, current locked workspaces, no new core dependencies.
Spec: user-approved three-step proposal in conversation; first milestone only.

Constraints: full growing u64 semantics, synchronous shared-reference queries,
English docs, no unsafe, preserve existing dirty changes, no commits/pushes.
Review focus: parent/rank/size consistency under rotations; recycling cut edge
handles; isolated growing nodes; cyclic replacement and failed searches; generator
oracle independence and actual operation ratios; process RSS is not engine heap.

- [x] Add AVL sequence/ETT tests that fail before implementation; implement private
      forest module with structural invariant tests and deterministic bounds.
- [x] Add public ForestGraph with Graph-compatible methods, counters and exhaustive
      smaller-component replacement; differential-test arbitrary histories and
      growth against matrix closure and retained reference.
- [x] Independently extend traces to sustained path/block churn, query ratios,
      topology-derived answers; validate generator with matrix oracle.
- [x] Integrate selector `ett-scan`, identity
      `knotrel-core/ett-scan-v1`; preserve all historical backend identities.
- [x] Collect separate-process scale reports, cache regimes/repeats, setup, p99,
      replacement counters and whole-process peak RSS with explicit scope.
      Sizes 10k/100k/1m staged, reduce expensive sampling transparently if needed.
- [x] Run both workspaces' required checks, independent review, artifact hash audit;
      report wins and regressions, and the next HDT/Cluster Forest decision gate.
