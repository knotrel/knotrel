# Node and edge lifecycle implementation plan

> **For agentic workers:** Use `superpowers:executing-plans` to implement this plan task-by-task after the M6 gate and contract acceptance. Do not start implementation from this planning task. Do not commit without explicit user authorization.

**Goal:** Support reversible node/edge deactivation and permanent node removal without losing explicitly configured connection state.

**Architecture:** Keep a canonical registered topology with independent node-active and edge-enabled flags, and derive the active graph used by connectivity engines. The authoritative data source remains external. Put shared lifecycle semantics in `knotrel-core`, with HTTP and ordered-feeder adapters in the server; do not implement a different state machine for each engine.

**Tech Stack:** Existing Rust workspace, Rust 1.100.0, edition 2024; preserve core MSRV 1.98.1 and Elastic-2.0. No new dependency is presumed necessary.

**Spec:** The proposed behavioral specification in sections 1–3 of this document. Scheduling is approved by the owner on 2026-11-13; detailed contracts require M10.1 acceptance before implementation.

**Status:** Planned, not implemented. Start only after Acceptance M6, including its overhead/cardinality gate. This is not part of released v0.3.0 and has no assigned release version. It does not silently change v0.4.0 persistence scope.

## Global constraints

- Preserve existing `link`, `cut`, query, batching and source-managed authority contracts for clients not opting into lifecycle support.
- Keep graph algorithms synchronous; no HTTP or telemetry dependency in the core.
- Do not couple this feature to persistence, multi-graph namespaces, typed domain nodes or electrical simulation.
- Acknowledgment still describes in-memory state, not durability.
- Preserve coherent response metadata, generation fencing, ordered source progress, deduplication, cancellation uncertainty and worker-owned shutdown accounting.
- Registered inactive objects consume limits and memory. Disabling must not bypass resource limits.
- Do not assume dense engine indices are safely reusable or that cutting incident edges frees vertex storage.

## Review focus

1. Disabling a node must not erase the independently configured enabled state of incident edges (tasks 2–3).
2. Removing and recreating an external ID must not reconnect stale engine adjacency or replay old lifecycle events (tasks 3–5).
3. Partial engine errors must not leave canonical and active topology disagreeing while continuing to serve (tasks 2–5).
4. Queries and import publication must never expose half an isolation/removal operation (tasks 4–5).
5. Inactive topology must survive source rebuild, limits and observability accounting (tasks 5–6).

## 1. Proposed behavior for M10.1 acceptance

An edge is effective exactly when it exists in the registered topology, its enabled flag is true, and both endpoints exist and are active. Flags describe intended state, not measured electrical availability.

| Proposed operation | Meaning | Repeated operation |
| --- | --- | --- |
| `set_node_active(node, active)` | Change the node flag, retaining incident registered edges and their flags. | Existing node already in that state: unchanged. |
| `set_edge_enabled(source, target, enabled)` | Change an existing edge's independent flag. Enabling while an endpoint is inactive does not make the edge effective. | Existing edge already in that state: unchanged. |
| `remove_node(node)` | Permanently remove the node and all incident registered edges, including disabled ones. | Missing node: unchanged (proposal). |
| Existing `cut(source, target)` | Remove the registered edge, whether effective or disabled. | Preserve current missing-edge and invalid-input behavior. |
| Existing `add_node` | Create an active node when absent; do not implicitly reactivate an existing inactive node. | Existing node: unchanged. |
| Existing `link` | Create an enabled registered edge when absent; preserve current endpoint-creation behavior. Existing disabled edges are not implicitly enabled. | Existing registered edge: unchanged; use the explicit setter. |

Proposed Rust setters/removal return `Result<bool, GraphError>` where true means a canonical state change. Use existing NodeId types. Names and wire shapes are proposals until task 1 is accepted.

Queries use effective topology. Proposed policy: an inactive endpoint makes `connected` false, including `connected(v, v)`; unknown-node and malformed/self-edge operation errors retain the existing rules. Missing node/edge setters return a specific domain error rather than silently create topology. Task 1 must resolve error representation without inadvertently breaking exhaustive public enum matches.

Removing a node is a cascade; no restrict/cascade option is planned initially. Re-adding its ID creates a new active isolated node. Removed edges never reappear automatically. No source-asset identity/incarnation system is added: ordered source history and fencing remain responsible for preventing obsolete updates.

### Electrical example

Disable substation A2: its incident connections become ineffective. Explicitly disable one of those connections while A2 is inactive. Reactivate A2: only independently enabled connections to active neighbors return. Removing A2 instead destroys all incident registered edges; recreating A2 does not restore them.

## 2. Compatibility, atomicity and versioning

