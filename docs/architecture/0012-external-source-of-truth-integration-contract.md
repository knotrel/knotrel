# External source-of-truth integration contract

Status: Proposed (Candidate Design for Review — Awaiting Maintainer Approval).

Cross-reference: [Normative Integration Contract](../integration-contract.md).

## Context

Knotrel v0.1.0 provides an in-memory dynamic connectivity accelerator for a single undirected graph. All mutations (`POST /v1/operations`, `POST /v1/batch`) apply directly to memory. State is strictly volatile: restart loses all graph data.

In production architectures, an external database or transactional event stream remains the authoritative source of truth. Knotrel must serve as a derived read accelerator. To prevent data corruption and divergence, Knotrel must never claim that applying an update in memory makes it durable in the authoritative source. Furthermore, untracked direct client writes must not bypass source ordering, and process restart must trigger a structured rebuild from the external source rather than presenting an empty graph as synchronized.

Feature 0.2.1 establishes the architectural contract for v0.2.0 (Integration & Graph Loading) prior to any runtime implementation.

## Decision

### 1. Two Operating Modes
- **Standalone Volatile Mode (Default):** Retains 100% compatibility with v0.1.0. Direct mutations via `/v1/operations` and `/v1/batch` are accepted. Readiness transitions to `ready` at startup.
- **Source-Managed Mode (Opt-In):** Explicitly selected at startup (`KNOTREL_MODE=source-managed`). Direct mutating operations (`add_node`, `link`, `cut`) and mutating batches are rejected with HTTP 409 Conflict (`direct_mutation_disabled`). Connectivity queries remain supported. Readiness is false until verified synchronization. Restart discards state and requires full external rebuilding.

### 2. Identity, Instance Scoping, and Fencing
- **Instance ID:** Ephemeral UUID generated on startup; changes on every restart. Required in `X-Knotrel-Instance-Id` across all feeder requests. Requests bearing a stale `instance_id` are rejected immediately with HTTP 409 (`stale_instance_id`), preventing cross-restart generation collisions.
- **Graph ID:** Single implicit logical graph identifier (`"default"`).
- **Generation:** Monotonic 64-bit integer ($1, 2, \dots$) incremented on each atomic graph import. Updates must match the active generation; older generations are rejected (`stale_generation`).
- **State Version:** Preserves v0.1.0 arbitrary-precision decimal string. Increments only on topological changes (`changed: true`). Accepted no-op events advance source position without incrementing `state_version`.
- **Source Lineage:** `source_id` and `source_epoch` bound to the generation. Source failover or epoch changes reject updates (`source_epoch_mismatch`) and require a full rebuild.
- **Feeder Lease Protocol:** Explicit session registration (`POST /v1/feeder/session`) and periodic heartbeats (`POST /v1/feeder/heartbeat`) govern active feeder ownership. HTTP connection drops do not expire ownership; monotonic lease expiration (proposed default 10s timeout) transitions state to `Stale`. Higher `feeder_epoch` fences stale feeders (`fenced_feeder_session`). Heartbeats renew the lease deadline but cannot restore `Ready` without synchronization proof.

### 3. Synchronization Protocol ($S \to \text{Catch-up} \to B$)
- **Image Import ($S$):** The feeder delivers a coherent baseline graph image at source position $S$. Mode is explicitly `create_only` or `replace`. Offline candidate build is bounded by a proposed 64 MiB payload limit and a concurrency ceiling of 1 (`KNOTREL_MAX_CONCURRENT_IMPORTS = 1`). Atomic swap under graph mutex establishes generation $g$ and enters `CatchingUp`.
  - **Error Segregation on Import:** Calling `create_only` when `generation > 0` returns `409 Conflict` (`graph_already_initialized`). For `replace`, composite precondition mismatch on `If-Match: "<gen>.<state_version>.<percent_encoded_source_position>"` returns `412 Precondition Failed` (`precondition_failed`). Fencing or lease failures return `409 Conflict` (`fenced_feeder_session`, `lease_expired`).
