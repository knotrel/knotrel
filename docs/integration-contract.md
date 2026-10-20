# Knotrel External Source-of-Truth Integration Contract

**Document Status:** Candidate Design Specification for Review — PENDING MAINTAINER APPROVAL  
**Milestone:** v0.2.0 — Integration & Graph Loading (Feature 0.2.1)  
**License:** Elastic License 2.0  
**Target Platform:** Rust 1.98.1, Edition 2024  
**Implementation Notice:** All endpoints, schemas, headers, and protocol messages specified herein that are not part of the v0.1.0 release contract are marked as **`[PROPOSED — NOT IMPLEMENTED]`**. They define the candidate contract to be implemented by Features 0.2.2–0.2.6 upon maintainer approval.

---

## 1. Purpose and Non-Goals

### 1.1 Purpose
Knotrel is an in-memory dynamic connectivity accelerator for undirected graphs. An external system (such as an authoritative transactional database, an event stream, or an application cluster) remains the sole authoritative source of graph truth.

The purpose of this contract is to specify:
1. The boundary and responsibilities between Knotrel and the external system (the **Feeder**).
2. Two formal operating modes: default **Standalone Volatile Mode** and opt-in **Source-Managed Mode**.
3. The synchronization protocol: coherent initial graph image at source position $S$, incremental ordered update streaming, deduplicated progress events, and verification-only synchronization barriers $B$.
4. The identity, versioning, generation, instance scoping, and feeder lease/fencing model.
5. The acknowledgment, visibility, retry, exact bounded duplicate-detection, and failure semantics.
6. The synchronization-aware lifecycle, readiness states, lease tracking, and freshness visibility.
7. The deterministic acceptance-test specification matrix and Feature 0.2.2 implementation handoff.

### 1.2 Non-Goals
The following capabilities are explicitly out of scope for v0.2.0:
- **Local Durability:** Knotrel does not provide local Write-Ahead Logging (WAL), Append-Only Files (AOF), disk snapshots, or local crash recovery in v0.2.0. Applying an update in memory **never** implies durability in the authoritative store.
- **Provider-Specific Connectors:** Knotrel does not embed database-specific drivers, Kafka/Postgres/MySQL connectors, or CDC parsers. An external feeder is responsible for reading the source and calling Knotrel's transport API.
- **Multi-Graph Routing:** Knotrel continues to own exactly one graph per server instance. Logical graph identifiers are used for validation and fencing, not dynamic multi-tenancy.
- **Transaction Rollback:** Knotrel executes operations in memory without transactional undo or two-phase commit rollback.
- **Distributed Consensus:** Knotrel is a single-process accelerator; distributed consensus, active-active multi-leader replication, and partition merging remain feeder responsibilities.

---

## 2. Operating Modes and Selection

Knotrel defines two mutually exclusive operating modes configured at startup:

```
+-----------------------------------------------------------------------------------------+
|                                    KNOTREL MODES                                        |
+-----------------------------------------------------------------------------------------+
|  Standalone Volatile Mode (Default)        |  Source-Managed Mode (Opt-In)              |
|  - KNOTREL_MODE=standalone                 |  - KNOTREL_MODE=source-managed             |
|  - Self-contained, in-memory graph         |  - Graph derived from external source      |
|  - Mutating /v1/operations & /v1/batch OK  |  - Direct mutating REST operations BLOCKED |
|  - state_version increments on changes     |  - Graph advanced solely by Feeder protocol|
|  - Restart starts empty or from supplied   |  - Restart requires full external rebuild  |
|  - Ready immediately after startup         |  - Ready ONLY after verified barrier B     |
+-----------------------------------------------------------------------------------------+
```

### 2.1 Standalone Volatile Mode (Default)
- **Selection:** Selected when `KNOTREL_MODE` is unset or set to `standalone`.
- **Compatibility:** 100% backward compatible with Knotrel v0.1.0 preview.
- **Graph Initialization:** Starts empty (for the standalone binary) or from the user-supplied `Graph` (for library embedders calling `router(graph)`).
- **Mutations:** Direct client mutations via `POST /v1/operations` (`add_node`, `link`, `cut`) and `POST /v1/batch` are fully supported.
- **Readiness:** `/ready` transitions from `starting` to `ready` as soon as the listener begins serving, and reports 503 only during shutdown (`draining`) or worker panic (`unavailable`).
- **State Version:** Tracks local mutations using the arbitrary-precision decimal counter (`"0"`, `"1"`, `"2"`, ...) established in v0.1.0.

### 2.2 Source-Managed Mode (Opt-In)
- **Selection:** Explicitly selected at startup via `KNOTREL_MODE=source-managed` (or `ServerConfig::with_mode(Mode::SourceManaged)` for embedded callers).
- **Graph Authority:** Knotrel is a derived read accelerator. The external data store is the authoritative source of truth.
- **Direct Mutation Prohibition:** Direct client mutations via `POST /v1/operations` (`add_node`, `link`, `cut`) and mutating operations in `POST /v1/batch` are **rejected** with HTTP 409 Conflict (code: `direct_mutation_disabled`). Clients must not bypass source ordering.
- **Queries:** Connectivity queries (`POST /v1/operations` with `op: "connected"`) and batch queries remain supported, subject to the readiness and freshness policy.
- **Readiness:** `/ready` reports 503 during bootstrap (`uninitialized`, `loading`, `catching_up`) and transitions to 200 `ready` only after an initial coherent graph image is published and verified through a synchronization barrier $B$.
- **Restart Semantics:** Process restart completely discards in-memory volatile graph state. Upon restart, the server re-enters `uninitialized` with a new `instance_id` and requires the external feeder to perform a full bootstrap.

### 2.3 Mode Admission & Route Enforcement Matrix

| Route | Method | Standalone Volatile Mode | Source-Managed Mode |
|---|---|---|---|
| `/health` | GET | 200 `{"status":"ok"}` | 200 `{"status":"ok"}` |
| `/ready` | GET | 200 `ready` (503 if starting/draining/unavailable) | 200 `ready` ONLY when synchronized; 503 during bootstrap, lag, gap, or failure |
| `/v1/info` | GET | 200 (settings snapshot, `mode: "standalone"`) | 200 (settings snapshot, `mode: "source-managed"`, `instance_id`) |
| `/v1/stats` | GET | 200 (live node/edge counts, `state_version`) | 200 (counts, `instance_id`, `state_version`, `generation`, `source_position`, `sync_status`) |
| `/v1/operations` (`connected`) | POST | 200 (connectivity outcome) | 200 (if ready/stale-serving allowed; 503 if uninitialized/loading) |
| `/v1/operations` (`add_node`/`link`/`cut`) | POST | 200/422 (applied under graph lock) | **409 Conflict** (`code: "direct_mutation_disabled"`) |
| `/v1/batch` (all queries) | POST | 200 (query outcomes) | 200 (if ready/stale-serving allowed; 503 if uninitialized/loading) |
| `/v1/batch` (contains $\ge 1$ mutation) | POST | 200 (runs with continue-on-error) | **409 Conflict** (`code: "direct_mutation_disabled"`) |
| `[PROPOSED]` `/v1/feeder/session` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: acquires or fences feeder session lease |
| `[PROPOSED]` `/v1/feeder/heartbeat` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: extends feeder lease deadline |
| `[PROPOSED]` `/v1/feeder/import` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: candidate built, published, generation bumped |
| `[PROPOSED]` `/v1/feeder/events` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: ordered updates applied under graph lock |
| `[PROPOSED]` `/v1/feeder/barrier` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: barrier verified, transitions to `ready` |
| `[PROPOSED]` `/v1/feeder/gap` | POST | **404 Not Found** or **409 Mode Mismatch** | Admitted: declares source history loss $\to$ `Desynchronized` |

---

## 3. Identity, Generations, Versions, and Fencing

To ensure deterministic synchronization and prevent split-brain anomalies or cross-restart collisions, Knotrel establishes an explicit identity and version hierarchy.

```
+-----------------------------------------------------------------------------------------------+
|                                      IDENTITY HIERARCHY                                       |
+-----------------------------------------------------------------------------------------------+
| Service Instance Identity:  instance_id (UUID, ephemeral, changes every process restart)      |
|   |                                                                                           |
|   +--> Feeder Lease:  feeder_session_id & feeder_epoch (fencing token, renewed by heartbeat)  |
|   |                                                                                           |
|   +--> Logical Graph Identity:  graph_id (string, e.g. "default", defines projection)         |
|         |                                                                                     |
|         +--> Published Generation:  generation (u64, 1, 2, ..., scoped to instance_id)       |
|               |                                                                               |
|               +--> Source Lineage:  source_id & source_epoch (source branch/failover era)     |
|               |                                                                               |
|               +--> Source Position:  applied_source_position (opaque string, advances on op)  |
|               |                                                                               |
|               +--> Local Revision:  state_version (decimal string, advances on topology chg)  |
+-----------------------------------------------------------------------------------------------+
```

### 3.1 Token Definitions

1. **Service Instance Identity (`instance_id`):**
   - Type: Ephemeral string (UUIDv4 generated at process startup, e.g. `"srv-550e8400-e29b-41d4-a716-446655440000"`).
   - Invariant: **Changes on every process launch/restart.**
   - Discovery: Exposed in `GET /ready`, `GET /v1/info`, and `GET /v1/stats`.
   - **Enforcement Rule:** Every feeder request (`import`, `session`, `heartbeat`, `events`, `barrier`, `gap`) **must** supply `instance_id` in the HTTP header `X-Knotrel-Instance-Id` (and in the JSON payload). If the request `instance_id` does not match the active server `instance_id`, Knotrel rejects it immediately with **`409 Conflict` (`code: "stale_instance_id"`)** before reading the graph or acquiring permits. This prevents stale in-flight requests from a previous incarnation from colliding with a new incarnation that happens to be at generation 1.

2. **Logical Graph Identity (`graph_id`):**
   - Type: UTF-8 string (1–64 characters, `[a-zA-Z0-9_-]+`).
   - Scope: Identifies the logical graph dataset or projection. Default: `"default"`.
   - Invariant: A Knotrel instance validates `graph_id` in every feeder payload. Mismatched IDs are rejected with HTTP 400 (`invalid_graph_id`).

3. **Published Graph Generation (`generation`):**
   - Type: Unsigned 64-bit integer (`u64`), represented as a decimal integer or string in JSON.
   - Lifecycle: Starts at `0` before any import. Set to `1` on the initial baseline import, and increments monotonically ($g \to g + 1$) on every subsequent atomic graph replacement.
   - Scoping: Generational lineage is strictly scoped to `instance_id`.
   - Invariant: Every incremental event, progress event, and barrier declaration **must** specify the exact current `generation`. Any request bearing an older or newer generation is rejected immediately with HTTP 409 (`stale_generation`).

4. **Local State Revision (`state_version`):**
   - Type: Arbitrary-precision decimal string (`"0"`, `"1"`, `"2"`, ...), encoded as little-endian digits internally without fixed-width integer wraparound.
   - Semantics: Preserves v0.1.0 behavior. Starts at `"0"` upon service creation or upon publication of a new generation image. Increments by 1 if and only if an applied operation results in a topological change (`changed: true`).
   - Invariant: An accepted source event that is an effective graph no-op (e.g. linking an edge that already exists) or an explicit `progress` event **does not increment `state_version`**, but **does advance `applied_source_position`**.

5. **Source Identity (`source_id`):**
   - Type: UTF-8 string identifying the external authoritative store or database cluster (e.g., `"pg-cluster-prod-01"` or `"kafka-graph-events"`).
   - Invariant: Bound at import. Updates from another `source_id` are rejected with HTTP 409 (`source_mismatch`).

