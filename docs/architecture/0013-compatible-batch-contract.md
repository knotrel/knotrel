# Compatible batch contract and failure model (M3.1)

Status: Proposed (Candidate Design for Review — Awaiting Maintainer Approval).

Cross-references:
- [HTTP API v1 Specification](../http-api.md)
- [Normative Integration Contract](../integration-contract.md)
- [ADR 0010: Coherent graph snapshots and instance-local revisions](0010-live-graph-statistics.md)
- [ADR 0012: External source-of-truth integration contract](0012-external-source-of-truth-integration-contract.md)

## Context

Knotrel v0.1.0 released an in-memory dynamic connectivity engine with a volatile standalone HTTP service supporting single operations (`POST /v1/operations`) and batches (`POST /v1/batch`). Knotrel v0.2.0 delivered the external source-of-truth integration architecture (`POST /v1/feeder/import`, `POST /v1/feeder/events`, `POST /v1/feeder/barrier`), fencing, and synchronization-aware readiness.

Milestone v0.3.0 addresses operational maturity. Feature M3 focuses on batch processing, retry semantics, and update efficiency. Feature M3.1 establishes the foundational contract:
1. Inventory and audit existing batch implementations.
2. Define a backward-compatible result reporting and version coherence contract.
3. Explicitly preserve the distinct semantics of the legacy `/v1/batch` endpoint and the feeder event endpoint `/v1/feeder/events` without artificial unification.
4. Assess client observability regarding operation outcomes, revision progression, timeout uncertainty, and metadata coherence.
5. Identify functional gaps and formulate minimally invasive, evidence-backed improvements.

## Analysis of Existing Batch Implementations

### 1. Legacy Batch Endpoint (`POST /v1/batch`)
- **Execution Model:** 1 to 1024 operations processed sequentially in request order under a single exclusive graph lock acquisition.
- **Isolation:** Readers and writers cannot interleave within a batch. Intra-batch operations observe the exact sequential effects of preceding operations in the same batch.
- **Failure Model (Continue-on-Error):** Graph errors on individual operations (e.g., `unknown_node`, `self_loop`, `node_limit_exceeded`, `edge_limit_exceeded`) do not abort execution or roll back earlier mutations. Completed operations remain applied in memory (**no rollback**). Subsequent operations continue to execute.
- **HTTP Status:** Always returns HTTP 200 OK if the batch request envelope is valid and admitted, even if individual operations fail. Syntax errors, envelope violations, or batch size bounds (< 1 or > 1024) reject the entire request with HTTP 400, 413, 415, or 422 before any graph work begins.
- **Mode Gate:** In `source-managed` mode, direct mutations are forbidden to prevent untracked divergence. Any batch containing $\ge 1$ mutation (`add_node`, `link`, `cut`) is rejected with HTTP 409 Conflict (`code: "direct_mutation_disabled"`). Query-only batches (`connected`) remain supported.
- **Result Reporting:** The response body contains `{"results": [Outcome, ...]}` with a 1-to-1 positional correspondence to `operations`.

### 2. Feeder Event Endpoint (`POST /v1/feeder/events`)
- **Execution Model:** 1 to 1024 source events processed under a single exclusive graph lock acquisition.
- **Fencing & Continuity:** Strictly enforced via `X-Knotrel-Instance-Id`, `feeder_session_id`, `feeder_epoch`, `generation`, `source_id`, `source_epoch`, contiguous `delivery_sequence` ($D = D_{applied} + 1$), and predecessor cursor `prev_source_position`.
- **Failure Model (Stop-on-First-Error):** On the first domain error (e.g., self-loop 422), execution halts immediately. The committed prefix $D \le D_{failed}-1$ remains applied in memory; uncommitted events are discarded without cursor advancement.
- **HTTP Status:** Partial failures return HTTP 422 (or 409/404) with structured diagnostics (`failed_batch_index`, `failed_delivery_sequence`, `failed_event_id`, and current applied frontier).
- **Deduplication:** Maintained in a bounded ring buffer window ($N = 10{,}000$). Exact duplicate retries return historical outcome without re-applying; conflicting duplicates trigger `Desynchronized`.

### 3. Public Semantic Separation Principle
The legacy `/v1/batch` endpoint and `/v1/feeder/events` serve fundamentally different architectural roles:
- `/v1/batch` is an uncoordinated, continue-on-error batch execution tool for standalone deployments where clients lack external event sequencing.
- `/v1/feeder/events` is an ordered, fail-fast stream ingestion pipeline for source-managed integration where an authoritative external log guarantees linear progression.

Unifying these endpoints by changing `/v1/batch` to stop on error or requiring external tokens would break existing standalone clients. Changing `/v1/feeder/events` to continue after errors would allow silent divergence from the external source of truth. **The two endpoints must remain distinct.**

---

## M3.1 Contract Decisions & Invariants

### D1: Operation Outcome Determinism
Clients can reliably determine each operation's outcome via the 1-to-1 mapping between `operations[i]` and `results[i]`. Every item in `results` is fully typed:
- Mutation: `{"result":"changed","changed":true|false}`
- Query: `{"result":"connected","connected":true|false}`
- Domain error: `{"result":"error","code":"...","message":"..."}`