- **Verifiable Continuity & Execution Boundary:** External cursors are treated as opaque strings. The feeder provides a transport continuity envelope: contiguous `delivery_sequence` ($1, 2, 3, \dots$), `prev_delivery_sequence`, and predecessor cursor `prev_source_position`.
- **Ordered Progress Events:** Source progress without graph mutations is advanced **solely** through ordered, deduplicated `progress` events (`op: "progress"`).
- **Mutually Exclusive Barrier Evaluation Table ($B$):** The feeder declares barrier $B$ via `POST /v1/feeder/barrier`. Barriers are strictly verification-only: they assert that the stream has applied updates and progress up to $B$ ($D_B, P_B$) and never mutate topology or advance cursors.
  - Evaluation requires valid feeder ownership (`feeder_session_id`, `feeder_epoch`), an active unexpired monotonic lease, matching `instance_id`, and matching `generation` (mismatches return `409 Conflict` while preserving state).
  - Mutually exclusive decision rules:
    - **Exact Match ($D_B == D_{applied} \land P_B == P_{applied}$):**
      - If `CatchingUp` or `Stale` (published $g > 0$), returns `200 OK` (`result: "synchronized"`) and restores **`Ready`** (Case B1).
      - If already `Ready` (published $g > 0$), returns `200 OK` (`result: "already_synchronized"`) and remains **`Ready`** (Case B2).
      - If `Uninitialized` ($g == 0$), returns `409 Conflict` (`code: "graph_uninitialized"`) and remains **`Uninitialized`** (Case B3; no barrier may make an unpublished initial graph `Ready`).
      - If initial baseline `Loading` ($g == 0$), returns `409 Conflict` (`code: "graph_uninitialized"`) and remains **`Loading`** (Case B4).
      - If `Desynchronized`, returns `409 Conflict` (`code: "graph_desynchronized"`) and remains **`Desynchronized`** (Case B5; barrier cannot clear gap).
      - If `Failed`, returns `503 Service Unavailable` (`code: "unavailable"`) and remains **`Failed`** (Case B6).
    - **Position Mismatch at Equal Sequence ($D_B == D_{applied} \land P_B \ne P_{applied}$):** Returns `409 Conflict` (`code: "barrier_position_mismatch"`) and preserves state (Case B7).
    - **Premature Sequence ($D_B > D_{applied}$):** Returns `409 Conflict` (`code: "barrier_not_reached"`) and preserves state (Case B8).
    - **Older Sequence ($D_B < D_{applied}$):** Conservatively rejected with `409 Conflict` (`code: "superseded_barrier"`) and preserves state (Case B9). Because Knotrel maintains only the current stream frontier $(D_{applied}, P_{applied})$ and recent operation descriptors in its ring buffer without retaining an unbounded historical position ledger, historical matches cannot be proven. Conservative rejection prevents delayed or reordered barriers from regressing or altering synchronization state.
  - **Prohibition of Barriers Making Unpublished Graphs Ready:** No barrier can make an unpublished graph `Ready`. In `Uninitialized` and initial baseline `Loading` ($g == 0$), all barrier attempts are rejected with `409 graph_uninitialized`.
  - **Relationship Between Replacement Loading and Published Graph:** While a replacement candidate $g+1$ is constructed off-lock, the live published graph remains active at generation $g$. Barriers for generation $g$ evaluate against generation $g$'s stream frontier normally. Barriers for candidate $g+1$ are rejected with `409 Conflict` (`code: "stale_generation"`), preserving generation $g$'s live state. Only upon atomic swap under lock is generation $g+1$ published.
  - **State Preservation Invariant:** Every rejected barrier (B3 through B9) strictly preserves the existing synchronization state.