6. **Source Epoch (`source_epoch`):**
   - Type: UTF-8 string or 64-bit integer identifying the timeline, failover epoch, or schema version of the source stream.
   - Invariant: If the external database undergoes failover, timeline reset, or schema change, it increments its epoch. An event carrying a different `source_epoch` is rejected with HTTP 409 (`source_epoch_mismatch`) and forces a full graph rebuild.

7. **Applied Source Position (`applied_source_position`):**
   - Type: Opaque UTF-8 string (1 to 256 bytes) representing the external cursor (e.g. Postgres LSN `"0/16B3740"`, Kafka offset `"104928"`, or hybrid token `"epoch=3,pos=9912"`).
   - Invariant: Knotrel **never** interprets this string as an integer, never parses its structure, and never compares cursors lexicographically. It treats the cursor as an opaque token received from the feeder.

8. **Feeder Session Identity & Lease Fencing (`feeder_session_id`, `feeder_epoch`):**
   - `feeder_session_id`: Unique string identifying the active feeder process instance.
   - `feeder_epoch`: Monotonically increasing 64-bit integer fencing token.
   - Invariant: Only requests bearing the currently registered `feeder_session_id` and matching `feeder_epoch` are admitted to apply updates. A request bearing an older `feeder_epoch` is rejected with HTTP 409 (`fenced_feeder_session`).

---

## 4. Feeder Session and Lease Protocol

Ownership and readiness in source-managed mode are governed by an explicit **Lease Protocol**, decoupled from underlying HTTP TCP connections.

### 4.1 Session Acquisition and Transitions
To become the active writer, a feeder registers a session:
- **Endpoint:** `POST /v1/feeder/session`
- **Payload:** `instance_id`, `feeder_session_id`, `feeder_epoch`, `lease_duration_ms` (proposed default: 10,000 ms; bounds: 1,000 ms to 60,000 ms).
- **Clock:** Lease duration is evaluated exclusively against the host's monotonic clock (`std::time::Instant`), never wall-clock time.

#### Complete Feeder Session Transition Matrix

| Current Server Lease State | Incoming Session Request | Action & Response | Resulting Lease State |
|---|---|---|---|
| No active session (Startup) | Any `(session_A, epoch_1)` | Accept: set active session, initialize monotonic deadline. Return `200 session_acquired`. | Active `(session_A, epoch_1)` |
| Active `(session_A, epoch_1)` | Exact retry: `(session_A, epoch_1)` | Idempotent: extend monotonic deadline. Return `200 session_acquired`. | Active `(session_A, epoch_1)` |
| Active `(session_A, epoch_1)` | Same session, lower epoch: `(session_A, epoch_0)` | Reject: `409 Conflict` (`fenced_feeder_session`). | Active `(session_A, epoch_1)` |
| Active `(session_A, epoch_1)` | Same session, higher epoch: `(session_A, epoch_2)` | Accept: update epoch to 2, extend monotonic deadline. Return `200 session_acquired`. | Active `(session_A, epoch_2)` |
| Active `(session_A, epoch_1)` | Another session, higher epoch: `(session_B, epoch_2)` | **Takeover Accepted:** preempt session A, set active session to B, reset monotonic deadline. Return `200 session_acquired`. | Active `(session_B, epoch_2)` |
| Active `(session_A, epoch_1)` | Another session, lower/equal epoch: `(session_B, epoch_1)` | Reject: `409 Conflict` (`fenced_feeder_session`). | Active `(session_A, epoch_1)` |
| Active `(session_A, epoch_1)` | Monotonic deadline elapses without heartbeat | **Lease Expired:** ownership remains assigned to session A, but lease is expired. State transitions to `Disconnected`/`Stale`, recording `last_expired_epoch = 1`. | Expired `(session_A, epoch_1)` |
| Expired `(session_A, epoch_1)` | Heartbeat from `(session_A, epoch_1)` | **Lease Renewed:** extends deadline. **Readiness remains `Stale`** until synchronization recovery proof under a newly registered epoch (§4.3). Return `200 heartbeat_acknowledged`. | Active `(session_A, epoch_1)` |
| Any active lease | Requests queued in worker pool across takeover | Under mutex, queued request observes `live.feeder_epoch > req.feeder_epoch`. **Fenced:** rejected with `409 fenced_feeder_session`. | Unchanged |

### 4.2 Independent Observation of Lease Expiry
Lease expiration is checked independently of slow graph worker jobs:
- The server maintains a lock-free atomic word (`PublishedReadiness`) encoding status, publication sequence, and monotonic deadline millis.
- Probes (`GET /ready`), stats (`GET /v1/stats`), and request admission gates read this atomic word without acquiring the graph lock.
- If the current monotonic clock exceeds the published deadline:
  - If server phase was `Ready`, it is immediately observed as **503 `status: "stale"`**, and `/ready` attempts a lock-free CAS to update the published status word to `Stale` without overwriting newer publications.
  - A long-running graph worker traversal cannot block or delay detection of feeder abandonment.

### 4.3 Lease Renewal vs. Synchronization Proof & Recovery Fencing
> [!IMPORTANT]
> **A heartbeat proves feeder liveness, NOT graph catch-up.**
- When a lease expires, Knotrel transitions to `Disconnected`/`Stale` and records the expired epoch (`last_expired_epoch`).
- A subsequent valid heartbeat (`POST /v1/feeder/heartbeat`) successfully renews the lease deadline, but **DOES NOT restore readiness to `Ready`**.
- **Explicit Recovery-Fencing Across Expiry:** To prevent delayed, obsolete in-flight barriers created before expiry from executing after a heartbeat and resurrecting `Ready` without fresh proof, Knotrel explicitly fences synchronization recovery:
  - Any barrier submitted under `feeder_epoch <= last_expired_epoch` is rejected with `409 Conflict` (`code: "fenced_feeder_session"`).
  - To restore `Ready` after lease expiry, the feeder **must** register a strictly newer epoch (`POST /v1/feeder/session` with `feeder_epoch > last_expired_epoch`).
  - Under the new epoch, the feeder supplies fresh verification proof that Knotrel is synchronized with the source:
    1. If source updates were produced during the disconnect, the feeder streams them followed by barrier $B$.
    2. If zero source updates were produced, the feeder issues an exact verification barrier (`POST /v1/feeder/barrier`) matching current $D_{applied}$ and $P_{applied}$, affirming no changes occurred.
- Heartbeats **must never** clear `Failed`, `Desynchronized`, `Uninitialized`, `Loading`, or `CatchingUp` states.

---

## 5. Source-to-Knotrel Synchronization Protocol

The synchronization protocol coordinates the external feeder and Knotrel through an explicit state machine.

```
                      +-------------------+
                      |   UNINITIALIZED   |
                      +-------------------+
                                |
                                | [Feeder: POST /v1/feeder/import (Image S)]
                                v
                      +-------------------+
                      |      LOADING      |
                      +-------------------+
                                |
                                | [Import Validated & Published as Gen g]
                                v
                      +-------------------+
           +--------> |    CATCHING_UP    | <--------+
           |          +-------------------+          |
           |                    |                    |
           | [More Updates/     | [Feeder: POST /v1/feeder/barrier (Pos B)]
           |  Progress Events]  v                    |
           |          +-------------------+          |
           +--------- | READY/SYNCHRONIZED| ---------+
                      +-------------------+
                           |         |
     [Lease Expiration /   |         | [Declared Unrepairable Gap /
      Heartbeat Timeout]   |         |  Source Epoch Mismatch]
                           v         v
                 +--------------+  +-----------------+
                 | STALE/DISCONN|  | DESYNCHRONIZED  |
                 +--------------+  +-----------------+
                         |                 |
                         | [Feeder Proves  | [Full Rebuild Required]
                         |  Sync at B]     |
                         +-----------------+
```

### 5.1 Verifiable Continuity: Delivery Sequences and Predecessor Tokens
Because opaque external cursors cannot be verified as contiguous by Knotrel, the feeder **must** supply a transport-level continuity envelope for every event:

- `delivery_sequence` ($D$): A strictly increasing 64-bit integer, starting at `1` for the first update following import $S$, incrementing by exactly `1` for each subsequent update ($1, 2, 3, \dots$).
- `prev_delivery_sequence` ($D_{prev}$): The delivery sequence of the immediate predecessor ($0$ for the first event).
- `source_position` ($P$): The opaque source cursor for this event (up to 256 bytes UTF-8).
- `prev_source_position` ($P_{prev}$): The opaque source cursor of the predecessor (for $D=1$, matches snapshot position $S$).
- `event_id`: A unique string identifier (up to 64 bytes) for idempotent retry tracking.

### 5.2 Ordered Progress Events (Sole Mechanism for Non-Mutating Progress)
To advance `applied_source_position` through source transactions that affect tables unrelated to the graph (or produce zero graph mutations), the feeder **must** emit explicit `progress` events:
```json
{
  "event_id": "prog-1001",
  "delivery_sequence": 15,
  "prev_delivery_sequence": 14,
  "source_position": "0/16B4000",
  "prev_source_position": "0/16B3800",
  "op": "progress"
}
```
- A `progress` event is ordered, validated, and deduplicated identically to `link` and `cut`.
- Under the graph lock: advances `applied_source_position = event.source_position` and $D_{applied} \leftarrow D$.
- `state_version` is **not incremented** because no topological mutation occurred.
- Retries of `progress` events are deduplicated identically to other events.

### 5.3 Verification-Only Synchronization Barriers
A barrier declaration (`POST /v1/feeder/barrier`) is **strictly verification-only**:
- It **never** mutates graph topology.
- It **never** advances `applied_source_position` or `delivery_sequence`.
- It asserts that Knotrel has applied all events and progress up to barrier position $B$ ($D_{barrier}$, $P_{barrier}$).

#### Mutually Exclusive Barrier Evaluation Decision Table (Under Graph Lock)

Preconditions checked before table evaluation:
1. `req.instance_id == live.instance_id` (mismatch $\to$ `409 stale_instance_id`, state preserved).
2. `req.feeder_session_id == live.feeder_session_id` and `req.feeder_epoch == live.feeder_epoch` (mismatch $\to$ `409 fenced_feeder_session`, state preserved).
3. Monotonic lease is unexpired (`monotonic_now() <= live.lease_deadline`; expired $\to$ `409 lease_expired`, state preserved).
4. `req.generation == live.generation` (mismatch $\to$ `409 stale_generation`, state preserved).

Under the mutex, $(D_{barrier}, P_{barrier})$ is matched against $(D_{applied}, P_{applied})$:

| Case | Sequence Condition | Position Condition | Current Sync State | Outcome & HTTP Response | Resulting Sync State |
|---|---|---|---|---|---|
| **B1: Exact Match (Restores Ready)** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `CatchingUp` or `Stale` (published $g > 0$) | **`200 OK`** (`result: "synchronized"`) | Transitions to **`Ready`** |
| **B2: Exact Match (Affirms Ready)** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Ready` (published $g > 0$) | **`200 OK`** (`result: "already_synchronized"`) | Remains **`Ready`** |
| **B3: Exact Match While Uninitialized** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Uninitialized` ($g == 0$) | **`409 Conflict`** (`code: "graph_uninitialized"`) | Remains **`Uninitialized`** (no barrier may make an unpublished initial graph `Ready`) |
| **B4: Exact Match While Initial Loading** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Loading` (initial baseline, $g == 0$) | **`409 Conflict`** (`code: "graph_uninitialized"`) | Remains **`Loading`** (initial candidate unpublished) |
| **B5: Exact Match While Desynchronized** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Desynchronized` | **`409 Conflict`** (`code: "graph_desynchronized"`) | Remains **`Desynchronized`** (barrier cannot clear gap) |
| **B6: Exact Match While Failed** | $D_B == D_{applied}$ | $P_B == P_{applied}$ | `Failed` | **`503 Service Unavailable`** (`code: "unavailable"`) | Remains **`Failed`** (worker failed permanently) |
| **B7: Position Mismatch at Equal Sequence** | $D_B == D_{applied}$ | $P_B \ne P_{applied}$ | Any | **`409 Conflict`** (`code: "barrier_position_mismatch"`) | **Unchanged** (preserves existing state) |
| **B8: Premature Sequence** | $D_B > D_{applied}$ | Any | Any | **`409 Conflict`** (`code: "barrier_not_reached"`) | **Unchanged** (preserves existing state) |
| **B9: Older Sequence (Conservative Rejection)** | $D_B < D_{applied}$ | Any | Any | **`409 Conflict`** (`code: "superseded_barrier"`) | **Unchanged** (preserves existing state) |

