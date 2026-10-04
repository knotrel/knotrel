# ADR 0011: opt-in caller-owned BFS workspace

Status: experimental API; compact default and server dispatch unchanged.

`Graph::connected_with_workspace` accepts `&mut BfsWorkspace` while retaining an
immutable graph reference. Each worker/caller owns its scratch, so shared graph
queries do not need an internal lock. Workspace reuse never caches answers.

The queue and mark storage retain capacity. Marks are u32 generations; an epoch
increment invalidates earlier visits, including when reusing the workspace with
another graph. On rollover, reset the entire retained mark array. Growth adds
zero marks. Unknown-ID validation preserves source-first errors; reflexive
queries do not traverse. Queue length clears on both success and failure.

A first touched-mark-reset strategy was measured and retained in benchmark
snapshots; it regressed on large cyclic blocks. Generation marks remove that
reset pass at the cost of four bytes per node instead of one. End-to-end sparse
replay improves in this campaign, but dense cells remain mixed. This is not
sufficient evidence to replace the default API or configure server use silently.

The sibling benchmark repository retains the complete reference campaign and
new before/after evidence under `results/2026-10-04-workspace-v2-comparison/`.
Compare same-build baseline and candidate, plus historical baseline drift.
Use the same traces, preserve regressions, and report process RSS, setup and
latency tails separately. Warmup constructs a different graph/workspace than the
measured replay; first-query workspace growth is part of measured query cost.

Tests cover mutations, growth, different index mappings, concurrent workspaces,
early exit and generation rollover requiring traversal through a stale-marked
intermediate vertex. No unsafe code or dependencies are added.