Execution is deterministic: domain errors never roll back earlier successes or abort subsequent operations.

### D2: Revision Coherence and Version Arithmetic
Response headers (`X-Knotrel-State-Version`, `ETag`, `X-Knotrel-Generation`, `X-Knotrel-Instance-Id`) are captured under the **same exclusive lock boundary** as the batch execution.
- The revision in `X-Knotrel-State-Version` and `ETag` corresponds strictly to the **final graph state** after all operations in the batch complete.
- **Effective Mutation Rule:** Only operations returning `Outcome::Changed { changed: true }` increment `state_version` (by exactly 1). No-ops (`changed: false`), queries (`connected`), and errors never advance the revision.
- **Revision Arithmetic Formula:** A client observing final revision $V_{final}$ can compute the exact revision progression without intermediate token polling:
  $$\Delta V = \sum_{i=0}^{N-1} [\text{results}[i].\text{result} == \text{"changed"} \land \text{results}[i].\text{changed} == \text{true}]$$
  $$V_{start} = V_{final} - \Delta V$$
  $$V_k = V_{start} + \sum_{i=0}^{k} [\text{results}[i].\text{result} == \text{"changed"} \land \text{results}[i].\text{changed} == \text{true}]$$
- Because `state_version` uses little-endian decimal digits in `GraphState`, revision arithmetic holds across arbitrary precision without 64-bit integer overflow.

### D3: Timeout Uncertainty Model
When `POST /v1/batch` exceeds `KNOTREL_REQUEST_TIMEOUT_MS`, the HTTP connection returns HTTP 503 `request_timeout` ("request exceeded the server time limit; its effect is unknown").
- Once admitted to the internal `spawn_blocking` pool, worker tasks own their permit and run to completion regardless of client cancellation or network disconnection.
- A timed-out batch leaves an **inherently uncertain outcome**: the client cannot distinguish whether the batch was rejected before admission, aborted during network I/O, or executed completely in the background.
- Because standalone mode does not maintain request ledgers or deduplication IDs, retrying a timed-out batch may observe `changed: false` for previously applied operations.
- Operational rule: Clients requiring verifiable retry idempotence across timeouts must operate in `source-managed` mode using `/v1/feeder/events`.

### D4: Response Metadata and Concurrency Invariant
- The exclusive graph lock guarantees sequential consistency across requests.
- Concurrent reads (`GET /v1/stats`, `POST /v1/operations` queries, or query batches) observe either the complete graph state prior to the batch or the complete graph state after all operations in the batch have executed.
- No client can ever observe an intermediate state or a split revision where some operations of a batch are visible while others are pending.

---

## Evaluation of Potential Contract Extensions

1. **Top-Level `state_version` in `BatchResult` Body:**
   - *Proposal:* Add `"state_version": "<version>"` to the JSON response of `/v1/batch`.
   - *Assessment:* Redundant with the mandatory `X-Knotrel-State-Version` and `ETag` HTTP headers emitted on every successful batch. Introducing new JSON fields risks breaking existing client parsers configured with `deny_unknown_fields`.
   - *Decision:* **Rejected for M3.1.** Preserve the minimal, backward-compatible JSON schema `{"results": [...]}`.

2. **Per-Item Intermediate Version Reporting:**
   - *Proposal:* Include `"state_version"` inside each `Outcome` element in `results`.
   - *Assessment:* Imposes significant allocation, cloning, and serialization overhead on large batches (up to 1024 operations) while providing zero new information, since intermediate revisions are trivially derivable via the deterministic formula in D2.
   - *Decision:* **Rejected.** Violates update efficiency and minimal overhead principles.

3. **Conditional Precondition Header (`If-Match`) on `/v1/batch`:**
   - *Proposal:* Allow standalone batches to supply `If-Match: "<version>"` to assert starting state.
   - *Assessment:* Requires a broader protocol decision regarding optimistic concurrency across single operations vs batches in standalone mode. Standalone mode currently uses lock-acquisition ordering without client-side state preconditions.
   - *Decision:* **Deferred.** Preserved for operator discussion; held out of M3.1 to avoid unilateral protocol speculation.

---

## Verification & Acceptance
Feature M3.1 is evidenced through:
1. Architectural decision documentation (this document).
2. Specification enhancements in `docs/http-api.md`.
3. Dedicated deterministic contract and concurrency integration tests in `crates/knotrel-server/tests/batch.rs` covering:
   - 1-to-1 outcome reporting across mutations, queries, no-ops, and domain errors.
   - Continue-on-error behavior preserving earlier and later mutations without rollback.
   - Revision progression arithmetic and arbitrary-precision carry.
   - Intra-batch sequential visibility of prior mutations to subsequent queries.
   - Concurrency isolation proving absence of intermediate state leakage.
   - Mode segregation rejecting direct mutations in source-managed mode.
   - Timeout uncertainty demonstration under client abort.