#### Prohibition of Barriers Making Unpublished Graphs Ready
Under no circumstances may a barrier request make an unpublished graph `Ready`:
- In `Uninitialized` ($g == 0$), no baseline graph has been imported. Any barrier request—even if specifying $D_B = 0$ and matching $P_{applied} = \text{null}$—is rejected with **`409 Conflict` (`code: "graph_uninitialized"`)**, preserving `Uninitialized`.
- During initial baseline `Loading` ($g == 0$), an import candidate is being parsed and constructed off-lock. The graph has not been published to readers. Any barrier arriving during this phase is rejected with **`409 Conflict` (`code: "graph_uninitialized"`)**, preserving `Loading`.

#### Relationship Between Replacement Loading and the Currently Published Graph
When a replacement graph is imported (`mode: "replace"`):
1. The candidate graph is parsed and constructed in memory off-lock by a dedicated import worker thread.
2. During this background build, the *currently published live graph* remains active at `generation g`. It continues serving queries and processing incremental feeder updates under the mutex according to its active synchronization status (`Ready`, `CatchingUp`, or `Stale`).
3. If a barrier arrives specifying `generation == g`, it is evaluated strictly against generation $g$'s live stream frontier $(D_{applied}, P_{applied})$ using the decision table above (e.g. Case B1 restores `Ready`, Case B2 affirms `Ready`). It has **zero relationship** with the uncommitted replacement candidate.
4. If a barrier arrives specifying candidate `generation == g + 1` before publication, it is rejected by Precondition 4 with **`409 Conflict` (`code: "stale_generation"`)**, because generation $g + 1$ has not yet been published. The live state of generation $g$ is strictly preserved.
5. Only upon successful candidate construction, precondition verification under the mutex, and atomic pointer swap is generation $g + 1$ published. At that instant, `live.generation` becomes $g + 1$, $D_{applied}$ resets to 0, `applied_source_position` becomes snapshot $S$, and the server transitions to `CatchingUp`. From that moment onward, barriers for generation $g + 1$ are evaluated against the newly published baseline.

#### Conservative Rejection of Older Barriers (Case B9)
Proving whether an older barrier ($D_B < D_{applied}$) was historically valid would require retaining an unbounded historical ledger recording past $(D, P)$ checkpoints. Knotrel stores only the single current stream frontier $(D_{applied}, P_{applied})$ and recent operation descriptors in its ring buffer deduplication window, without a historical position ledger. Knotrel therefore avoids unsupported matching claims and conservatively rejects all older barriers with **`409 Conflict` (`code: "superseded_barrier"`)**. This rejection strictly preserves the existing synchronization state and prevents delayed or reordered barrier requests from altering state.

#### Conditions for Restoring Ready
A barrier request transitions the server from `CatchingUp` or `Stale` to `Ready` **strictly under Case B1**, which requires all of the following:
1. Valid feeder session ownership (`req.feeder_session_id == live.feeder_session_id` and `req.feeder_epoch == live.feeder_epoch`).
2. An active, unexpired monotonic lease (`monotonic_now() <= live.lease_deadline`).
3. Matching instance identity (`req.instance_id == live.instance_id`) and generation (`req.generation == live.generation`).
4. Exact delivery sequence AND position equality ($D_B == D_{applied} \land P_B == P_{applied}$).

Case B2 affirms that an already `Ready` server remains `Ready` (`200 OK`, `result: "already_synchronized"`). If the server is in `Desynchronized` (Case B5) or `Failed` (Case B6), matching sequence and position cannot bypass the failure, and the barrier is rejected while preserving the failure state.

> [!IMPORTANT]
> **State Preservation Invariant:** Every rejected barrier (B3 through B9) strictly preserves the existing synchronization state without alteration. Because barriers never modify `applied_source_position`, delayed or reordered barriers can **never** regress or corrupt the source cursor.

---

## 6. Execution Boundary, Predecessor Validation, and Sequence Classification

To eliminate race conditions between deduplication, predecessor checks, and sequence validation, all checks are evaluated **together under the exclusive graph lock**.

### 6.1 Request Validation and Execution Pipeline

```
[Phase 1: Pre-Lock Validation]
  1. Route & Method Validation (404/405)
  2. Content-Type is JSON (415 invalid_request)
  3. Body size within bound (413 invalid_request)
  4. JSON syntax valid (400 invalid_request)
  5. Schema shape, field constraints, node ID strings (422 invalid_request)
  6. Lifecycle check: not draining (503 shutting_down)
  7. Instance identity check: req.instance_id == live.instance_id (409 stale_instance_id)
  8. Feeder fencing check: req.feeder_epoch == live.feeder_epoch & session match (409 fenced_feeder_session)
  9. Generation precondition: req.generation == live.generation (409 stale_generation)
 10. Source lineage: req.source_id == live.source_id & req.source_epoch == live.source_epoch (409 source_mismatch / source_epoch_mismatch)
 11. Acquire worker permit from semaphore (503 busy)

[Phase 2: Under Exclusive Graph Lock]
 12. Re-verify instance, feeder lease, and generation tokens under lock.
 13. Sequence Classification of delivery_sequence D against applied sequence D_applied:

       +---------------------------------------------------------------------------------+
       |  Case A: D <= D_applied (Previous / Retried Sequence)                           |
       |    - If D < D_window_min (evicted from N-entry window):                         |
       |        return 409 Conflict (code: "unverifiable_duplicate"). State unchanged.   |
       |    - If D in [D_window_min, D_applied]: look up D in sequence ring buffer:      |
       |        * If req.event_id != retained.event_id:                                  |
       |            return 409 (conflicting_duplicate) -> state Desynchronized.          |
       |        * If req.event_id == retained.event_id: perform Tier 2 canonical check:  |
       |            - If canonical content matches: return 200 OK (already_applied).     |
       |            - If canonical content differs: return 409 (conflicting_duplicate)   |
       |              -> state Desynchronized (protocol violation).                      |
       +---------------------------------------------------------------------------------+
       |  Case B: D == D_applied + 1 (Expected Next Sequence)                           |
       |    - Verify D_prev == D_applied.                                                |
       |    - Source Predecessor Check: verify event.prev_source_position ==             |
       |      live.applied_source_position.                                              |
       |      * If mismatch: reject with 409 (source_predecessor_mismatch), transition   |
       |        state to Desynchronized, STOP progress.                                  |
       |    - Retained Event ID Uniqueness Check:                                        |
       |      * Check if req.event_id exists in retained event_id hash index:            |
       |        - If found: return 409 (conflicting_duplicate) -> state Desynchronized. |
       |          (retained event ID reused at new sequence).                            |
       |        - Note: an event ID evicted from window (> N events ago) cannot be       |
       |          detected and is evaluated as fresh at D_applied + 1.                   |
       |    - Execute graph mutation(s) or progress event:                               |
       |      * If domain error (self_loop / limit_exceeded): reject with 422,          |
       |        STOP progress. (Uncommitted rejected attempts NOT recorded in window).   |
       |      * If success:                                                              |
       |        - If changed: true (and not progress), increment state_version.          |
       |        - Advance applied_source_position = event.source_position.               |
       |        - Advance D_applied = D.                                                 |
       |        - Record in sequence ring buffer and event_id index (evicting oldest     |
       |          if window exceeds capacity N).                                         |
       |        - Return 200 OK (applied).                                               |
       +---------------------------------------------------------------------------------+
       |  Case C: D > D_applied + 1 (Future Sequence / Transport Gap)                    |
       |    - Reject request immediately: 409 Conflict (out_of_order_sequence).          |
       |    - Live graph state remains untouched at D_applied.                           |
       |    - Feeder must resend from expected sequence D_applied + 1.                   |
       +---------------------------------------------------------------------------------+
```

### 6.2 Source Predecessor Validation and Feeder Obligations
- **Validation:** For every new event ($D == D_{applied} + 1$), Knotrel verifies under the mutex that `event.prev_source_position == live.applied_source_position`.
- **Mismatch Result:** If `prev_source_position` does not match, Knotrel responds with **`409 Conflict` (`code: "source_predecessor_mismatch"`)** and transitions state to **`Desynchronized`**.
- **Feeder Obligation:** Sequence continuity proves transport order, but **cannot** prove that the feeder extracted all authoritative source transactions. Faithful snapshot extraction, transaction filtering, and projection remain feeder responsibilities.

### 6.3 Normative Sequence Jump and Gap Resolution Table

| Condition | Error Code | HTTP Status | Next Server State | Query Serving Behavior | Feeder Recovery Action | Condition to Return to Ready |
|---|---|---|---|---|---|---|
| **Transient Transport Out-of-Order** ($D > D_{applied} + 1$) | `out_of_order_sequence` | `409` | Unchanged (`CatchingUp` or `Ready`) | Continues serving queries if was `Ready` | Resend missing event starting at $D_{applied} + 1$ | When sequence reaches barrier $B$ |
| **Source Predecessor Mismatch** ($P_{prev} \ne P_{applied}$) | `source_predecessor_mismatch` | `409` | **`Desynchronized`** | All queries return `503 desynchronized` | Inspect source timeline; rebuild from fresh snapshot $S'$ | Full atomic replacement import at $S'$ followed by catch-up to $B'$ |
| **Declared Unrepairable Source Gap** (`POST /v1/feeder/gap`) | `gap_acknowledged` | `200` | **`Desynchronized`** | All queries return `503 desynchronized` | Abort incremental catch-up; obtain fresh snapshot $S'$ | Full atomic replacement import at $S'$ followed by catch-up to $B'$ |
| **Source Epoch Reset / Failover** (`source_epoch` mismatch) | `source_epoch_mismatch` | `409` | **`Desynchronized`** | All queries return `503 desynchronized` | Feeder must rebuild from authoritative source | Full atomic replacement import at new epoch |
| **Evicted Duplicate** ($D \le D_{applied}$, evicted from ring buffer) | `unverifiable_duplicate` | `409` | Unchanged | Continues serving queries if was `Ready` | Re-verify feeder tracking or initiate full rebuild if continuity lost | N/A (already ready if stream is active) |
| **Conflicting Duplicate** ($D \le D_{applied}$, content differs) | `conflicting_duplicate` | `409` | **`Desynchronized`** | All queries return `503 desynchronized` | Fatal feeder logic bug; inspect feeder event store | Full atomic replacement import |
| **Retained Sequence with Different Event ID** ($D \in [D_{win}, D_{app}]$, $ID \ne ID_{ret}$) | `conflicting_duplicate` | `409` | **`Desynchronized`** | All queries return `503 desynchronized` | Protocol violation: sequence reassigned; inspect feeder | Full atomic replacement import |
| **Retained Event ID Reused at New Sequence** ($D == D_{app}+1$, $ID \in \text{window}$) | `conflicting_duplicate` | `409` | **`Desynchronized`** | All queries return `503 desynchronized` | Protocol violation: event ID duplicate; inspect feeder | Full atomic replacement import |
| **Premature Barrier** ($B$ declared but $D_{applied} < D_{barrier}$) | `barrier_not_reached` | `409` | Remains `CatchingUp` | Queries return `503 catching_up` | Feeder streams remaining events up to $D_{barrier}$, then re-declares barrier | Barrier declaration succeeds |
| **Barrier Position Mismatch** ($D_B == D_{applied}$, $P_B \ne P_{applied}$) | `barrier_position_mismatch` | `409` | Unchanged (preserves state) | Queries return status based on current state | Reconcile feeder position tracking; stream missing events or re-declare matching barrier | Barrier declaration succeeds |
| **Older / Superseded Barrier** ($D_B < D_{applied}$) | `superseded_barrier` | `409` | Unchanged (preserves state) | Queries return status based on current state | Advance feeder barrier target to current frontier $D_{applied}$ and declare matching barrier | Barrier declaration succeeds |
| **Barrier While Uninitialized / Initial Loading** ($g == 0$) | `graph_uninitialized` | `409` | Unchanged (`Uninitialized` or `Loading`) | Queries return `503 uninitialized` or `503 loading` | Feeder must complete atomic import before declaring barrier | Initial import publishes baseline $S$ $\to$ `CatchingUp` |

