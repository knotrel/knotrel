# Comparison-driven optimization gates

Status: Proposed, 2026-09-27. Refines the implementation order in ADR 0002.
At proposal time no production engine had been replaced. The compact-core
milestone is now implemented in [ADR 0004](0004-compact-core.md).

## Evidence

The sibling benchmark repository records the
[embedded comparison](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/2026-09-26-competitors/README.md)
and [candidate selection](https://github.com/knotrel/knotrel-benchmarks/blob/main/docs/research/2026-09-26-competitive-landscape.md).
Petgraph is the primary external embedded baseline because it represents an
adopted alternative to a custom connectivity kernel. Outils is only an HDT
algorithmic control; its small, inactive project does not establish market demand.
Differential Dataflow has named product adoption through Materialize and is the
next incremental reference. Memgraph merits a later product comparison if a real
customer needs topology queries within its broader database workflow.

At 4096 vertices in the warmed experiment, compact BFS and petgraph traversal
have lower median base-operation totals than outils on three of four families;
outils wins the hub family. Its cheaper queries do not compensate for updates
everywhere. These totals exclude setup and extra repeat timings, but the repeats
affect cache state. The experiment does not establish a universal crossover.

## Decision

Retain the original synchronous core as a correctness reference. First develop
and measure a compact representation supporting the full growing-u64-node
contract. The current benchmark-only compact adapter uses a fixed universe and
is not ready to replace the core. Evaluate maintained connectivity against both
that stronger traversal baseline and petgraph, not only the original ordered BFS.
HDT remains a candidate rather than an already justified production choice.

Before selecting it, measure larger graphs, sustained churn, query/update mixes,
replacement-search stress, full loading cost and memory. Keep correctness checks,
fresh/warmed protocols and repeated-query reporting. Production changes must
preserve isolated nodes, arbitrary u64 IDs, idempotent updates and exact visibility.
No GraphScope, Memgraph or Differential Dataflow performance claim is supported
until an equivalent adapter is executed and validated.