- **Sequence Classification and Dual-Index Deduplication under Mutex:** Request sequence is evaluated under the graph mutex using a bounded window of capacity $N = 10{,}000$ spanning $[D_{window\_min}, D_{applied}]$:
  - **Previous Sequence ($D \le D_{applied}$):**
    - If evicted ($D < D_{window\_min}$): returns `409 Conflict` (`code: "unverifiable_duplicate"`); state unchanged.
    - If in window ($D \in [D_{window\_min}, D_{applied}]$):
      - Retained sequence with different event ID (`req.event_id != retained.event_id`): returns `409 Conflict` (`code: "conflicting_duplicate"`) $\to$ transitions state to `Desynchronized`.
      - Retained sequence with matching event ID: Tier 2 canonical check. If canonical content matches, returns `200 OK` (`status: "already_applied"`). If canonical content differs, returns `409 Conflict` (`code: "conflicting_duplicate"`) $\to$ transitions state to `Desynchronized`.
  - **Next Sequence ($D == D_{applied} + 1$):**
    - Predecessor sequence check verifies $D_{prev} == D_{applied}$. Predecessor position check verifies `event.prev_source_position == live.applied_source_position` (mismatch returns `409 Conflict` (`code: "source_predecessor_mismatch"`) $\to$ `Desynchronized`).
    - Event ID uniqueness check in window: if `req.event_id` exists in the retained `event_id` hash index, returns `409 Conflict` (`code: "conflicting_duplicate"`) $\to$ transitions state to `Desynchronized`.
    - If not in window: executes mutation or progress event. On success, increments `state_version` (if changed), advances `applied_source_position` and $D_{applied}$, and records entry in ring buffer and `event_id` hash index (evicting oldest entry if $> N$).
    - Evicted history boundary: if an incoming event at $D_{applied} + 1$ reuses an `event_id` from an evicted sequence ($< D_{window\_min}$), the bounded window cannot detect the historical reuse; it evaluates as fresh. Detection guarantees hold strictly within the retained $N$-entry window.
  - **Future Sequence ($D > D_{applied} + 1$):** Rejected as `409 Conflict` (`code: "out_of_order_sequence"`). Live state remains at $D_{applied}$.
- **Rejected Events vs. Committed Events:**
  - The deduplication window records **only successfully committed events** ($D \le D_{applied}$).
  - An uncommitted rejected attempt (e.g. failing domain validation like `self_loop` or capacity limit rejection) is **not recorded** in the deduplication window. Retrying at sequence $D_{applied} + 1$ with corrected payload is evaluated fresh and can succeed without triggering `conflicting_duplicate`.
  - In contrast, once an event is committed ($D \le D_{applied}$), any reuse of its sequence with a different event ID, reuse of its event ID at a new sequence, or alteration of its canonical content triggers `409 Conflict` (`code: "conflicting_duplicate"`) $\to$ `Desynchronized`.
- **Progress Invariants on Failed Mutations:**
  - A `progress` event is permitted **only** when the corrected authoritative source projection requires no graph mutation.
  - Feeder is **strictly forbidden** from substituting a `progress` event to skip or bypass a required graph mutation that failed (e.g., due to capacity limits such as `node_limit_exceeded` or `edge_limit_exceeded`). Bypassing a required mutation causes silent, unrecoverable topological divergence.
  - Required mutation failures require halting stream progress and executing either operator capacity reconfiguration followed by full external rebuild, or declaring an unrepairable source gap via `POST /v1/feeder/gap`.
- **Declared Gap:** Irreparable source loss declared via `POST /v1/feeder/gap` returns `200 OK` (`result: "gap_acknowledged"`) and transitions state to `Desynchronized`.

### 4. Readiness and Freshness
- Mode-specific readiness states: `Uninitialized`, `Loading`, `CatchingUp`, `Ready` (Synchronized), `Disconnected`/`Stale`, `Desynchronized`, `Failed`.
- Queries executed in source-managed mode expose coherent metadata headers (`X-Knotrel-Instance-Id`, `X-Knotrel-Generation`, `X-Knotrel-State-Version`, `X-Knotrel-Source-Position`, `X-Knotrel-Sync-Status`).
- A graph known to be divergent or lagging past an unrepairable gap is **never** reported as synchronized.