---

## 7. Exact Bounded Deduplication and Resource Layout

Knotrel guarantees deterministic deduplication within a bounded window without relying on 64-bit hash equality as the sole proof of content match.

### 7.1 Deduplication Model, Dual-Index Structure, and Identity Semantics
- **Generation Scoping:** Deduplication records belong strictly to the active `generation`. When an import publishes generation $g+1$, previous records are flushed.
- **Retained Window Horizon:** The window stores up to $N$ entries (proposed default $N = 10{,}000$), spanning sequence numbers:
  $$[D_{window\_min}, D_{applied}] \quad \text{where} \quad D_{window\_min} = \max(1, D_{applied} - N + 1)$$
- **Dual-Index Data Structure:**
  1. *Sequence Ring Buffer:* Direct modulo indexing by `delivery_sequence % N` storing entries for the active contiguous sequence range $[D_{window\_min}, D_{applied}]$.
  2. *Event ID Hash Index:* Fast hash index mapping `event_id` $\to$ `delivery_sequence` for all active entries in the window.
- **Two-Tier Equality Verification:**
  - *Tier 1 (Indexing):* Fast sequence lookup in ring buffer and hash lookup in `event_id` index.
  - *Tier 2 (Canonical Equality):* Exact byte-for-byte comparison of:
    - `event_id` string (up to 64 bytes).
    - `source_position` string (up to 256 bytes).
    - Canonical operation descriptor: operation type (`link`, `cut`, `add_node`, `progress`) and normalized endpoints (`min(u,v)`, `max(u,v)` for undirected edges).
- **Mutually Exclusive Bounded Identity Decisions:**
  1. **Retained Committed Sequence with Different Event ID ($D \in [D_{window\_min}, D_{applied}]$, incoming `event_id != retained.event_id`):**
     Sequence $D$ was already committed with a different `event_id`.
     $\implies$ Rejected with **`409 Conflict` (`code: "conflicting_duplicate"`)**, transitions state to **`Desynchronized`**.
  2. **Retained Event ID Reused at New Sequence ($D == D_{applied} + 1$, `event_id` found in retained hash index):**
     The feeder is attempting to reuse an already-committed `event_id` under a new sequence number.
     $\implies$ Rejected with **`409 Conflict` (`code: "conflicting_duplicate"`)**, transitions state to **`Desynchronized`**.
  3. **Exact Retry ($D \in [D_{window\_min}, D_{applied}]$, incoming `event_id == retained.event_id`, Tier 2 matches):**
     Both identity and canonical payload match the committed event.
     $\implies$ Idempotent **`200 OK` (`status: "already_applied"`, `result: "duplicate"`)**. Live graph topology and `state_version` remain unmodified; $D_{applied}$ does not advance.
  4. **Conflicting Content ($D \in [D_{window\_min}, D_{applied}]$, incoming `event_id == retained.event_id`, Tier 2 differs):**
     Canonical operation, endpoints, or `source_position` differ from the committed record.
     $\implies$ Rejected with **`409 Conflict` (`code: "conflicting_duplicate"`)**, transitions state to **`Desynchronized`**.
  5. **Evicted History ($D < D_{window\_min}$ or Evicted `event_id` Reuse):**
     - *Evicted Sequence:* If an incoming request specifies $D \le D_{applied}$ with $D < D_{window\_min}$, the record has been evicted. Knotrel cannot verify canonical content and returns **`409 Conflict` (`code: "unverifiable_duplicate"`)**, leaving state unchanged.
     - *Evicted Event ID at New Sequence:* If an incoming request at $D == D_{applied} + 1$ reuses an `event_id` that was committed more than $N$ events ago ($< D_{window\_min}$), that historical event was evicted from both the ring buffer and the hash index. Knotrel **cannot detect** this historical reuse and evaluates the request as fresh at $D_{applied} + 1$. Knotrel guarantees deduplication and conflict detection **strictly within the retained bounded window of $N$ entries**, and explicitly disclaims detection outside this horizon.

### 7.2 Memory Accounting Requirements
- **Window Size ($N$):** Proposed default $N = 10{,}000$ entries (configurable at startup via `KNOTREL_DEDUP_WINDOW_SIZE`).
- **Entry Bounds:**
  - Max `event_id`: 64 bytes UTF-8.
  - Max `source_position`: 256 bytes UTF-8.
  - Fixed metadata: sequence integer, hash tokens, outcome flags.
- **Implementation Memory Accounting Requirements:**  
  The total memory consumed by the deduplication window depends on the data structure chosen by the implementation. An implementation must account for:
  1. The primary ring buffer / index collection storage.
  2. Hash indexing structures and bucket capacities.
  3. Retained string heap allocations for variable-length `event_id` and `source_position` values.
  4. Normalized canonical operation payload descriptors.
  5. Global allocator per-allocation bookkeeping overhead and fragmentation.  
  Implementations must strictly enforce the entry ceiling $N$ and field bounds without leaking memory across generations.
- **Complete Import Resource Accounting:**
  - Body buffering is gated: connection limits and single-permit semaphore (`KNOTREL_MAX_CONCURRENT_IMPORTS = 1`) check occurs **before** streaming request body bytes.
  - Total import peak memory must be accounted for across all phases:
    $$\text{Peak Memory} = \text{Old Live Graph (retained for reads)} + \text{Buffered Raw JSON Body (max 64 MiB)} + \text{Decoded AST Collections} + \text{Candidate Graph Structure} + \text{Queued Work}$$

---

## 8. Atomic Graph Import and Replacement Preconditions

Knotrel supports both initial baseline creation and conditional atomic replacement of the live graph:

### 8.1 Create-Only Mode vs. Replace Mode
In `POST /v1/feeder/import`, the payload explicitly declares `mode`:

1. **`mode: "create_only"`:**
   - Precondition: Valid **only** when Knotrel is in state `Uninitialized` (`generation == 0`).
   - If Knotrel already owns an active graph (`generation > 0`), rejected with **`409 Conflict` (`code: "graph_already_initialized"`)**.

2. **`mode: "replace"`:**
   - Precondition: **Mandatory composite precondition.**
   - Header format (RFC 3986 percent-encoded cursor):
     `If-Match: "<expected_generation>.<expected_state_version>.<percent_encoded_source_position>"`
   - Body format (explicit fields):
     `"expected_generation": u64`, `"expected_state_version": string`, `"expected_source_position": string`.
   - **Post-Build Mutex Recheck:** After candidate graph is built off-lock, worker acquires graph mutex and rechecks:
     1. Feeder lease and session epoch remain active and valid (failure $\to$ `409 fenced_feeder_session` or `409 lease_expired`).
     2. `live.generation == expected_generation`.
     3. `live.state_version == expected_state_version`.
     4. `live.applied_source_position == expected_source_position`.
   - If any precondition field mismatches (2, 3, or 4), candidate is discarded, lock released, and request rejected with **`412 Precondition Failed` (`code: "precondition_failed"`)**.

### 8.2 Atomic Publication Package
Under a single graph mutex acquisition, the following fields are atomically updated:
1. `live.graph = candidate.graph`
2. `live.generation = live.generation + 1`
3. `live.state_version = "0"`
4. `live.source_id = import.source_id`
5. `live.source_epoch = import.source_epoch`
6. `live.applied_source_position = import.source_position`
7. `live.delivery_sequence = 0`
8. `live.dedup_window.clear()`
9. `live.sync_status = CatchingUp`
10. `live.feeder_session_id = import.feeder_session_id`, `live.feeder_epoch = import.feeder_epoch`

---

## 9. Partial Batch Failures, Rejected Events, and Remediation

### 9.1 Partial Batch Failure Semantics (Non-Trivial Global Sequence Example)
Feeder event batches use strict **stop-on-first-error** semantics.

Consider a batch starting at global sequence $D = 101$ with 3 events:
- Index 0: $D = 101$ (succeeds)
- Index 1: $D = 102$ (succeeds)
- Index 2: $D = 103$ (fails: self-loop error)

#### Execution Outcome:
1. Events at $D = 101$ and $D = 102$ are applied and recorded in the deduplication window.
2. Event at $D = 103$ is rejected; $D_{applied}$ advances to $102$.
3. Knotrel responds with HTTP 422:
```json
{
  "result": "partial_failure",
  "code": "event_failed",
  "message": "batch stopped at batch index 2 (global sequence 103) due to domain error",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "generation": 1,
  "applied_delivery_sequence": 102,
  "applied_source_position": "0/16B3800",
  "state_version": "45",
  "failed_batch_index": 2,
  "failed_delivery_sequence": 103,
  "failed_event_id": "evt-8803",
  "error": {
    "code": "self_loop",
    "message": "self-loops are not permitted: node 5"
  }
}
```

### 9.2 Rejected Events vs. Committed Events and Progress Invariants

#### 1. Rejected Attempts are Not in Deduplication Window
- The deduplication window records **only successfully committed events** ($D \le D_{applied}$).
- A rejected event (e.g. failing at sequence $D_{applied} + 1$ due to `self_loop` or `edge_limit_exceeded`) is **never applied** to the graph and is **not recorded** in the deduplication window.
- Retrying an uncommitted sequence:
  - If retried with the exact same payload at $D_{applied} + 1$, it is evaluated fresh under the mutex and fails with the same domain error.
  - If retried with corrected payload at $D_{applied} + 1$ (e.g. self-loop fixed to valid edge), it is evaluated as a new candidate for sequence $D_{applied} + 1$ and succeeds.
- In contrast, once an event is **committed** ($D \le D_{applied}$):
  - **Retained sequence with different event ID:** If a request reuses a committed sequence $D \in [D_{window\_min}, D_{applied}]$ with a different `event_id`, it is rejected as **`409 Conflict` (`conflicting_duplicate`)** and transitions state to `Desynchronized`.
  - **Retained event ID at new sequence:** If a request at sequence $D_{applied} + 1$ reuses an `event_id` already present in the retained window, it is rejected as **`409 Conflict` (`conflicting_duplicate`)** and transitions state to `Desynchronized`.
  - **Altered canonical content:** If a request at sequence $D \in [D_{window\_min}, D_{applied}]$ matches the committed `event_id` but alters operation, endpoints, or `source_position`, it is rejected as **`409 Conflict` (`conflicting_duplicate`)** and transitions state to `Desynchronized`.
  - **Exact retry:** If a request at sequence $D \in [D_{window\_min}, D_{applied}]$ matches `event_id` and canonical payload, it returns **`200 OK` (`status: "already_applied"`)**.
  - **Evicted history boundary:** Sequences older than the retained window ($D < D_{window\_min}$) return **`409 Conflict` (`code: "unverifiable_duplicate"`)** with server state unchanged. Reusing an `event_id` from an evicted sequence ($< D_{window\_min}$) at a new sequence cannot be detected by the bounded window and is evaluated as fresh at $D_{applied} + 1$. Knotrel explicitly guarantees deduplication strictly within the retained $N$-entry window.