Adopt an additive lifecycle-capable core facade and explicit server opt-in as the starting design. Existing `Graph`, `ConnectivityGraph` constructors and legacy service mode should retain their observable semantics and avoid duplicating the complete topology when lifecycle support is disabled. Inspect existing engine storage before selecting the concrete facade or internal abstraction in task 1.

A single effective lifecycle command advances local revision exactly once, irrespective of the number of incident edges affected. A flag change advances revision even if effective connectivity is unchanged. No-op/rejected commands do not advance it. Generation changes only on replacement import, not removal or reactivation. Existing batch ordering, continue-after-domain-error behavior and no rollback remain unchanged; each lifecycle command must nevertheless be indivisible to observers.

Perform service mutations under the existing graph lock. Prevalidate ordinary errors before touching either representation. A panic/unrecoverable internal failure must use the existing unavailable/fail-closed policy; do not return a successful or ordinary rejected operation with inconsistent graph state. Metrics published after the operation must describe its accepted boundary.

For embedded callers, define exclusive mutation access and document behavior on failure. Atomic visibility does not imply transactional rollback across a whole legacy batch.

## 3. Integration and resource contract

- Source-managed mode accepts lifecycle changes only through ordered feeder events; standalone direct operations must not bypass source authority.
- New event variants include flags in duplicate identity/fingerprints. Exact duplicates never reactivate removed topology or advance revision again.
- Import must encode all registered nodes/edges and their flags, including isolates and disabled edges. Existing payloads default to all active/enabled. Reject unsupported lifecycle payloads when the feature is disabled; never silently discard flags.
- Keep imported topology and flags in the candidate until atomic publication, with existing ownership, lease and If-Match validation.
- External sources must provide flags on authoritative rebuild. A restart still loses in-memory state; no local recovery is introduced.
- Proposed count policy: existing logical node/edge counts describe registered objects in lifecycle mode; add explicitly named active/effective counts. Document the opt-in semantics for stats, gauges and capacity. A metric or label migration needs explicit review under M10.1.
- Limits apply to registered objects. Explain logical deletion versus actual memory reclamation. Do not claim repeated remove/reinsert workloads use bounded memory until engine slot reuse/compaction is tested.
- Deactivation/removal cost depends at least on incident degree and engine updates. Evaluate build-then-swap fallback where an engine cannot delete vertices safely; disclose its whole-graph cost rather than promise logarithmic removal.

## 4. Implementation tasks (after M6)

### Task 1 — M10.1 contract and engine feasibility

**Files:** inspect `crates/knotrel-core/src/{lib,configuration,index,workspace,dynamic,hdt}.rs`, server `src/{api,service,configuration}.rs`, existing import/event tests; update this plan and create `docs/topology-lifecycle.md`.

**Consumes:** existing v0.3.0 contracts and completed M6 instrumentation.
**Produces:** accepted Rust/wire signatures, opt-in configuration, errors, count semantics and an engine-by-engine deletion strategy.

- [ ] Inventory node ownership, edge storage, missing-node query rules, limits and reusable slots in each supported engine.
- [ ] Resolve proposed choices in sections 1–3 against compatibility constraints; record an explicit decision for every deviation.
- [ ] Specify exact HTTP operation/event/import shapes and defaults, error mapping, ID-reuse semantics and telemetry names.
- [ ] Specify recoverable-error atomicity and unsupported-engine behavior. Do not silently switch engines.
- [ ] Record acceptance before implementing tasks 2–6. Do not start a benchmark campaign in this contract task.

### Task 2 — M10.2 reversible lifecycle in the core

**Files:** create `crates/knotrel-core/src/topology.rs` and `crates/knotrel-core/tests/topology.rs`; expose the accepted facade through `src/lib.rs`; adapt `src/configuration.rs` only where needed.

**Consumes:** accepted setters, canonical/effective state invariant and engine adapter strategy.
**Produces:** engine-independent lifecycle facade with synchronous mutation/query access.

- [ ] Write failing oracle tests for disable/enable, inactive self-query, unknown setters, independent flags, parallel disabled endpoints, no-ops and links to inactive endpoints.
- [ ] Implement canonical state and effective-edge transitions; validate ordinary failures before publication.
- [ ] Test all supported engines against a separate BFS oracle filtering registered objects by flags. Enumerate small graphs and deterministic mixed mutation sequences.
- [ ] Verify construction with lifecycle disabled preserves existing behavior and does not allocate the additional topology catalog.
- [ ] Run `cargo test -p knotrel-core --locked` and `cargo +1.98.1 test -p knotrel-core --locked`.

### Task 3 — M10.3 permanent node removal

**Files:** core `src/topology.rs`, agreed engine adapters/indexing files; `tests/topology.rs`.