### 5. Roadmap Decisions D1–D6 Resolution
- **D1 (Identity):** Single implicit graph with `instance_id` header validation, monotonic `generation`, and `feeder_epoch` lease fencing.
- **D2 (Modes & Ack):** Strict startup segregation via `KNOTREL_MODE`. Direct writes rejected in source-managed mode (`direct_mutation_disabled`). Ack certifies volatile memory application only.
- **D3 (Ordering & Retry):** Feeder-supplied `delivery_sequence` and `prev_source_position` with sequence classification, dual-index bounded deduplication under the mutex (sequence ring buffer and `event_id` hash index), and verification-only barriers with a complete 9-case decision table. Conflicting duplicate detection covers sequence reassignment, event ID reuse, and canonical content alterations within the retained $N$-entry horizon, while evicted sequences return `409 unverifiable_duplicate`. Uncommitted rejected attempts are not recorded in the deduplication window and may be retried at $D_{applied}+1$. Progress events cannot bypass failed required mutations. Deduplication memory accounting requires explicit tracking of ring buffer storage, hash index buckets, string heap allocations (max 64 B `event_id`, max 256 B `source_position`), canonical payload descriptors, and allocator overhead for the proposed 10,000-entry window without invented numerical guarantees. Fail-fast batch semantics; legacy `/v1/batch` continuation preserved in standalone mode.
- **D4 (Capacity & Freshness):** Lease-driven discrete freshness states without unsupported millisecond SLOs. Heartbeats renew lease but do not bypass catch-up. Proposed 64 MiB default import limit, 1 concurrent import permit.
- **D5 (Import & Publication):** Offline candidate build with atomic pointer swap. Create-only vs. mandatory composite percent-encoded precondition (`If-Match`) for replacement. Post-build mutex recheck of ownership and preconditions. Strict error segregation: composite precondition mismatch returns `412 Precondition Failed` (`precondition_failed`), whereas ownership, fencing, and state conflicts return `409 Conflict` (`graph_already_initialized`, `fenced_feeder_session`, `lease_expired`). Readers never observe partial state.
- **D6 (Trust Boundary):** Partitioned query vs feeder surfaces. Optional feeder token (`KNOTREL_FEEDER_TOKEN`). Reverse proxy required for untrusted networks; M8 brought forward if public exposure is required.

## Consequences & Compatibility

- **v0.1.0 Backward Compatibility:** Preserved completely in standalone mode. Existing Rust crate APIs, REST contracts, batch semantics, and arbitrary-precision decimal revisions remain unchanged.
- **Zero Durability Promises:** The system explicitly documents that in-memory acceptance does not guarantee persistence in the authoritative source or recovery after restart.
- **Implementation Isolation:** Features 0.2.2–0.2.6 can implement import, metadata exposure, ordered streaming, and readiness probes against this candidate specification once approved by the maintainer.

## Deterministic Testing and Handoff

The contract specifies 34 deterministic traces (TR-01 through TR-34) covering happy paths, stale instance IDs, duplicate events, transient out-of-order sequences, worker timeouts with controlled interleavings, replacement during catch-up, fenced feeders, epoch changes, history expiration, process restart, verification-only barriers, progress-event retries, lease expiry vs synchronization proof, session takeovers during build, predecessor mismatches, hash collisions with 256-byte cursors, partial batch global sequences, declared gaps, equal barrier sequences with mismatching positions (TR-15), older superseded barriers (TR-24), rejected barriers preserving server states (TR-25), uncommitted event retry vs committed duplicate conflict (TR-26), prohibition of progress on failed required mutations (TR-27), import precondition failures vs ownership fencing (TR-28), exact barrier matches while uninitialized (TR-29), barriers during initial baseline loading (TR-30), barriers during replacement loading (TR-31), retained committed sequences with different event IDs (TR-32), retained event IDs reused at new sequences (TR-33), and evicted history boundaries (TR-34).

Feature 0.2.2 is handed an exact candidate interface definition for bounded atomic HTTP import (`POST /v1/feeder/import`) with preconditions, limits, and atomic publication invariants.