#### 2. Progress Events Cannot Bypass Required Graph Mutations
- A `progress` event is permitted **only** when the corrected authoritative source projection genuinely produces zero topological changes under the graph projection filter (e.g. a non-graph database row update, or an extraneous record emitted due to a projection filter bug).
- **Prohibition:** Feeder is **strictly forbidden** from using a `progress` event to skip or bypass a required graph mutation that failed (e.g. because of `node_limit_exceeded` or `edge_limit_exceeded`). Bypassing a required mutation would cause silent, unrecoverable topological divergence between Knotrel and the authoritative source.
- **Remediation for Required Mutation Failure (e.g. Capacity Ceilings):**
  1. Stream progress is halted at the failing sequence.
  2. The operator must either reconfigure Knotrel with increased capacity (`KNOTREL_MAX_NODES`, `KNOTREL_MAX_EDGES`) and initiate an external full rebuild, or
  3. The feeder must declare an unrepairable source gap via `POST /v1/feeder/gap`, transitioning state to `Desynchronized`.

---

## 10. Proposed HTTP Interface Specification

> [!NOTE]
> All endpoints in this section are **`[PROPOSED — NOT IMPLEMENTED]`** in v0.1.0. They define the candidate contract to be implemented in Features 0.2.2–0.2.5.

### 10.1 Feeder Session Registration
- **Endpoint:** `POST /v1/feeder/session`
- **Headers:** `X-Knotrel-Instance-Id: <instance_id>`

#### Request Example:
```json
{
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "lease_duration_ms": 10000
}
```

#### Success Response (`200 OK`):
```json
{
  "result": "session_acquired",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "lease_duration_ms": 10000,
  "generation": 0,
  "applied_delivery_sequence": 0,
  "applied_source_position": null,
  "status": "uninitialized"
}
```

---

### 10.2 Feeder Heartbeat
- **Endpoint:** `POST /v1/feeder/heartbeat`
- **Headers:** `X-Knotrel-Instance-Id: <instance_id>`

#### Request Example:
```json
{
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1
}
```

#### Success Response (`200 OK`):
```json
{
  "result": "heartbeat_acknowledged",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "lease_expires_in_ms": 10000,
  "status": "ready"
}
```

---

### 10.3 Atomic Graph Import
- **Endpoint:** `POST /v1/feeder/import`
- **Headers:**  
  `X-Knotrel-Instance-Id: <instance_id>`  
  `If-Match: "1.45.0%2F16B3800"` *(mandatory when `mode: "replace"`)*

#### Request Example:
```json
{
  "mode": "create_only",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "source_id": "postgres-cluster-01",
  "source_epoch": "epoch-2026-10-19",
  "source_position": "0/16B3740",
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "nodes": [
    "1", "2", "3", "4", "5", "100"
  ],
  "edges": [
    {"source": "1", "target": "2"},
    {"source": "2", "target": "3"},
    {"source": "4", "target": "5"}
  ]
}
```

#### Success Response (`201 Created`):
```json
{
  "result": "published",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "generation": 1,
  "state_version": "0",
  "node_count": 6,
  "edge_count": 3,
  "source_position": "0/16B3740",
  "status": "catching_up"
}
```

---

### 10.4 Ordered Incremental Events (Including Progress Events)
- **Endpoint:** `POST /v1/feeder/events`
- **Headers:** `X-Knotrel-Instance-Id: <instance_id>`

#### Request Example (Batch Starting at Sequence 101):
```json
{
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "source_id": "postgres-cluster-01",
  "source_epoch": "epoch-2026-10-19",
  "events": [
    {
      "event_id": "evt-8801",
      "delivery_sequence": 101,
      "prev_delivery_sequence": 100,
      "source_position": "0/16B3758",
      "prev_source_position": "0/16B3740",
      "op": "link",
      "source": "3",
      "target": "4"
    },
    {
      "event_id": "prog-8802",
      "delivery_sequence": 102,
      "prev_delivery_sequence": 101,
      "source_position": "0/16B3800",
      "prev_source_position": "0/16B3758",
      "op": "progress"
    }
  ]
}
```

#### Success Response (`200 OK`):
```json
{
  "result": "applied",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "generation": 1,
  "applied_delivery_sequence": 102,
  "applied_source_position": "0/16B3800",
  "state_version": "1",
  "events_applied": 2
}
```

#### Duplicate Event Response (`200 OK`):
```json
{
  "result": "duplicate",
  "status": "already_applied",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "generation": 1,
  "event_id": "evt-8801",
  "delivery_sequence": 101,
  "historical_outcome": {
    "changed": true
  },
  "current_applied_delivery_sequence": 102,
  "current_applied_source_position": "0/16B3800",
  "current_state_version": "1"
}
```

---

### 10.5 Synchronization Barrier Declaration (Verification-Only)
- **Endpoint:** `POST /v1/feeder/barrier`
- **Headers:** `X-Knotrel-Instance-Id: <instance_id>`

#### Request Example:
```json
{
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "barrier_delivery_sequence": 102,
  "barrier_source_position": "0/16B3800"
}
```

#### Success Response (`200 OK`):
```json
{
  "result": "synchronized",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "generation": 1,
  "state_version": "1",
  "applied_source_position": "0/16B3800",
  "status": "ready"
}
```

---

### 10.6 Declaring Irreparable Source Gap
- **Endpoint:** `POST /v1/feeder/gap`
- **Headers:** `X-Knotrel-Instance-Id: <instance_id>`

#### Request Example:
```json
{
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "graph_id": "default",
  "generation": 1,
  "feeder_session_id": "feeder-node-a-uuid",
  "feeder_epoch": 1,
  "failed_delivery_sequence": 103,
  "reason": "source CDC history expired before catch-up completed"
}
```

#### Success Response (`200 OK`):
```json
{
  "result": "gap_acknowledged",
  "instance_id": "srv-550e8400-e29b-41d4-a716-446655440000",
  "generation": 1,
  "status": "desynchronized",
  "action_required": "full_rebuild_required"
}
```

---

## 11. Deterministic Acceptance-Test Specification

This test matrix defines reproducible, deterministic test traces for the future v0.2.0 implementation. Every test specifies input sequence, controlled synchronization points, version tuples, and failure oracles.

### 11.1 Deterministic Test Traces