**Consumes:** lifecycle facade from task 2.
**Produces:** cascading `remove_node`, safe ID reuse and a documented storage-reclamation strategy.

- [ ] Write failing tests for isolated, active, inactive, high-degree and cyclic-node removal; disabled incident edges; repeated removal; remove/reinsert of the same external ID.
- [ ] Implement removal using task 1's approved engine strategy, preserving canonical/effective consistency.
- [ ] Verify surviving connectivity with the filtered BFS oracle and no stale adjacency after ID reuse.
- [ ] Test bounded repeated churn against the documented slot/reclamation policy; distinguish logical object limits from allocated storage.
- [ ] Run core/MSRV tests; document degree-dependent and any rebuild costs without unmeasured performance claims.

### Task 4 — M10.4 standalone HTTP, revisions and cancellation

**Files:** server `src/{api,service,configuration}.rs`, `tests/{http,batch,identity,metrics}.rs`, new `tests/topology.rs`; `docs/http-api.md` and `docs/openapi.yaml`.

**Consumes:** core lifecycle facade and accepted HTTP schema.
**Produces:** opt-in standalone operations with exactly one revision step per changed command.

- [ ] Add failing contract tests for feature disabled, active flags, no-ops, errors, cascade removal and one-element/mixed batches.
- [ ] Integrate under existing worker/lock/guard ownership, preserving batch and metadata semantics.
- [ ] Use deterministic synchronization to prove concurrent queries see before or after state, never partially removed incident edges.
- [ ] Test caller timeout/cancellation followed by eventual completion; preserve uncertain-outcome behavior and coherent revision metadata.
- [ ] Run focused HTTP, batch, identity, lifecycle and metrics tests in both configurations.

### Task 5 — M10.5 source-managed events and atomic import

**Files:** server `src/{api,service}.rs`, `tests/{events,import,e2e_rebuild}.rs`, `tests/common/feeder.rs`; `docs/integration-contract.md`, `docs/openapi.yaml`.

**Consumes:** accepted event/import schema and task 4 mutation semantics.
**Produces:** exact ordered replay and complete external reconstruction of registered and effective topology.

- [ ] Add failing tests for flags in imports, missing/default flags, invalid references, limits including inactive objects and rejected candidate invisibility.
- [ ] Extend feeder events and duplicate fingerprints; keep sequence/position advancement after applied updates and normal progress/no-op rules.
- [ ] Test partial-prefix failure, retries, same ID removal/recreation, old generation/session fencing and duplicate removal/reactivation events.
- [ ] Verify direct lifecycle writes remain rejected in source-managed mode.
- [ ] Test restart plus authoritative import/replay reproduces flags and connectivity; test concurrent replacement and queries preserve coherent tuples.
- [ ] Run import, feeder, identity and end-to-end suites with test-support.

### Task 6 — M10.6 observability, tutorial and qualification

**Files:** server `src/metrics.rs`, `tests/metrics.rs`; `docs/{observability-contract,http-api,deployment}.md`; `examples/power-grid/`, `docs/tutorials/power-grid.md`; benchmark repository changes in a separately scoped follow-up.

**Consumes:** accepted count semantics and stable implementation from tasks 2–5.
**Produces:** updated operator guidance, electrical-topology demonstration and bounded qualification evidence.

- [ ] Test registered versus effective gauges, revision/no-op operation counters, disabled mode and fixed series bounds. Do not label with object IDs.
- [ ] Extend the tutorial to disable a substation, disable one incident edge independently, reactivate and demonstrate that the edge stays disabled; separately remove/recreate the node.
- [ ] Document retained edge state, limits, source requirements and algorithmic costs. Continue distinguishing topology from electrical power flow.
- [ ] Propose a finite performance/resource protocol before execution: low/high degree, cycles, inactive-heavy graphs, remove/reinsert churn, imports, and an unchanged all-active control. Record source/build identities and compare disabled/enabled costs and memory. Obtain budget/threshold approval before measurement; do not inherit old thresholds automatically.
- [ ] Run formatting, strict Clippy, default/test-support workspace tests, warning-denied Rustdoc and core MSRV checks. Validate benchmark additions in their repository.
- [ ] Record independent acceptance of semantics, engine equivalence, compatibility, resource behavior and approved performance results. Assign release scope/version separately; do not amend v0.3.0 acceptance.

## Completion gate

M10 is complete only when the accepted lifecycle contract works in the core, standalone HTTP and source-managed import/replay, all supported engines pass the independent oracle tests, inactive objects respect limits, concurrency/cancellation invariants hold, operator documentation and the tutorial agree with behavior, and resource/performance evidence meets its separately approved criteria. Unsupported engine combinations must be explicit, never silently degraded.