| Test ID | Scenario Description | Initial State | Inputs & Controlled Interleaving Protocol | Expected Final State & Output Oracle |
|---|---|---|---|---|
| **TR-01** | Happy Path: Image $S \to$ Catch-up $\to$ Barrier $B$ | `Uninitialized` (`gen=0`) | 1. `POST /v1/feeder/session` (`epoch=1`)<br>2. `POST /v1/feeder/import` (`mode: "create_only"`, $S=\text{"pos-10"}$, 3 nodes, 2 edges)<br>3. `POST /v1/feeder/events` ($D=1, P=\text{"pos-11"}$, link 3-4)<br>4. `POST /v1/feeder/barrier` ($B=\text{"pos-11"}$, $D=1$) | State: `Uninitialized` $\to$ `Loading` $\to$ `CatchingUp` $\to$ `Ready`.<br>Final tuple: `(gen=1, state_version="1", applied_pos="pos-11")`.<br>`GET /ready` returns `200 ready`. |
| **TR-02** | Stale Instance ID Rejection | `Ready` on `inst_B` | 1. Feeder sends request with `X-Knotrel-Instance-Id: inst_A` | Request rejected: `409 Conflict` (`code: "stale_instance_id"`).<br>State remains intact on `inst_B`. |
| **TR-03** | Exact Duplicate Event Retry | `CatchingUp` ($D_{applied}=1$, event `evt-1`) | 1. `POST /v1/feeder/events` (resend `evt-1`, $D=1$, link 3-4) | Response: `200 OK` (`status: "already_applied"`).<br>`state_version` does not increment.<br>`D_{applied}` remains 1. |
| **TR-04** | Conflicting Duplicate Event | `CatchingUp` ($D_{applied}=1$, event `evt-1`, link 3-4) | 1. `POST /v1/feeder/events` (same `evt-1`, $D=1$, but payload cut 1-2) | Request rejected: `409 Conflict` (`code: "conflicting_duplicate"`).<br>State transitions to `Desynchronized`; stream halted. |
| **TR-05** | Transient Out-of-Order Sequence ($D > D_{applied}+1$) | `CatchingUp` ($D_{applied}=1$) | 1. Feeder sends event with $D=3$.<br>2. Server responds with 409.<br>3. Feeder sends event $D=2$.<br>4. Feeder resends event $D=3$. | Step 1 yields `409 out_of_order_sequence` (`expected_sequence: 2`). State stays `CatchingUp`.<br>Step 3 yields `200 applied` ($D_{applied}=2$).<br>Step 4 yields `200 applied` ($D_{applied}=3$). |
| **TR-06** | No-Op Event Advances Source Position | `Ready` ($D_{applied}=1, P=\text{"pos-11"}$, edge 1-2 exists) | 1. `POST /v1/feeder/events` ($D=2, P=\text{"pos-12"}$, link 1-2) | Response: `200 OK` (`result: "applied"`).<br>`applied_source_position` advances to `"pos-12"`.<br>`state_version` **remains unchanged**. |
| **TR-07** | Worker Timeout with Controlled Interleaving | `CatchingUp` ($D_{applied}=1$) | 1. Install `WorkerPauseHandle` on worker 1.<br>2. Client sends event $D=2$, admitted to worker.<br>3. Advance virtual clock past `KNOTREL_REQUEST_TIMEOUT_MS` $\to$ client observes 503.<br>4. Release worker 1 to complete under mutex.<br>5. Client retries event $D=2$ with same `event_id`. | Step 3 yields `503 request_timeout`.<br>Step 4 worker completes under mutex: $D_{applied}=2$.<br>Step 5 retry hits deduplication window $\to$ returns `200 already_applied`. |
| **TR-08** | Replacement During Catch-up with Controlled Lock Boundary | `CatchingUp` (gen 1, $D_{applied}=1$) | 1. Worker 1 parses candidate replacement graph (gen 2) off-lock.<br>2. Worker 2 enqueues event for gen 1 ($D=2$).<br>3. Worker 1 acquires lock, swaps pointer, sets gen 2, releases lock.<br>4. Worker 2 acquires lock, checks generation. | Worker 1 publishes gen 2.<br>Worker 2 fails under mutex: `409 Conflict` (`code: "stale_generation"`).<br>Live state belongs cleanly to gen 2. |
| **TR-09** | Fenced Stale Feeder Under Mutex Check | `Ready` (gen 1, `feeder_epoch=2`) | 1. Stale feeder sends `POST /v1/feeder/events` (`feeder_epoch=1`).<br>2. Request reaches service validation under mutex. | Request rejected: `409 Conflict` (`code: "fenced_feeder_session"`).<br>Validated under service boundary before graph execution; live state intact. |
| **TR-10** | Source Epoch Reset / Failover | `Ready` (epoch `"ep-1"`) | 1. Feeder sends `POST /v1/feeder/events` (epoch `"ep-2"`, $D=2$) | Request rejected: `409 Conflict` (`code: "source_epoch_mismatch"`).<br>State transitions to `Desynchronized`. Requires full rebuild. |
| **TR-11** | Feeder Lease Expiration and Stale Readiness | `Ready` (lease 10s) | 1. Stop feeder heartbeats.<br>2. Advance virtual clock by 10,001 ms.<br>3. Probe `GET /ready`.<br>4. Send heartbeat.<br>5. Probe `GET /ready`. | Step 3 returns `503 Service Unavailable` (`status: "stale"`).<br>Step 4 heartbeat returns `200 heartbeat_acknowledged`.<br>Step 5 probe **remains 503 `status: "stale"`** until synchronization barrier is verified (§4.3). |
| **TR-12** | Process Restart and Full Rebuild | `Ready` (gen 1, 1000 nodes) | 1. Send SIGTERM; server exits clean.<br>2. Start new Knotrel process.<br>3. Old feeder sends gen 1 event with old `instance_id`. | Step 2 new process starts with new `instance_id`, `gen=0`, state `Uninitialized`.<br>Step 3 rejected: `409 Conflict` (`stale_instance_id`).<br>Feeder discovers new `instance_id` and performs full import. |
| **TR-13** | Direct Mutation Rejection in Source-Managed Mode | Mode: `source-managed`, `Ready` | 1. `POST /v1/operations` (`link 1-2`)<br>2. `POST /v1/batch` (contains 1 link, 1 query) | 1. Rejected: `409 Conflict` (`direct_mutation_disabled`).<br>2. Rejected: `409 Conflict` (`direct_mutation_disabled`).<br>Graph topology unmodified. |
| **TR-14** | Standalone Mode Backward Compatibility | Mode: `standalone` (default) | 1. `POST /v1/operations` (`link 1-2`)<br>2. `POST /v1/batch` (mixed ops with domain error) | 1. Succeeds: `200 OK`, `changed: true`.<br>2. Succeeds: `200 OK`, continue-on-error behavior verified.<br>`state_version` increments identically to v0.1.0. |
| **TR-15** | Equal Barrier Sequence with Mismatching Position | `CatchingUp` ($D_{applied}=10, P_{applied}=\text{"0/16B3800"}$) | 1. Feeder sends `POST /v1/feeder/barrier` ($D=10, P=\text{"0/16B9999"}$). | Rejected under decision table case B7: `409 Conflict` (`code: "barrier_position_mismatch"`).<br>State remains `CatchingUp`; cursor untouched. |
| **TR-16** | Progress Event Retry and Conflicting Retry | `CatchingUp` ($D_{applied}=5, P=\text{"pos-10"}$) | 1. Send `progress` event ($D=6, P=\text{"pos-11"}$).<br>2. Resend exact `progress` event.<br>3. Send same ID with conflicting position. | Step 1 applies: $D_{applied}=6, P=\text{"pos-11"}$, version unchanged.<br>Step 2 returns `200 already_applied`.<br>Step 3 returns `409 conflicting_duplicate` $\to$ `Desynchronized`. |
| **TR-17** | Heartbeat After Lease Expiry While Updates Remain Unapplied | `Disconnected`/`Stale` (lease expired, source has new events) | 1. Send heartbeat `POST /v1/feeder/heartbeat`.<br>2. Probe `GET /ready`.<br>3. Send events up to $B$ and call barrier. | Step 1 returns `200 heartbeat_acknowledged`.<br>Step 2 returns `503 stale` (proves heartbeat does not bypass catch-up).<br>Step 3 verifies barrier $\to$ `/ready` returns `200 ready`. |
| **TR-18** | Heartbeat During Desynchronized and CatchingUp | `Desynchronized` (epoch mismatch) | 1. Send heartbeat `POST /v1/feeder/heartbeat`.<br>2. Probe `GET /ready`. | Step 1 returns 200 (lease extended).<br>Step 2 probe **remains 503 `status: "desynchronized"`** (heartbeat never clears failure). |
| **TR-19** | Session Takeover Pausing Candidate Import Before Publication | `CatchingUp` (Feeder A, `epoch=1`) | 1. Feeder A begins building candidate import off-lock.<br>2. Feeder B takes over: `POST /v1/feeder/session` (`epoch=2`).<br>3. Feeder A candidate build completes and acquires lock for publication. | Step 2 updates `live.feeder_epoch = 2`.<br>Step 3 under lock rechecks epoch: Feeder A (`epoch=1`) rejected with `409 fenced_feeder_session`. Candidate discarded. |
| **TR-20** | Sequence Continuity with Incorrect Source Predecessor | `CatchingUp` ($D_{applied}=1, P_{applied}=\text{"pos-10"}$) | 1. Feeder sends event with $D=2, D_{prev}=1$, but `prev_source_position: "pos-WRONG"`. | Rejected under mutex: `409 Conflict` (`source_predecessor_mismatch`).<br>State transitions to `Desynchronized`. Graph unchanged. |
| **TR-21** | Hash Collision Handling with 256-Byte Cursor | `CatchingUp` | 1. Fabricate two events with same SipHash but different 256-byte `source_position` strings. | Tier 2 full string comparison detects mismatch; entry treated as cache miss, preventing false deduplication. |
| **TR-22** | Partial Batch Beginning at Non-Trivial Global Sequence | `CatchingUp` ($D_{applied}=100$) | 1. Send batch with 3 events: $D=101$ (valid), $D=102$ (valid), $D=103$ (self-loop). | Events 101 and 102 applied ($D_{applied}=102$).<br>Response returns `422 partial_failure` (`failed_batch_index: 2`, `failed_delivery_sequence: 103`). |
| **TR-23** | Declared Source History Loss via `/v1/feeder/gap` | `CatchingUp` ($D_{applied}=5$) | 1. Feeder calls `POST /v1/feeder/gap` (`failed_delivery_sequence: 6`). | Server acknowledges gap (`200 gap_acknowledged`).<br>State transitions to `Desynchronized`. Queries fail with 503. |
| **TR-24** | Older Barrier with Different Position (Superseded Case B9) | `Ready` ($D_{applied}=20, P_{applied}=\text{"0/16B5000"}$) | 1. Feeder sends delayed barrier ($D=10, P=\text{"0/16B3000"}$). | Rejected under decision table case B9: `409 Conflict` (`code: "superseded_barrier"`).<br>State remains `Ready`; cursor remains at 20. |
| **TR-25** | Rejected Barriers Preserving Ready, Stale, and Desynchronized | Test across 3 distinct server states | 1. State `Ready`: send premature barrier ($D=50$).<br>2. State `Stale`: send mismatched position barrier ($P=\text{"bad"}$).<br>3. State `Desynchronized`: send exact matching barrier. | 1. Returns `409 barrier_not_reached`; state stays `Ready`.<br>2. Returns `409 barrier_position_mismatch`; state stays `Stale`.<br>3. Returns `409 graph_desynchronized`; state stays `Desynchronized`. |
| **TR-26** | Rejected Event Retry vs. Committed-Event Conflict | Initial: $D_{applied}=100$ | 1. Event $D=101$ (`evt-1`, self-loop) fails with 422.<br>2. Resend sequence 101 with corrected link.<br>3. Event 101 commits ($D_{applied}=101$).<br>4. Resend committed `evt-1` with altered cut. | Step 1 uncommitted $\to$ not in dedup window.<br>Step 2 succeeds: `200 applied` ($D_{applied}=101$).<br>Step 4 resends committed ID with altered content $\to$ `409 conflicting_duplicate` $\to$ `Desynchronized`. |
| **TR-27** | Required Mutation Failure Cannot Be Bypassed with Progress | Graph at edge capacity ceiling (`remaining_edges: 0`) | 1. Event $D=50$ (`link 1-2`) fails with `422 edge_limit_exceeded`.<br>2. Feeder attempts to bypass by emitting event $D=50$ as `op: "progress"`. | Stream progress is halted at 49. Feeder is strictly forbidden from substituting `progress` for failed required mutations. Feeder must either halt and declare gap via `/v1/feeder/gap` or operator increases limits. |
| **TR-28** | Import Precondition Failure (412) vs. Ownership Fencing (409) | Mode: `source-managed`, `Ready` (gen 1, epoch 1) | 1. Call import with stale feeder epoch 0.<br>2. Call import with valid epoch 1 but mismatched `If-Match: "1.99.pos"`. | Step 1 fails fencing: `409 Conflict` (`fenced_feeder_session`).<br>Step 2 passes fencing but fails precondition: `412 Precondition Failed` (`precondition_failed`). Live graph untouched. |
| **TR-29** | Exact Barrier Match While Uninitialized | `Uninitialized` ($g=0, D_{app}=0, P_{app}=\text{null}$) | 1. Feeder sends `POST /v1/feeder/barrier` ($D=0, P=\text{null}$). | Rejected under Case B3: `409 Conflict` (`code: "graph_uninitialized"`).<br>State remains `Uninitialized`. No barrier may make an unpublished initial graph `Ready`. |
| **TR-30** | Barrier During Initial Baseline Loading | `Loading` ($g=0$, candidate parsing off-lock) | 1. Feeder sends `POST /v1/feeder/barrier` ($D=0$ or matching candidate snapshot $S$). | Rejected under Case B4: `409 Conflict` (`code: "graph_uninitialized"`).<br>State remains `Loading`; candidate remains unpublished. |
| **TR-31** | Barrier During Replacement Loading | `Ready` (live $g=1, D_{app}=10$), candidate $g=2$ parsing off-lock | 1. Feeder sends barrier for candidate `gen=2` ($D=0$).<br>2. Feeder sends barrier for live `gen=1` ($D=10, P=P_{live}$). | Step 1 rejected: `409 Conflict` (`stale_generation`); live $g=1$ untouched.<br>Step 2 accepted under Case B2: `200 already_synchronized`; live $g=1$ remains `Ready`. |
| **TR-32** | Retained Committed Sequence with Different Event ID | `CatchingUp` ($D_{applied}=5$, seq 5 committed with `evt-05`) | 1. Feeder sends `POST /v1/feeder/events` ($D=5, \text{event\_id: "evt-99"}$). | Ring buffer sequence check fails: `evt-99 != evt-05`.<br>Rejected: `409 Conflict` (`code: "conflicting_duplicate"`). State transitions to `Desynchronized`. |
| **TR-33** | Retained Event ID Reused at New Sequence | `CatchingUp` ($D_{applied}=5$, `evt-03` committed at seq 3) | 1. Feeder sends `POST /v1/feeder/events` ($D=6, D_{prev}=5, \text{event\_id: "evt-03"}$). | Event ID hash index check detects reuse of `evt-03`.<br>Rejected: `409 Conflict` (`code: "conflicting_duplicate"`). State transitions to `Desynchronized`. |
| **TR-34** | Evicted History Boundaries | Window $N=10$, $D_{applied}=15$ ($D_{win}=6$). `evt-04` at seq 4 is evicted. | 1. Feeder resends sequence $D=4$ (`evt-04`).<br>2. Feeder sends new sequence $D=16$ reusing `evt-04`. | Step 1: $D=4 < D_{win} \to$ rejected: `409 Conflict` (`unverifiable_duplicate`), state unchanged.<br>Step 2: `evt-04` evicted $> N$ events ago; cannot be detected by bounded window $\to$ evaluated fresh at $D=16$ and succeeds (`200 applied`, $D_{applied}=16$). |

---

## 12. Resolution of Roadmap Decisions D1–D6

This section documents the formal architectural decisions resolving items D1–D6 from `ROADMAP.md`.

### Decision D1: Identity and Ownership

| Category | Details |
|---|---|
| **Context** | Knotrel needs stable identity and fencing tokens to prevent split-brain updates, cross-graph contamination, and resurrection of stale versions after restart. |
| **Alternatives Considered** | 1. *Dynamic Multi-Graph Routing:* Add per-request graph URI routing (`/graphs/{id}/...`). Rejected: unmeasured complexity, contradicts single-graph accelerator scope.<br>2. *Opaque Timestamps as Versions:* Use wall-clock timestamps for generation/revision. Rejected: non-monotonic clock skew, breaks deterministic testing.<br>3. *Strict Token Hierarchy with Instance Scoping (Recommended):* Explicit tuple `(instance_id, graph_id, generation, state_version, source_id, source_epoch, applied_source_position, feeder_session_id, feeder_epoch)`. Mandatory `instance_id` in headers. |
| **Recommended Design** | Adopt Alternative 3. Single implicit graph (`graph_id: "default"`). Monotonic `generation` ($1, 2, \dots$) increments on each atomic import. Existing arbitrary-precision decimal `state_version` increments on topological changes. Monotonic `feeder_epoch` fences stale feeders. |
| **Compatibility** | 100% compatible with v0.1.0 standalone mode (where generation is 0 and existing decimal revision rules apply). |
| **Correctness & Security** | Eliminates stale feeder races and cross-generation update application across restarts. |
| **Residual Questions for Maintainer** | Confirm default `graph_id` string (`"default"`). |

---

### Decision D2: Modes and Acknowledgment

| Category | Details |
|---|---|
| **Context** | Direct uncoordinated writes in source-managed mode would desynchronize the in-memory graph from the authoritative database, invalidating the applied cursor. |
| **Alternatives Considered** | 1. *Permissive Dual-Mode:* Allow direct writes and attempt to advance an artificial cursor. Rejected: guarantees silent divergence from external source.<br>2. *Synchronous Source Write-Through:* Knotrel calls the external DB to write changes before applying. Rejected: turns Knotrel into a distributed transaction coordinator, violating acceleration goals.<br>3. *Strict Mode Segregation (Recommended):* Explicit startup flag `KNOTREL_MODE`. Standalone mode accepts legacy writes. Source-managed mode strictly rejects mutating `/v1/operations` and `/v1/batch` with HTTP 409 `direct_mutation_disabled`. Acknowledgment certifies in-memory application only; durability disclaimer is explicit. |
| **Recommended Design** | Adopt Alternative 3. Maintain complete separation of concerns. In source-managed mode, direct mutations fail fast. |
| **Compatibility** | Standalone mode remains the default with unchanged v0.1.0 semantics. |
| **Correctness & Security** | Guarantees that in source-managed mode, Knotrel is a faithful projection of the verified source prefix. |
| **Residual Questions for Maintainer** | Confirm HTTP 409 Conflict vs HTTP 403 Forbidden for direct mutation rejection. |

---

### Decision D3: Ordering and Retry

| Category | Details |
|---|---|
| **Context** | External source cursors (e.g. Postgres LSNs, Kafka partition offsets, alphanumeric composite tokens) cannot be assumed to be contiguous integers. Knotrel needs a source-independent continuity protocol. |
| **Alternatives Considered** | 1. *Contiguous Integer Cursor Assumption:* Force external systems to provide 1, 2, 3... cursors. Rejected: incompatible with standard enterprise databases and Kafka.<br>2. *Lexicographical Cursor Comparison:* Compare arbitrary string cursors. Rejected: incorrect for non-fixed-width numbers or complex structures.<br>3. *Transport Delivery Sequence + Bounded Deduplication Evaluated Under Mutex (Recommended):* Feeder supplies strictly contiguous `delivery_sequence` ($1, 2, 3, \dots$) and `prev_delivery_sequence` alongside opaque source cursor strings. Under the graph mutex, requests are classified as: previous sequence (checked in bounded deduplication window with exact canonical equality), expected next sequence (executed), or future sequence (rejected as `out_of_order_sequence`). Feeder events use stop-on-first-error semantics. |
| **Recommended Design** | Adopt Alternative 3. Feeder owns cursor interpretation and ordering; Knotrel enforces transport sequence continuity, predecessor match, and bounded duplicate rejection under the lock via a dual-index window (sequence ring buffer and event ID hash index). Knotrel strictly differentiates exact retries (200 OK) from conflicting duplicates (409 Conflict -> Desynchronized) across both sequence reuse and event ID reuse within the active N-entry horizon, while conservatively returning 409 unverifiable_duplicate for evicted sequences. |
| **Compatibility** | Legacy `/v1/batch` error continuation is preserved for standalone mode. Feeder endpoints use their own strict fail-fast ordering contract. |
| **Correctness & Security** | Completely prevents gaps, reordering anomalies, cross-sequence event ID duplication, and false deduplication claims. |
| **Residual Questions for Maintainer** | Approve proposed default bounded duplicate window size (10,000 entries) with exact dual-index memory accounting requirements. |

---

### Decision D4: Capacity and Freshness

| Category | Details |
|---|---|
| **Context** | Graph memory must be bounded, and operators must know whether queries reflect current source state without inventing unsupported numerical freshness SLOs. |
| **Alternatives Considered** | 1. *Arbitrary Real-Time SLO:* Declare that lag $< 100\text{ ms}$ is ready. Rejected: arbitrary, unverifiable without synchronized distributed clocks.<br>2. *Silent Serving During Disconnect:* Continue reporting 200 Ready indefinitely when the feeder drops. Rejected: violates rule that divergent graphs must not be reported synchronized.<br>3. *Explicit Lease-Driven State Machine with Degradation Headers (Recommended):* Feeder renews lease via heartbeats against a monotonic clock. Heartbeat renews lease but does not restore `Ready` without synchronization proof. Discrete states: `CatchingUp`, `Ready`, `Stale`, `Desynchronized`. Bounded import body (proposed default: 64 MiB). On lease expiry, readiness drops to 503 by default; if `KNOTREL_SERVE_STALE=true`, queries succeed but carry `X-Knotrel-Sync-Status: stale`. Declared gaps always force `Desynchronized`. |
| **Recommended Design** | Adopt Alternative 3. Enforce deterministic state transitions without inventing unsupported millisecond promises. |
| **Compatibility** | Standalone mode retains existing cardinality limits (`KNOTREL_MAX_NODES`, `KNOTREL_MAX_EDGES`) and fixed 1 MiB body limit. |
| **Correctness & Security** | Ensures operators and downstream clients are never misled about data currency. |
| **Residual Questions for Maintainer** | Confirm proposed default import payload bound (64 MiB) and default `KNOTREL_SERVE_STALE` setting (default `false`). |

---

### Decision D5: Import and Publication

| Category | Details |
|---|---|
| **Context** | How should initial baseline images be loaded, and how should replacement images be applied without corrupting live query state? |
| **Alternatives Considered** | 1. *Streaming Chunked Import:* Stream nodes/edges over chunked HTTP. Deferred: high protocol complexity for initial v0.2.0.<br>2. *In-Place Mutation:* Mutate the live graph directly during import. Rejected: queries observe half-loaded graphs; failure leaves corrupt state.<br>3. *Offline Candidate Build + Atomic Swap with Mandatory Composite Precondition (Recommended):* Parse and build candidate `ConnectivityGraph` entirely outside the live lock. Concurrency bounded to 1. Acquire graph lock. For `create_only`, verify `generation == 0`. For `replace`, verify mandatory `If-Match: "<generation>.<state_version>.<percent_encoded_source_position>"`. Atomically swap graph pointer, increment `generation` ($g \to g + 1$), reset `state_version` to `"0"`, update `applied_source_position` to $S$, and enter `CatchingUp`. Failed build leaves live graph untouched. |
| **Recommended Design** | Adopt Alternative 3. Support both create-only (initial load) and conditional replace via mandatory composite `If-Match`. Readers never observe partial state. Strictly separate error categories: composite precondition mismatches on `If-Match` return `412 Precondition Failed` (`precondition_failed`); ownership, fencing, and state errors (`create_only` when already initialized, stale session epoch, expired lease) return `409 Conflict` (`graph_already_initialized`, `fenced_feeder_session`, `lease_expired`). |
| **Compatibility** | Preserves existing `try_from_edges` validation rules (duplicate edges are no-ops, self-loops rejected, limits checked). |
| **Correctness & Security** | Guarantees atomic publication, prevents concurrent update clobbering, and ensures clients unambiguously differentiate HTTP condition evaluation from fencing conflicts. |
| **Residual Questions for Maintainer** | Confirm composite header syntax `If-Match: "<generation>.<state_version>.<percent_encoded_source_position>"`. |

---

### Decision D6: Trust Boundary

| Category | Details |
|---|---|
| **Context** | Who is authorized to feed events, replace the graph, or declare barriers? |
| **Alternatives Considered** | 1. *Zero Authentication (v0.1.0 Baseline):* Assume trusted private network only. Problematic: rogue or crashed feeder can disrupt production graph.<br>2. *Full Integrated TLS + OAuth2/OIDC RBAC:* Build enterprise auth into Knotrel server. Rejected: premature, belongs to v1.0.0 Milestone M8.<br>3. *Partitioned Surfaces + Shared Secret Token (Recommended):* Keep loopback default. Partition routes into Query vs Feeder/Admin. Introduce optional `KNOTREL_FEEDER_TOKEN` (HTTP header `X-Knotrel-Feeder-Token`) for feeder routes. Require reverse proxy/gateway for non-local or multi-tenant deployment. If untrusted network exposure is required immediately, bring Milestone M8 forward. |
| **Recommended Design** | Adopt Alternative 3. Provide immediate lightweight protection against rogue feeder writes while preserving minimal dependency footprint. |
| **Compatibility** | Standalone local testing remains frictionless when token is unset. |
| **Correctness & Security** | Prevents accidental cross-talk between development tools and production feeder sessions. |
| **Residual Questions for Maintainer** | Confirm whether `KNOTREL_FEEDER_TOKEN` should be mandatory when non-loopback binding is enabled in source-managed mode. |

---

## 13. Handoff Specification for Feature 0.2.2 (Bounded Atomic HTTP Import)

To enable immediate, unambiguous implementation of Feature 0.2.2 without premature freezing of subsequent features, this section defines the minimal candidate handoff contract pending maintainer approval:

### 13.1 Implementation Prerequisites
1. **Startup Mode Support:** Implement configuration flag `KNOTREL_MODE` (`standalone` default, `source-managed` opt-in).
2. **Identity Model:** Implement `instance_id` (UUIDv4), `generation` (`u64`), `source_id` (`String`), `source_epoch` (`String`), `applied_source_position` (`String`), and `feeder_session_id` (`String`).
3. **Endpoint:** Implement `POST /v1/feeder/import` (enforcing `X-Knotrel-Instance-Id`).
4. **Import Mode Preconditions & Error Segregation:**
   - If `mode: "create_only"`: Server must be `Uninitialized` (`generation == 0`). Otherwise return `409 Conflict` (`graph_already_initialized`).
   - If `mode: "replace"`: Mandatory composite precondition `If-Match: "<gen>.<state_version>.<percent_encoded_source_position>"`. Server verifies match under mutex. If precondition fields mismatch, return `412 Precondition Failed` (`precondition_failed`). If feeder lease/fencing fails, return `409 Conflict` (`fenced_feeder_session` / `lease_expired`).
5. **Offline Candidate Build & Concurrency Ceiling:**
   - Single active import permit (`KNOTREL_MAX_CONCURRENT_IMPORTS = 1`); excess gets 503 `import_in_progress` before reading request body bytes.
   - Deserialization bounded by `KNOTREL_MAX_IMPORT_BYTES` (proposed default: 64 MiB).
   - Candidate `ConnectivityGraph` constructed in memory on a worker thread.
   - Enforce existing `try_from_edges` invariants: deduplicate edges, reject self-loops, enforce cardinality limits (`KNOTREL_MAX_NODES`, `KNOTREL_MAX_EDGES`).
6. **Atomic Publication:**
   - Acquire exclusive graph lock.
   - Recheck feeder lease and preconditions under lock.
   - Swap graph pointer, set `generation = generation + 1`, reset `state_version = "0"`, record `applied_source_position = S`.
   - Transition lifecycle state to `CatchingUp`.
7. **Query Isolation During Rebuild:**
   - If an existing generation was already serving queries, active queries continue executing against the old graph until the lock acquisition.
   - The swap is instantaneous. Readers subsequent to the swap observe the new generation and `CatchingUp` status.

### 13.2 Implementation Status: Feature 0.2.3 (Identity, Generation, Revision & Source Position)
Feature 0.2.3 is implemented in `knotrel-server`:
1. **Coherent Response Metadata:** Query responses (`POST /v1/operations`), batch responses (`POST /v1/batch`), live snapshots (`GET /v1/stats`), and import publications (`POST /v1/feeder/import`) capture results and identity/version headers (`ETag`, `X-Knotrel-Instance-Id`, `X-Knotrel-Graph-Id`, `X-Knotrel-Generation`, `X-Knotrel-State-Version`, `X-Knotrel-Source-Position`, `X-Knotrel-Sync-Status`) under the **same** graph mutex serialization boundary.
2. **Replacement Preconditions:** The composite token `ETag: "<generation>.<state_version>.<percent_encoded_source_position>"` is directly reusable as the mandatory `If-Match` header on replacement imports. Preconditions become stale upon replacement ($g \to g + 1$) or restart even when numeric revisions and graph topology coincide.
3. **Cursor Representation & Safety:** Opaque source cursors are safely RFC 3986 percent-encoded in HTTP headers, preventing header injection and delimiter collisions. Cursors are never interpreted as numbers or compared lexicographically.
4. **Generation Exhaustion:** If generation counter reaches `u64::MAX`, subsequent replacements are rejected with HTTP 409 `generation_exhausted` without arithmetic overflow or wrapping, preserving live state, topology, revision, and source lineage. Verified via dedicated service-level test on the real import publication path.
5. **Arbitrary-Precision Decimal Revisions:** Multi-digit carries beyond `u64::MAX` are preserved without numeric overflow. Verified across real mutation execution (599 -> 600), with duplicate no-ops and domain error rejections preserving the revision.
6. **Response Delivery Coherence:** Verified via deterministic test-support synchronization (`pause_next_worker_after_lock`) that a query capturing outcome and metadata under the graph lock retains its exact original topology result, generation, revision, cursor, and ETag even when response delivery is paused across a completed replacement publication on the live graph.
7. **Batch Scope:** Batch response headers reflect the final coherent state after all operations in the batch complete under continue-on-error.
8. **Feature Boundaries:** Non-mutating source progress (`progress` events) and ordered update streaming belong to Feature 0.2.4; synchronization barrier verification and automated readiness gating belong to Feature 0.2.5.

### 13.3 Implementation Status: Feature 0.2.4 (Ordered Incremental Updates, Deduplication & Gap Declaration)
Feature 0.2.4 is implemented in `knotrel-server`:
1. **Endpoints:** Implemented `POST /v1/feeder/events` (ordered update streaming & progress events with bounded deduplication and fail-fast batch execution) and `POST /v1/feeder/gap` (declared unrepairable gap transitioning to `Desynchronized`).
2. **Deterministic Dual-Index Deduplication:** Implemented `DedupWindow` ring buffer indexed modulo $N$ with an `event_ids` hash index.
   - Retained sequence matching `event_id` and canonical payload returns HTTP 200 `already_applied` (`result: "duplicate"`).
   - Retained sequence with altered content or differing `event_id` returns HTTP 409 `conflicting_duplicate` and transitions to `Desynchronized`.
   - Retained `event_id` reused at a new sequence returns HTTP 409 `conflicting_duplicate` and transitions to `Desynchronized`.
   - Evicted sequence ($D \le D_{applied} - W$) returns HTTP 409 `unverifiable_duplicate` preserving `CatchingUp`.
   - Evicted `event_id` reused at new sequence is admitted fresh at $D_{applied} + 1$ (TR-34).
   - Configured via `KNOTREL_DEDUP_WINDOW_SIZE` (default: 10,000, bounds: 1 to 1,000,000).
3. **Regression Invariant:** Applying `link(A, B)`, followed by `cut(A, B)`, then retrying the old `link(A, B)` event at $D=1$ yields HTTP 200 `already_applied` without modifying graph topology; edge $(A, B)$ strictly remains absent.
4. **Sequence Continuity and Gaps:** Contiguous delivery sequence verified ($D_i = D_{prev} + 1$) with initial $D_{prev} == D_{applied}$. Gaps or out-of-order delivery return HTTP 409 `gap_detected`.
5. **Fail-Fast Batch Semantics:** Events are processed sequentially under a single graph mutex acquisition. On first domain error (e.g. self-loop 422, unknown node 404, capacity limit 422), execution stops immediately; already committed prefix remains applied in memory; partial failure reports `failed_batch_index`, `failed_delivery_sequence`, `failed_event_id`, and current applied frontier. Uncommitted events are not recorded in the deduplication window and can be retried at $D_{applied} + 1$ with corrected payload.
6. **No-Op & Progress Events:** No-op mutations and `progress` events advance `applied_source_position` and delivery sequence without incrementing `state_version`.
7. **Sequence Boundary Semantics and Checked Arithmetic:** Delivery sequence is bounded to `u64`. When $D_{applied} = \text{u64::MAX}$, checked arithmetic (`checked_add(1)`) prevents integer overflow panics. Because no `u64` wire payload can represent a value beyond `u64::MAX`, any incoming wire sequence satisfies $\text{seq} \le D_{applied}$. Retained duplicate retries at `u64::MAX` return HTTP 200 `already_applied`; wrapped sequence zero or evicted sequences return HTTP 409 `unverifiable_duplicate` without mutating topology, revision, sequence, or position. The internal `SequenceExhausted` error variant serves as a defensive guard for checked arithmetic and is unreachable through the `u64` wire interface.
8. **Declared Gap & Desynchronized State Event Rejection:** `POST /v1/feeder/gap` transitions sync status to `Desynchronized`. Once desynchronized (via declared gap, conflicting duplicate, predecessor mismatch, or source epoch mismatch), subsequent event submissions are rejected under lock with HTTP 409 `graph_desynchronized` without mutating topology, revision, delivery sequence, source position, or deduplication state. Subsequent queries return HTTP 503 `desynchronized`. Feeder heartbeats renew session lease but probe reports `desynchronized`. Live graph remains intact for diagnostics; recovery requires atomic graph replacement via `POST /v1/feeder/import`.
9. **Feature Boundaries:** Synchronization barrier verification (`POST /v1/feeder/barrier`), barrier timeout, and automated readiness promotion belong to Feature 0.2.5.

### 13.4 Implementation Status: Feature 0.2.5 (Synchronization-Aware Bootstrap and Readiness)
Feature 0.2.5 is implemented in `knotrel-server`:
1. **Verification-Only Barrier Endpoint:** Implemented `POST /v1/feeder/barrier` validating mode, headers (`X-Knotrel-Instance-Id`), JSON body schema, graph identity (`"default"`), generation, feeder lease, and worker health under the graph mutex. It strictly executes Decision Table B1 through B9:
   - Case B1: Exact match ($D_B == D_{applied} \land P_B == P_{applied}$) in `CatchingUp` or `Stale` transitions to `Ready` (`200 synchronized`).
   - Case B2: Exact match in `Ready` affirms `Ready` (`200 already_synchronized`).
   - Case B3/B4: Barrier while `Uninitialized` or initial baseline `Loading` ($g == 0$) is rejected with HTTP 409 `graph_uninitialized` (no barrier can make an unpublished graph `Ready`).
   - Case B5: Barrier while `Desynchronized` is rejected with HTTP 409 `graph_desynchronized` (barriers cannot bypass unrepairable gaps or desynchronization).
   - Case B6: Barrier while `Failed` is rejected with HTTP 503 `unavailable`.
   - Case B7: Position mismatch at equal sequence ($D_B == D_{applied} \land P_B \ne P_{applied}$) is rejected with HTTP 409 `barrier_position_mismatch`.
   - Case B8: Premature sequence ($D_B > D_{applied}$) is rejected with HTTP 409 `barrier_not_reached`.
   - Case B9: Older sequence ($D_B < D_{applied}$) is conservatively rejected with HTTP 409 `superseded_barrier`.
2. **Strict Readiness Precedence:** Implemented 7-level strict evaluation precedence in `readiness()`:
   `Permanent Worker Failure` (`unavailable`) > `Draining` (`draining`) > `Starting` (`starting`) > `Uninitialized / Loading` (`uninitialized` / `loading`) > `Desynchronized` (`desynchronized`) > `Expired Lease / Stale` (`stale`) > `CatchingUp` (`catching_up`) > `Ready` (`ready`).
3. **Lock-Free Monotonic Lease Expiry & Coherent Atomic Publication:** `GET /ready` evaluates lease expiry against host monotonic time without acquiring the graph mutex, reading a single atomic 64-bit word (`PublishedReadiness`) encoding status (4 bits), publication sequence (16 bits), and lease deadline (44 bits). On observing lease expiry in `Ready`, `/ready` attempts a lock-free CAS to transition the word to `Stale`. If a concurrent publication (such as a fresh barrier or new epoch) modified the publication sequence or deadline, the CAS safely fails without overwriting newer state.
4. **Heartbeat Non-Regression:** Heartbeats (`POST /v1/feeder/heartbeat`) extend lease deadline and renew ownership, but if the graph was stale, `GET /ready` remains HTTP 503 `stale` until a barrier verification proves synchronization. Heartbeats never clear `desynchronized`, `uninitialized`, `loading`, `catching_up`, or worker panic.
5. **Explicit Recovery-Fencing Across Lease Expiry:** To prevent delayed pre-expiry barriers from executing after a heartbeat renewal on the same session/epoch and restoring readiness without fresh proof, Knotrel implements explicit recovery fencing: when lease expiry occurs, `last_expired_epoch` records the expired epoch. Any barrier request submitted under `feeder_epoch <= last_expired_epoch` is rejected with HTTP 409 `fenced_feeder_session`. Restoration of `Ready` from `Stale` strictly requires the feeder to register a newly incremented `feeder_epoch > last_expired_epoch` and supply fresh barrier proof.
6. **Drain Gate and Shutdown Integration:** The existing shutdown gate in `serve()` admits requests once and answers with 503 `draining`. In-flight barrier and event workers complete cleanly within the drain timeout.
7. **Verification Evidence:** 21 deterministic integration tests in `crates/knotrel-server/tests/bootstrap.rs` (including `test_20_delayed_barrier_fenced_across_lease_expiry` and `test_21_probe_paused_at_expiry_does_not_overwrite_newer_publication`) and unit tests in `crates/knotrel-server/src/service.rs` verify all contract scenarios TR-01 through TR-31 and concurrency interleavings with zero failures.

---

## 14. Maintainer Decision Checklist

Before declaring Feature 0.2.1 complete and proceeding to implementation in Feature 0.2.2, the maintainer must review and formally approve:

- [ ] **Approval of Operating Mode Segregation:** Confirm that direct mutations in source-managed mode return HTTP 409 Conflict (`direct_mutation_disabled`).
- [ ] **Approval of Instance ID Header Enforcement:** Confirm mandatory `X-Knotrel-Instance-Id` header across all feeder endpoints.
- [ ] **Approval of Progress Events & Verification-Only Barrier:** Confirm that `progress` events are the sole non-mutating progress mechanism, barriers are verification-only, and mutually exclusive evaluation table (§5.3) is adopted.
- [ ] **Approval of Sequence Classification & Predecessor Validation:** Confirm sequence classification ($D \le D_{applied}$, $D == D_{applied}+1$, $D > D_{applied}+1$) and predecessor check under the graph lock prior to mutation execution.
- [ ] **Approval of Feeder Lease Protocol:** Confirm `POST /v1/feeder/session` and `POST /v1/feeder/heartbeat` with monotonic lease timeout (proposed default: 10s), where heartbeat renews lease but does not restore `Ready` without synchronization proof.
- [ ] **Approval of Mandatory Replace Precondition:** Confirm `If-Match: "<gen>.<state_version>.<percent_encoded_source_position>"` for graph replacements.
- [ ] **Approval of Exact Bounded Duplicate Window:** Confirm proposed default 10,000 entry window with dual-index (sequence ring buffer + event ID hash index), Tier 2 canonical equality check, and memory accounting requirements.
- [ ] **Approval of Import Bounds:** Confirm proposed default 64 MiB body limit and `KNOTREL_MAX_CONCURRENT_IMPORTS = 1`.
