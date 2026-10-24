# Knotrel Roadmap: In-Memory Dynamic Connectivity Accelerator

**Status:** Approved product direction; proposed release-oriented execution plan. Future features remain unimplemented until verified.

**Scope:** `knotrel` (independent Rust library and HTTP server) and `knotrel-benchmarks` (correctness/performance evidence).

**Product direction:** An in-memory dynamic connectivity accelerator for evolving undirected graphs. An external system remains the authoritative source of graph data.

**Baseline:** Preserve the released v0.1.0 volatile service, synchronous Rust library, engine selection, graph limits, revisions, existing batch behavior and `ConnectivityGraph::try_from_edges`.

Knotrel maintains a specialized, derived graph representation for graph loading, incremental updates and exact connectivity queries. It is not a general-purpose graph database, a replacement for the customer's authoritative store, a distributed persistence system, an automatically durable service or a Redis-compatible server. Correctness, synchronization, simple integration and measurable connectivity performance are the primary differentiators; commercial advantage requires evidence, not positioning alone.

An external feeder reads the authoritative database or event stream and supplies coherent graph images and ordered updates. Provider-specific PostgreSQL, Kafka, Neo4j or other connectors do not belong inside the server in these milestones. Optional local persistence remains a future startup/recovery accelerator; it does not transfer authority to Knotrel.

**Scope labels:** **REQUIRED** means a release acceptance gate. **OPTIONAL** means a separately enabled capability whose absence does not invalidate the basic product contract. **DEFERRED** means no implementation commitment in that release. A future capability can be optional for operators while its safety criteria are required before shipping it.

> **Execution rule:** Follow dependencies within each release, retain independent review gates, and mark tasks complete only against verified evidence. v0.1.0 is the released baseline. Its historical checklist and evidence below are preserved, including unresolved bookkeeping; this planning update does not certify previously pending platform or publication checks. Release numbers beyond v0.1.0 are planning targets.

## Release overview and scope

| Release | Goal | REQUIRED scope | OPTIONAL / DEFERRED scope |
|---|---|---|---|
| **v0.1.0 — Public Preview** | Released Rust library and experimental volatile service | Preserve verified API, networking, lifecycle, examples and release history | Persistence/import absent; existing batches remain supported, not future work |
| **v0.2.0 — Integration & Graph Loading** | Rebuildable single-graph accelerator fed by an external authority | Integration contract, atomic HTTP import, identity/version model, ordered events with duplicate/gap detection, synchronization-aware bootstrap/readiness, external feeder and reproducible measurements | Local WAL/AOF, automatic snapshots and disk recovery DEFERRED; export OPTIONAL; embedded provider connectors, multi-graph and advanced concurrency DEFERRED |
| **v0.3.0 — Operational Service** | Operable integration and measured update/query efficiency | Batch compatibility and integration improvements, lifecycle, deployment, metrics/tracing; evidence-based contention decision | Concurrency redesign only if justified; local persistence not a prerequisite |
| **v0.4.0 — Optional Local Persistence & Fast Recovery** | Reduce startup/reconstruction cost while preserving external authority | Evaluation and independent review before shipping any selected persistence mode; synchronization after local recovery | Persistence OPTIONAL and disabled by default; snapshot-only, AOF-only and combined modes are candidates, not three implementation commitments |
| **v1.0.0 — Production Readiness** | Supported operational, security and compatibility guarantees | Deployment threat model, supported API/integration compatibility, fault tests and runbooks | Multi-graph demand-gated; local persistence need not be enabled or implemented to qualify a volatile deployment; general-purpose/distributed database features excluded |

**Exposure rule:** Loopback or a trusted, access-controlled environment remains the boundary. Untrusted exposure makes authentication/authorization and abuse tests blocking for that release, regardless of their later roadmap placement. Import and feeder ownership are security-sensitive capabilities.

## Feature Progress Dashboard

Checks reflect verified implementation and acceptance gates. Features 0.2.5+ remain unchecked pending implementation and acceptance.

### v0.1.0 — Public Preview
- [x] **Feature 0.1.1:** Inventory and freeze minimum release scope (P0)
- [x] **Feature 0.1.2:** Document v0.1.0 service contract (M0-min, P0)
- [x] **Feature 0.1.3:** Establish a safe deployment boundary (M8-min, P0)
- [x] **Feature 0.1.4:** Minimal health, readiness and lifecycle (M4-min, P0)
- [x] **Feature 0.1.5:** Rust library adoption, API and examples (P0)
- [ ] **Feature 0.1.6:** CI, packaging and release candidate validation (M9-min, P0)

### v0.2.0 — Integration & Graph Loading

- [x] **Feature 0.2.1:** External source-of-truth integration contract (REQUIRED)
- [x] **Feature 0.2.2:** Bounded atomic HTTP graph import (REQUIRED)
- [x] **Feature 0.2.3:** Graph identity, generation, local revision and source position (REQUIRED)
- [x] **Feature 0.2.4:** Ordered incremental updates, duplicate and gap detection (REQUIRED)
- [x] **Feature 0.2.5:** Synchronization-aware bootstrap and readiness (REQUIRED)
- [x] **Feature 0.2.6:** External feeder example, restart/rebuild tests and reproducible measurements (REQUIRED)

### v0.3.0 — Operational Service

- [ ] **Feature M3:** Compatible batch processing, retry semantics and update efficiency
- [ ] **Feature M4:** Operational lifecycle, configuration and deployability
- [ ] **Feature M6:** Integration-aware observability
- [ ] **Feature M5:** Measured contention, query concurrency and isolation

### v0.4.0 — Optional Local Persistence & Fast Recovery

- [ ] **Feature P1:** Persistence value, strategy and support decision
- [ ] **Feature P2:** Canonical local checkpoint and snapshot evaluation
- [ ] **Feature P3:** AOF value and protocol evaluation
- [ ] **Feature P4:** Source-aware recovery, retention and lifecycle
- [ ] **Feature P5:** Fault injection, migration and recovery-cost evidence

### v1.0.0 — Production Readiness

- [ ] **Feature M8:** Supported security and access control
- [ ] **Feature M9:** Packaging, compatibility and operations
- [ ] **Feature M7:** Multi-graph management (OPTIONAL, demand-gated)

## Guiding principles

1. **External authority.** In integration mode, source changes are recorded externally before being delivered to Knotrel. An in-memory acknowledgment does not commit anything in the source or guarantee local durability.
2. **Correctness before throughput.** Preserve exact graph semantics, limits and error behavior. Synchronization failures cannot silently produce a ready but divergent graph.
3. **Separate concerns.** Core algorithms remain synchronous and independent of HTTP/storage; the server owns protocol and execution; feeders own provider access. Retain Rust 1.98.1, edition 2024 and Elastic-2.0.
4. **Explicit versions and ordering.** Graph identity, instance identity, generation, local revision, source position and persistence checkpoint are different concepts. Publish coherent graph/version tuples.
5. **Backward compatibility.** Preserve standalone volatile mode, Rust APIs and the existing ordered, continue-after-domain-error batch endpoint. A new feeder protocol may have a separately documented failure model; do not silently change `/v1/batch`.
6. **Bounded and observable.** Bound admission, import bodies, candidate construction, replay and deduplication state. Cardinality limits alone do not prove a memory or latency bound.
7. **Persistence is optional.** Select it only for demonstrated recovery benefits. Local restore never proves source synchronization. No implicit downgrade or unsupported acknowledgment guarantee.
8. **Evidence before performance claims.** Compare equivalent semantics and include loading, catch-up, HTTP and memory costs where applicable. Preserve before/after workloads and regressions.

---

## v0.1.0 — Public Preview (complete these in order)

**Release promise:** Publicly usable Rust library and experimental single-process HTTP server. **In-memory only:** graph state is lost when the process stops. No durability, crash recovery, import throughput or small performance improvement is promised.

### 0.1.1 — Inventory and freeze the minimum release scope (P0)

- [x] **Feature Complete: 0.1.1 — Inventory and freeze the minimum release scope**
- [x] **Audit HEAD and the current working tree.** Confirm crate/package layout, supported BFS/ETT/HDT entry points, HTTP routes, state ownership, locking, validation, existing health endpoints, test coverage and outstanding changes. Record what already exists; avoid duplicate work.
- [x] **Define the public-preview scope.** List supported graph operations, supported engines, single-graph assumption, accepted node identifiers, default configuration and non-goals in one release-scope note.
- [x] **Choose a supported runtime matrix.** State Rust MSRV if intended, supported OS/architectures, and whether the service binary is supported outside the development environment. Do not claim platforms that CI has not verified.
- [x] **Record release blockers separately from future enhancements.** Decide whether any known correctness, memory/resource, or API contract issue must be fixed before tagging.
- [x] **Acceptance:** A short `docs/releases/v0.1.0-scope.md` (or repo-conventional equivalent) maps required features to current implementation/tests and has no unresolved release-blocking questions.

Evidence and execution plan: [v0.1.0 scope](docs/releases/v0.1.0-scope.md).
Closed after the local gates passed on the uncommitted working tree: fmt,
Clippy, Rustdoc, the full workspace test run (including `tests/startup.rs`) and
`codectl` from the workspace root. Later features are not implicitly complete.

### 0.1.2 — Document the v0.1.0 service contract (M0-min, P0)

- [x] **Feature Complete: 0.1.2 — Document the v0.1.0 service contract**
- [x] **Specify server state semantics:** one in-memory graph per process (unless current implementation proves otherwise), graph initialization, lifetime, restart behavior, and visibility of successful changes.
- [x] **Specify requests and responses:** existing route paths, payloads, success/no-op behavior, error/status mapping, unknown nodes, duplicate/reversed edges, self-loops, graph size limits and request validation order. Preserve existing behavior or explicitly document deliberate changes.
- [x] **Specify concurrency and order:** when two requests overlap, define the observable serialization/consistency guarantee supported by the current lock/handler implementation. Do not promise parallel reads or transactional batches without evidence.
- [x] **Specify resource bounds:** max request bytes, graph node/edge limits, invalid payload handling, timeouts and behavior on resource exhaustion. Define safe defaults or document unsupported limits as release blockers if exploitable.
- [x] **Label all non-guarantees:** no persistence, no recovery, no HA, no rollback across multiple requests, no cross-request transaction and no external-exposure security guarantee.
- [x] **Add protocol-level contract tests** for supported operations, malformed inputs and error codes. Confirm both Rust library and HTTP behavior without relying on performance benchmarks.
- [x] **Acceptance:** A caller can use the API and predict the result of every supported operation and what is lost on restart. Automated contract tests match the documentation.

Evidence: [HTTP contract](docs/http-api.md) and
[contract tests](crates/knotrel-server/tests/contract.rs). Unlimited graph size
and absent timeouts are documented unsupported bounds, so exposure to untrusted
clients stays blocked until 0.1.3/0.1.4 (0.1.3 has since added finite default
graph limits; timeouts and connection limits remain 0.1.4 work).

### 0.1.3 — Establish a safe deployment boundary (M8-min, P0)

- [x] **Feature Complete: 0.1.3 — Establish a safe deployment boundary**
- [x] **Default to loopback binding** (`127.0.0.1` or platform equivalent) unless the existing server already has a demonstrably safer policy. Make non-loopback exposure an explicit operator choice.
- [x] **Document the trust model:** no built-in authentication/authorization is promised for the preview unless verified. Provide reverse-proxy/private-network guidance, but do not describe a private network alone as sufficient security.
- [x] **Validate input and bound requests** before expensive parsing/allocation where possible; set reasonable request-body limits and reject malformed data with safe errors.
- [x] **Verify no secrets or graph payloads leak** through default logs, panic output or release artifacts. Avoid shipping benchmark environment paths where not necessary.
- [x] **Test exposure/configuration:** startup defaults, invalid bind/config values, oversized/malformed input and clear failure when an unsupported deployment mode is requested.
- [x] **Acceptance:** Out-of-box use is restricted to a trusted local boundary. Any deployment exposed to untrusted clients remains explicitly unsupported until full M8 is delivered.

Evidence: [trust model](docs/http-api.md#trust-model),
[security tests](crates/knotrel-server/tests/security.rs) (defaults and
boundaries on all engines, 413 before parse, batch cap, error sanitization,
non-loopback gate) and the configuration unit tests. Defaults of 100 000 nodes
and 500 000 edges are conservative cardinality caps, not benchmark-derived
memory bounds. The process-level `tests/startup.rs` check (invalid bind/config
exits before listening) passed locally (`1 passed`). It runs in the normal CI
suite (`cargo test --workspace --locked` on Linux, Windows and macOS), but no CI
run exists yet for these uncommitted changes, so CI evidence on the exact
revision is still to be collected once the change is pushed.

### 0.1.4 — Minimal health, readiness and lifecycle (M4-min, P0)

- [x] **Feature Complete: 0.1.4 — Minimal health, readiness and lifecycle**
- [x] **Implement or verify liveness** (`/health` or repo-consistent route) responding while the server process is functional; do not imply graph persistence.
- [x] **Implement or verify readiness** (`/ready` or repo-consistent route), true only after initialization and false during startup/shutdown or known fatal conditions; define exact states in documentation.
- [x] **Handle SIGINT/SIGTERM gracefully:** stop accepting requests, bound the drain period, exit with a meaningful status and avoid leaving corrupted protocol output. No disk-flush guarantee is required for a volatile graph.
- [x] **Validate startup configuration:** bind address/port, engine, graph limits, request size, logging and timeout defaults. Reject obviously inconsistent values clearly.
- [x] **Add lifecycle smoke tests:** startup, readiness transition, one mutation plus connectivity query, shutdown, restart with empty graph, failed bind and malformed configuration.
- [x] **Acceptance:** The server can be started, probed, stopped and restarted predictably, and the docs explicitly state that restart loses graph state.

Evidence: [`/ready` and lifecycle contract](docs/http-api.md#lifecycle-health-and-shutdown);
in-process socket tests ([lifecycle](crates/knotrel-server/tests/lifecycle.rs)) and
real-binary SIGTERM/SIGINT tests ([process](crates/knotrel-server/tests/process.rs), Unix only);
invalid-configuration cases in [startup](crates/knotrel-server/tests/startup.rs).
"Stop accepting" means new work is rejected with 503 `shutting_down` (the listener
stays open during the drain only to answer such requests). Outstanding graph operations
are tracked independently via worker-owned tokens so shutdown drains both HTTP connections
and graph workers under the configured deadline (exceeding it yields `DeadlineExceeded` and
process exit code 2). A race-free observation protocol in `serve()` pairs `borrow_and_update()`
with `changed()` to prevent missed wakeups and disabled arms at worker completion boundaries.
Worker panic is published directly to readiness via an RAII guard so `/ready` reports 503
`unavailable` even if the HTTP caller is cancelled or timed out. Fault-injection machinery
is removed from the default public API and isolated behind an unsupported, disabled-by-default
`test-support` feature. Standalone process terminates with exit 2 on deadline expiry while
embedded callers receive `DeadlineExceeded` to account for lingering blocking workers.
Regression tests cover observation boundaries, queued caller cancellation accounting,
failed graph early rejection, orphaned workers, deadline expiration, worker panic reflection
in `/ready`, clean worker drain, and second-signal forced exit.
Limits: tests ran in debug mode locally only, so release-mode smoke tests per target,
Windows signal behavior and CI on the exact revision remain unevidenced; there is no
write timeout or slow-reader protection, and a timed-out mutation has an unknown outcome
because it may still be queued or running in the background, racing with later reads.


### 0.1.5 — Rust library adoption and examples (P0)

- [x] **Feature Complete: 0.1.5 — Rust library adoption and examples**
- [x] **Document public library API:** graph construction (`try_from_edges`), `add_node`, `link`, `cut`, `connected`, supported backends, limits, errors and update behavior. Verify names/signatures against source.
- [x] **Add runnable Rust examples:** minimal connected/disconnected graph, dynamic bridge cut/relink and bulk construction, with expected output. Ensure examples compile in CI.
- [x] **Explain engine choice:** simple, evidence-grounded guidance on BFS/ETT/HDT trade-offs; link to benchmark evidence while clearly stating limits and avoiding unsupported blanket performance claims.
- [x] **Audit public docs and API hygiene:** crate descriptions, license/README links, rustdoc examples, exposed types/traits, and whether package metadata is suitable for publication. Do not promise API stability before 1.0.
- [x] **Cross-check standalone library use:** examples should not require the HTTP server, private benchmark tools or a development-only directory structure.
- [x] **Acceptance:** A new Rust user can copy the README example into a fresh project and use core connectivity operations without understanding the internal algorithms.

### 0.1.6 — CI, packaging and release candidate (M9-min, P0)

- [ ] **Feature Complete: 0.1.6 — CI, packaging and release candidate**
- [x] **Automate correctness gates:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and warning-denied Rustdoc for supported workspaces, plus any relevant Python collector tests where the benchmark repo participates.
- [x] **Pin reproducible build inputs:** commit/revision identifiers, lockfile policy, compiler/toolchain information and supported build instructions. Avoid conflating the benchmark repo's independent version with the service release.
- [ ] **Build/test the release binary:** clean build, startup, health/readiness, an API mutation/query, graceful shutdown and restart-empty behavior on every claimed supported platform (verified locally on macOS aarch64; Linux and Windows native CI jobs configured pending remote push).
- [x] **Review legal and publishing metadata:** license and third-party notices, package naming/availability, dependency licenses, release artifacts, GitHub README/repository links and any security contact policy.
- [x] **Publish documentation:** quick start, Rust example, curl/API examples, config reference, HTTP error table, architecture overview and **known limitations** prominently describing volatile state.
- [x] **Prepare release notes and compatibility policy:** summarize supported behavior, known gaps, preview status, breaking-change expectations before 1.0 and upgrade expectations (none for in-memory state).
- [x] **Perform a final clean-tree / diff / artifact review** for source, datasets and personal paths. Decide explicitly what benchmark raw evidence should be published; do not silently commit thousands of experimental files.
- [ ] **Create release artifacts only after approval:** tag `v0.1.0`, publish binary/container and optionally crate(s) only if the build/publish audit passes. Tagging/pushing/publishing is a separate authorized action, not implicit in completing this checklist.
- [ ] **Acceptance / v0.1.0 release gate:** Contract tests, examples, security boundary, smoke tests, documentation and supported-platform CI are green. The service is clearly labeled **experimental and in-memory**; there is no unsubstantiated production-readiness claim.

---

## v0.2.0 — Integration & Graph Loading

**REQUIRED outcome:** A single derived graph can be loaded atomically, advanced by ordered source updates and rebuilt after restart without confusing memory acceptance with durability. Existing standalone volatile and embedded Rust behavior remains supported.

**Not a release requirement:** Local WAL/AOF, automatic snapshots, filesystem crash recovery, provider-specific server connectors, multi-graph or concurrency redesign. A transport graph image is not a durable local checkpoint. Optional export must not delay the required integration path.

### 0.2.1 — External source-of-truth integration contract (P0, REQUIRED)

- [x] **Feature Complete: 0.2.1 — External source-of-truth integration contract**
- [x] Define standalone volatile versus opt-in source-managed mode, startup selection, request admission and mode-specific readiness. Existing callers must not acquire an external-source dependency.
- [x] Define feeder/source responsibilities: obtain a coherent graph image associated with source position S; retain/consume all relevant changes after S; deliver one logical ordered stream; explicitly complete bootstrap at a declared barrier B. An opaque cursor alone cannot prove continuity: specify a predecessor token, contiguous delivery sequence or another verifiable continuity protocol. Partition merging and database snapshot/event coordination remain feeder responsibilities.
- [x] Define acknowledgment, visibility, timeout/disconnect and retry semantics. An accepted update means applied to memory with its cursor; the authoritative source must already contain it. No exactly-once network delivery or disk durability is implied.
- [x] Specify version tokens and event failure rules needed by 0.2.2–0.2.5 before implementation. Decide feeder ownership/fencing, stale-session rejection, duplicate retention and behavior when source history is unavailable.
- [x] Prevent untracked writes from invalidating the source cursor: select and document a policy for legacy mutating requests in source-managed mode (recommended: reject them); retain legacy behavior in standalone mode. Queries remain compatible. Feeder events do not silently inherit legacy batch error continuation.
- [x] Define synchronization/freshness policy after bootstrap: barrier completion is not proof of perpetual freshness. Decide behavior on feeder disconnection, lag, source epoch changes and known gaps, without inventing SLOs.
- [x] **Acceptance:** Publish an implementable contract with request/response examples, state transitions, error precedence and a decision table for lost responses, duplicates, conflicting duplicates, gaps, stale generation, invalid source events and restart. Maintainer approval of unresolved decisions precedes coding.
- [x] **Tests specified:** Deterministic traces for coherent image S + catch-up to B, event-before-image races, reordered/duplicate events, timeout after admission, old feeder after replacement and source history expiration. Pin seeds and expected graph/version tuples; no runtime implementation is required for this contract feature.

Evidence: [Normative integration contract](docs/integration-contract.md);
[ADR 0012](docs/architecture/0012-external-source-of-truth-integration-contract.md).
Specification defines standalone vs. source-managed modes, feeder synchronization ($S \to \text{catch-up} \to B$),
delivery sequence and predecessor tokens, the 12 specified edge-case resolutions, bounded duplicate-detection, error precedence,
readiness/freshness state machine, acceptance test matrix, and D1–D6 decision resolutions.
Accepted and closed after independent review and maintainer approval.


### 0.2.2 — Bounded atomic HTTP graph import (P0, REQUIRED)

**Depends on:** 0.2.1, including the version/precondition model. Implement the minimal identity fields needed for safe publication here; 0.2.3 completes their shared exposure and validation. Do not ship unguarded replacement while waiting for 0.2.3.

- [x] **Feature Complete: 0.2.2 — Bounded atomic HTTP graph import**
- [x] Define create-only versus replace, required preconditions, explicit vertices/edges, source identity/epoch and snapshot position, payload schema, node-ID encoding and error precedence. Preserve `try_from_edges` semantics: nodes before edges, isolates retained, duplicate/reversed edges are no-ops, implicit endpoints allowed, self-loops rejected, configured limits enforced.
- [x] Bound body bytes, raw item counts (including duplicates), concurrent imports and candidate resources. A fully buffered bounded format is acceptable; streaming is DEFERRED. Document that graph cardinality limits need not imply the entire graph fits one import body.
- [x] Build a candidate away from live state, then recheck generation/revision preconditions under the mutation boundary. Publish topology and all associated metadata together. Failed parse/build/conflict leaves the live graph, generation, revision and cursor unchanged.
- [x] Define read behavior during construction and catch-up: old-state reads, if supported, must carry the old generation and obey freshness policy; initial bootstrap must not present an empty or partially synchronized graph as ready. Prevent queued old-generation events from applying to the new graph.
- [x] Define cancellation before publication versus an uncertain outcome after admission/publication; do not promise rollback after a successful swap. A process crash loses volatile state and triggers external bootstrap, not local recovery.
- [x] **Acceptance:** No reader observes half an imported graph or a graph paired with another generation's cursor. Successful import acknowledges coherent in-memory publication, not readiness or persistence. Reads and metadata obey the selected bootstrap policy.
- [x] **Tests:** Every engine; empty/isolate/disconnected/cyclic/dense graphs; full-width IDs; duplicates; malformed/oversized payloads; limits; competing imports/writes; cancellation; crash/restart; differential future mutations against incremental construction.

Evidence: [Import integration test suite](crates/knotrel-server/tests/import.rs)
covering 15 deterministic scenarios via `WorkerPauseHandle` and `on_drain_loop`:
1. Admission before body consumption & concurrency ceiling at one (competing import rejected with 503 `import_in_progress` without consuming body).
2. Fresh server identity generated per incarnation with isolation across cloned configurations and restarts (discovering identity via `GET /v1/info`, rejecting stale instance IDs with 409 `stale_instance_id`).
3. Exact ownership fencing in both directions (heartbeat and import reject both lower and higher unregistered epochs with 409 `fenced_feeder_session`).
4. Contract-compliant composite replacement preconditions and percent-encoded cursors (precondition mismatches return 412 `precondition_failed`, re-initialization returns 409 `graph_already_initialized`, mutations return 409 `direct_mutation_disabled`).
5. Deterministic shutdown with zero HTTP connections and active import worker via `lifecycle.on_drain_loop` observing `draining && tasks_empty && active_jobs == 1` without arbitrary sleeps.
6. Off-lock candidate construction and read isolation (concurrent `GET /v1/stats`, `POST /v1/operations` queries, and heartbeats complete against baseline graph while worker builds candidate).
7. Caller cancellation after blocking worker start (permit and job guard retained, publication completes in memory).
8. Worker panic after cancellation (job guard released, service marked unavailable, liveness probe remains functional).
9. Session takeover and lease expiry before publication (candidate build discarded, rejecting with 409 `fenced_feeder_session` and `lease_expired`).
10. Shutdown deadline expiry reporting `ShutdownOutcome::DeadlineExceeded { abandoned }`.
11. Domain error rollback (self-loop rejected with 422 `self_loop`, preserving baseline graph).
12. Graph identity validation (only `graph_id == "default"` admitted; unsupported IDs rejected with 400 `invalid_graph_id` without mutating or echoing arbitrary IDs).
13. HTTP Content-Type validation before body consumption (missing or unsupported media types rejected with 415 `invalid_request` without publication).
14. Strict replacement precondition negative validation (rejecting unquoted, unmatched, repeated, weak, wildcard, list, and malformed tags with 412 `precondition_failed`).
15. Error classification preserving contract (distinguishing syntax errors returning 400 `invalid_request` from schema/data rejections like invalid node IDs returning 422 `invalid_request`).

Limitations: Streaming import body parsing is deferred (bounded buffered payload up to `max_import_body_bytes`); ordered event stream processing remains Feature 0.2.4; synchronization state machine and readiness gating remain Feature 0.2.5. Accepted and closed after independent review and acceptance.

### 0.2.3 — Identity, generation, revision and source position (P0, REQUIRED)

- [x] **Feature Complete: 0.2.3 — Identity, generation, revision and source position**
- [x] Expose coherent metadata for one implicit graph; identity is not multi-graph routing. Separate graph identity (logical projection) from instance identity (process/service incarnation).
- [x] A generation identifies a published baseline/rebuild lineage; revision counts effective local changes within its documented scope. Preserve existing decimal revision behavior for standalone mode; define import/reset behavior explicitly. Prevent stale tokens from becoming valid after restart/replacement, even if topology and numeric revision match.
- [x] Source position identifies the applied prefix of a specific source stream/epoch. It is not the graph revision: an accepted no-op event can advance source progress without changing topology or local revision. Do not compare opaque provider cursors lexicographically or assume they are contiguous integers.
- [x] Reserve a distinct concept for a future local persistence checkpoint: the graph/version/source tuple actually recoverable from local storage, which may lag live state. v0.2.0 has no such durability guarantee.
- [x] Add backward-compatible metadata/precondition exposure and error mapping; keep the existing `/v1/info` immutable-settings versus `/v1/stats` coherent-live-state distinction. Do not advertise a token from one snapshot with a query result from another.
- [x] **Acceptance:** Identity and version scopes, no-op/error rules, restart behavior and conditional writes are documented and enforced without narrowing the existing arbitrary-precision revision to a machine integer.
- [x] **Tests:** Equal numeric revisions in distinct instances/generations, revision carry boundaries, no-op source advancement, errors not falsely advancing progress, coherent concurrent reads and stale preconditions.

Evidence: [Identity, generation, revision, and metadata integration test suite](crates/knotrel-server/tests/identity.rs)
and [service unit test suite](crates/knotrel-server/src/service.rs) covering 12 deterministic integration scenarios and 3 focused service tests:
1. Equal numeric revisions in distinct instances and generations (instance ID fencing and stale generation rejection).
2. Cloned configuration yields fresh isolated identities (fresh UUID generated per incarnation, cross-instance rejection).
3. Revision carry boundaries and arbitrary precision preservation (multi-digit 9 -> 10 carry; no-ops, queries, and errors do not increment revision).
4. Metadata coherence when queries race atomic replacement (synchronized interleaving via `WorkerPauseHandle`; queries observe either pre-replacement or post-replacement state under a single lock boundary, never split metadata).
5. Stale replacement tokens after publication (ETag derived from generation N rejected with 412 `precondition_failed` against generation N+1).
6. Old instance tokens rejected by a new server (incarnation tokens fail with 409 `stale_instance_id` or 412 `precondition_failed` across restarts).
7. Failed operations and imports preserve metadata (rejected graph IDs, domain errors, and precondition mismatches leave generation, revision, and source cursor intact).
8. Cursor encoding and token round trips (RFC 3986 percent-encoding safely escapes delimiters; round-tripping via `If-Match` succeeds).
9. Standalone compatibility and batch metadata scope (`/v1/info` and `/v1/stats` schemas preserved; batch response headers reflect final state under continue-on-error).
10. Generation bounds and overflow precondition rejection (`u64::MAX` generation precondition checked and rejected with 412 `precondition_failed`).
11. Response coherence across replacement with paused response delivery (query outcome and metadata captured under graph lock, worker paused via `pause_next_worker_after_lock` after releasing lock, replacement published on live graph, resumed response preserves original topology, generation, revision, cursor, and replacement token).
12. Arbitrary-precision revision beyond `u64::MAX` over HTTP (seeded beyond `u64::MAX` with multi-digit carry `599` -> `600`, no-op and rejection preservation, exact decimal header and ETag exposure).
13. Service-level actual generation exhaustion (`service_level_actual_generation_exhaustion_on_import_publication_path` initializes live generation to `u64::MAX`, executes import publication with matching preconditions, asserts `generation_exhausted`, and proves full preservation of baseline topology, generation, revision, source lineage, source position, and sync status).
14. Service-level arbitrary-precision mutation path (`arbitrary_precision_revision_beyond_u64_max_with_multi_digit_carry` proves execution through real mutation path beyond `u64::MAX`, multi-digit carry, no-op and rejected mutation preservation).
15. Post-lock hook panic after caller cancellation (`post_lock_hook_panic_after_caller_cancellation_marks_service_unavailable` proves panic in post-lock hook permanently marks service unavailable and fails future requests even when caller has aborted its future).

Limitations: Incremental source-progress cases without topology change (no-op feeder events advancing applied source cursor) and ordered event streaming belong to Feature 0.2.4; synchronization barrier verification and automated readiness gating belong to Feature 0.2.5. Accepted and closed after independent review and acceptance.

### 0.2.4 — Ordered incremental updates (P0, REQUIRED)

- [x] **Feature Complete: 0.2.4 — Ordered incremental updates**
- [x] Add a source-managed update contract separate from legacy mutation/batch semantics. Validate source identity/epoch, generation and feeder ownership before applying an event; use one serialization boundary for graph changes and applied cursor.
- [x] Define ordering and bounded duplicate detection: same identity with conflicting content is rejected; exact retries do not repeat effects; events outside the retained verification window receive an explicit stale/unverifiable outcome rather than an unearned claim of successful deduplication.
- [x] Detect missing predecessors/out-of-order delivery. Do not advance the applied source position past a gap or failed event. A known gap makes synchronized serving unavailable until repaired or rebuilt; arrival of a later event is not evidence that earlier events can be skipped.
- [x] For invalid source events, stop progress and report the failing position; require explicit remediation/rebuild. Decide event atomicity before supporting multi-operation source transactions. A safe initial restriction is one graph operation per event; do not invent transaction rollback.
- [x] Ensure worker-owned admission, generation checks and result accounting survive HTTP cancellation. Retries after restart must establish the new instance/generation; no durable request ledger is promised.
- [x] **Acceptance:** Applied state corresponds to a verified ordered source prefix. Duplicate link/cut idempotence alone is insufficient: an old link retried after a newer cut must not recreate the edge. Capacity rejection cannot be reported as source progress.
- [x] **Tests:** Lost acknowledgment, exact/conflicting/expired duplicates, gaps, reordering, source epoch reset, no-op events, limit/error cases, concurrent feeders, queued events racing import, worker panic and restart/rebuild.

Evidence: [Ordered incremental updates integration test suite](crates/knotrel-server/tests/events.rs)
and [service unit test suite](crates/knotrel-server/src/service.rs) covering 19 deterministic integration scenarios and unit tests:
1. Lost acknowledgment and duplicate retry (retained sequence matching `event_id` and canonical payload returns 200 `already_applied` without repeating side effects).
2. Regression invariant: old link retry after newer cut (link A-B, cut A-B, retrying old link at D=1 returns 200 `already_applied` without modifying graph topology; edge A-B remains strictly absent).
3. Conflicting duplicate event content (retained sequence with modified payload rejected with 409 `conflicting_duplicate` and transitions to `Desynchronized`).
4. Retained committed sequence with different event ID (seq D committed with `evt-A`, retried with `evt-B` rejected with 409 `conflicting_duplicate` and transitions to `Desynchronized`).
5. Retained event ID reused at new sequence (`evt-A` committed at seq 1, reused at seq 2 rejected with 409 `conflicting_duplicate` and transitions to `Desynchronized`).
6. Evicted duplicate outside bounded window ($D \le D_{applied} - W$ rejected with 409 `unverifiable_duplicate`; `CatchingUp` preserved).
7. Evicted event ID reused at new sequence (event ID evicted from ring buffer can be safely reused at $D_{applied} + 1$).
8. Missing predecessors and gap detection ($D > D_{applied} + 1$ or mismatched $D_{prev}$ rejected with 409 `gap_detected`).
9. Source lineage and epoch failover rejection (mismatched `source_id` or `source_epoch` rejected with 409 `source_mismatch` / `source_epoch_mismatch` and transitions to `Desynchronized`).
10. No-op mutations and progress events (no-op link and `progress` event advance `applied_source_position` and delivery sequence without incrementing `state_version`).
11. Fail-fast batch execution and partial error reporting (first domain error stops processing; committed prefix retained; partial failure reports failing index, seq, event ID, and current applied frontier; uncommitted event retried at $D_{applied} + 1$).
12. Queued events racing baseline replacement (events targeting generation N rejected with 409 `stale_generation` once generation N+1 is published).
13. Session takeover and lease expiry before execution (fenced epoch rejected with 409 `fenced_feeder_session`; expired lease rejected with 409 `lease_expired`).
14. Worker panic after caller cancellation (panic in event worker permanently marks service `unavailable`; subsequent requests rejected with 503 `unavailable`).
15. Stale instance ID rejected across restarts (previous server incarnation ID rejected with 409 `stale_instance_id`).
16. Standalone and direct mutation compatibility (standalone rejects feeder endpoints with 409 `mode_conflict`; source-managed rejects direct mutations with 409 `direct_mutation_disabled`).
17. Declared gap and desynchronized state (`POST /v1/feeder/gap` acknowledges gap, transitions to `Desynchronized`; subsequent queries return 503 `desynchronized`; heartbeat renews lease but probe reports `desynchronized`).
18. Payload schema and bounds validation (validates event limits 1..1024, node IDs, required fields; returns 422 `invalid_request`).
19. Stop event application after desynchronization and rebuild recovery (once `Desynchronized`, subsequent mutation and progress events rejected with 409 `graph_desynchronized` under lock; entire state strictly preserved; `/stats` remains available for diagnosis; atomic replacement rebuild via `POST /v1/feeder/import` publishes generation 2 in `CatchingUp` where ordered ingestion resumes).
20. Production boundary sequence semantics and checked arithmetic (`feeder_events_sequence_exhaustion_and_arbitrary_precision_carry` unit test verifies that for the `u64` wire protocol, retries at `u64::MAX` succeed as `duplicate`, wrapped sequence zero is rejected as `unverifiable_duplicate` without mutation, and `SequenceExhausted` serves as a defensive guard for checked arithmetic that is unreachable over the wire).
21. Under-lock desynchronization rejection (`feeder_events_rejected_after_desynchronization` unit test verifies that declared gaps and protocol mismatches immediately halt subsequent event ingestion without mutating topology, revision, sequence, or position).

Limitations: Synchronization barrier verification (`POST /v1/feeder/barrier`), barrier timeout, and automated readiness promotion belong to Feature 0.2.5. Accepted and closed after independent review and acceptance.


### 0.2.5 — Synchronization-aware bootstrap and readiness (P0, REQUIRED)

- [ ] Implement source-managed transitions covering awaiting-image, loading, catching-up, synchronized, out-of-sync/failed and draining. Map these to the public probe contract without silently changing exhaustive public Rust enums or standalone readiness.
- [ ] Bind bootstrap completion to the same generation/source epoch and a verified applied position covering barrier B. An arbitrary "ready" request, completed import or local file restore is insufficient.
- [ ] Keep liveness independent of graph synchronization. Define which graph/admin routes remain available while unready so a feeder can perform bootstrap; diagnostic metadata must identify generation and synchronization state.
- [ ] Apply the approved ongoing freshness policy for feeder loss/lag. A known gap cannot remain synchronized; uncertain source freshness must be reported without claiming knowledge of the source's current head.
- [ ] Integrate import/event jobs with existing drain accounting; admitted workers may outlive HTTP callers. Forced exit loses local volatile state; restart starts a new bootstrap, never an empty graph labeled synchronized.
- [ ] **Acceptance:** Source-managed mode is not ready until bootstrap is verified; a gap invalidates readiness. Standalone mode retains its existing startup semantics. Drain/timeout behavior does not falsely acknowledge canceled or unfinished synchronization.
- [ ] **Tests:** Barrier before/after catch-up, obsolete completion signals, wrong generation/epoch, feeder disconnect/reconnect, source unavailable, restart, panic, timeout and draining with active import/update workers. Use deterministic synchronization points.

Evidence: [Synchronization-aware bootstrap and readiness test suite](crates/knotrel-server/tests/bootstrap.rs)
and [service unit test suite](crates/knotrel-server/src/service.rs) covering 21 deterministic integration scenarios and unit tests:
1. Happy path: initial import S -> catch-up -> barrier B -> Ready (TR-01).
2. Equal barrier sequence with mismatched position (TR-15, Case B7 -> 409 `barrier_position_mismatch`).
3. Heartbeat after lease expiry while updates remain unapplied (TR-17; heartbeat extends deadline but probe remains 503 `stale` until barrier verifies catch-up under fresh epoch).
4. Heartbeat during desynchronized and catching-up (TR-18; probe remains 503 `desynchronized` or `catching_up`).
5. Older barrier with different position (TR-24, Case B9 -> 409 `superseded_barrier`).
6. Rejected barriers preserving Ready, Stale, and Desynchronized states (TR-25).
7. Import precondition failure vs ownership fencing (TR-28).
8. Exact barrier match while Uninitialized (TR-29, Case B3 -> 409 `graph_uninitialized`).
9. Barrier during initial baseline Loading (TR-30, Case B4 -> 409 `graph_uninitialized`).
10. Barrier during replacement Loading (TR-31; live graph generation 1 evaluated against live frontier while generation 2 candidate is built off-lock).
11. Barrier with omitted source position (matches delivery sequence only).
12. Fenced feeder session and epoch validation in barrier (409 `fenced_feeder_session`).
13. Stale instance ID and invalid graph ID rejection in barrier (409 `stale_instance_id`, 400 `invalid_graph_id`).
14. Standalone mode barrier rejection (409 `mode_conflict`).
15. Worker panic marks ready unavailable (503 `unavailable`).
16. Draining probe gate and graceful shutdown (503 `draining`).
17. Query rejection in source-managed mode across uninitialized, loading, catching_up, stale, and desynchronized states.
18. Lock-free lease expiration detection in /ready without holding graph lock.
19. Delayed barrier race resolution across takeover and sequence advancement.
20. Defect 1 regression: delayed pre-expiry barrier fenced across lease expiry and heartbeat renewal (`test_20_delayed_barrier_fenced_across_lease_expiry`). Requires `feeder_epoch > last_expired_epoch` to restore readiness.
21. Defect 2 regression: paused `/ready` probe at expiry does not overwrite newer publication (`test_21_probe_paused_at_expiry_does_not_overwrite_newer_publication`), verified via atomic `PublishedReadiness` CAS.
22. Unit test `test_barrier_decision_table_b1_through_b9` verifying cases B1 through B9.
23. Unit test `test_published_readiness_packing_roundtrip` verifying packed atomic bitfields.
24. Unit test `test_clock_advance_and_lease_expiry_staleness` verifying controllable monotonic clock, staleness transitions, and epoch recovery.

Limitations: External feeder reference implementation and end-to-end reproducible measurements belong to Feature 0.2.6. Feature 0.2.5 remains unchecked pending maintainer acceptance.

### 0.2.6 — External feeder and reproducible integration evidence (P0, REQUIRED)

- [ ] Provide a minimal feeder outside the server, backed by a deterministic external fixture/event source. Demonstrate coherent image S, ordered updates, barrier B, duplicate retry, gap detection and rebuild when history is missing. A provider-specific production connector is not required.
- [ ] Document the feeder's source-side obligations: snapshot consistency, event retention, checkpoint ownership, projection/filter version and one ordered delivery stream. Persisting a feeder offset must not cause it to skip rebuilding a restarted volatile Knotrel instance.
- [ ] Add black-box restart/rebuild scenarios and engine/oracle comparisons. Include source unavailable, stale client session and invalid event recovery; never synthesize a successful source acknowledgment.
- [ ] Measure parsing, construction, publication, catch-up and total time-to-ready separately, plus query/update latency and memory peak. Record engine, graph/event fixture hashes, source/feeder configuration, versions, hardware, seeds and cold/warm protocol. No benchmarks are required for this roadmap edit.
- [ ] Compare the same workloads before/after changes; comparisons with competitors must use equivalent visibility, synchronization and durability semantics. Retain regressions and raw evidence; set numerical budgets only after explicit approval.
- [ ] **Acceptance:** Another engineer can reproduce initial load, incremental synchronization, lost-response retry and full restart/rebuild without an embedded provider connector or local persistence. All declared supported engines pass the same correctness contract; performance reports distinguish measurement from guarantees.

*Implementation Status (Feature 0.2.6):* Working reference code, deterministic end-to-end restart/rebuild tests, and reproducible measurement tooling are delivered:
- **External Feeder Reference Example:** [`crates/knotrel-server/examples/external_feeder.rs`](crates/knotrel-server/examples/external_feeder.rs) demonstrates the complete integration lifecycle over HTTP/JSON against a real server process (discovery, session registration, observable background heartbeat loop, baseline snapshot import with isolated nodes, ordered catch-up events with links/cuts/no-ops/progress, high-water mark barrier, independent oracle verification, controlled fault proxy response dropping with automated feeder recovery, process crash simulation and authoritative rebuild, and awaited clean shutdown). Built together with the server binary: `cargo build --bin knotrel-server --example external_feeder`.
- **Independent Ground-Truth Oracle:** `ExternalSourceOracle` in [`crates/knotrel-server/tests/common/feeder.rs`](crates/knotrel-server/tests/common/feeder.rs) implements pure standard library BFS/adjacency verification, evaluating reachability, node count, edge count, and source position without referencing internal Knotrel data structures, reporting actual verified agreement percentage.
- **Deterministic End-to-End Restart & Recovery Tests:** [`crates/knotrel-server/tests/e2e_rebuild.rs`](crates/knotrel-server/tests/e2e_rebuild.rs) exercises 21 automated scenarios against real child server processes: initial bootstrap oracle verification, no-op/progress records preserving revision, committed event with lost ACK deterministically dropped via `ControlledFaultProxy` and recovered via exact retry with immutable identities (never synthesizing success from a frontier comparison), import with lost ACK and conservative reconciliation returning explicit `UncertainOutcome` followed by safe conditional rebuild (`conditional_rebuild_snapshot` with mandatory composite `If-Match`), old link retry after newer cut preserving cut, graceful restart (SIGTERM) and abrupt process crash (SIGKILL) requiring rediscovery and authoritative rebuild, declared gap followed by authoritative rebuild (`mode: "replace"` with composite `If-Match`), lease expiry requiring epoch bump to restore Ready, partial batch failure stopping cleanly with committed-prefix inspection, authoritative changes occurring strictly after snapshot S and before import completion via controlled upload synchronization catching up to barrier B and verified vs oracle, unverifiable duplicate eviction requiring safe recovery, observable heartbeat fencing detection and clean shutdown, lost ACK combined with replacement before retry, lost ACK combined with restart, lost ACK combined with competing ownership takeover, import reconciliation on existing graph with same cursor but different topology, import that never committed returning explicit `UncertainOutcome` ("publication not observed on server"), replacement by another feeder before reconciliation, restart during uncertain import outcome, heartbeat failure halting feeder control flow under lost ownership, and competing publication with same counts and cursor but different topology returning explicit `UncertainOutcome` without synthesizing false publication success.
- **Reproducible Measurements:** [`crates/knotrel-server/examples/measure_e2e.rs`](crates/knotrel-server/examples/measure_e2e.rs) and [`scripts/measure-e2e.sh`](scripts/measure-e2e.sh) measure snapshot import duration/throughput, catch-up duration/throughput, barrier duration, total Time-to-Ready, pure query serving throughput and latency distribution (min, mean, p50, p95, p99, max) strictly separated from out-of-band oracle computation, sampled maximum process RSS (polled every 25ms; brief transient allocations between sampling intervals may not be captured) alongside final sampled process RSS, and restart recovery duration across deterministic synthetic workloads (`small`, `medium`, `large`), supporting repeated runs (`--runs <N>`) with retained timestamped evidence.
- **Documentation:** Full operational lifecycle, monitoring, and recovery procedures documented in [`docs/integration-contract.md`](docs/integration-contract.md) (§13.5) and [`docs/http-api.md`](docs/http-api.md). Feature 0.2.6 checkboxes remain unchecked (`- [ ]`) pending independent maintainer acceptance.

**Exit gate v0.2.0:** 0.2.1–0.2.6 accepted; source-managed synchronization is explicit and testable; standalone/Rust compatibility retained. Export is OPTIONAL and needs its own coherent graph/position contract if selected. No local durability claim is needed to meet this gate.

---

## v0.3.0 — Operational Service

**REQUIRED scope:** Build on v0.2.0 integration and v0.1.0 lifecycle. Batch improvements, deployment and observability remain priorities. Concurrency redesign is conditional on measured contention. Local persistence is not a dependency.

### Release-wide performance checkpoints (required)

The [v0.3.0 performance evaluation plan](docs/performance-v0.3.0.md) defines separate core and HTTP evidence, immutable source identity, workload matrices, timing boundaries and regression review. Historical algorithm rankings are not measurements of the released v0.2.0 HTTP server. The plan is proposed for independent review; no numerical regression budgets are approved.

- [ ] **P0 — Released baseline:** Pin v0.2.0 to its resolved commit, verify isolated sibling core dependency resolution, qualify collectors and record core/server baselines before execution changes. Preserve historical evidence.
- [ ] **P1 — M3/M4 regressions:** Compare batch/update execution, mixed traffic, admission, lifecycle and external rebuild against P0 using identical workloads and explicit candidate SHAs.
- [ ] **P2 — M6/M5 evaluation:** Measure instrumentation overhead before relying on telemetry; establish coarse-mutex contention, fairness and query tails before choosing any concurrency alternative.
- [ ] **P3 — Release qualification:** Repeat the complete approved matrix against v0.2.0 and the release candidate on comparable controlled hosts. Archive raw evidence, correctness checks, per-workload gains/losses and reviewed regression decisions.

**Performance exit gate:** Correctness and source/binary identity failures block release qualification. Missing or underpowered evidence is explicitly inconclusive, never a claimed pass. Reproducible regressions require investigation and an explicit disposition; noisy shared CI is restricted to correctness and bounded benchmark smoke. Required coverage exceptions and resource budgets need owner approval. Retain the coarse mutex when evidence does not justify replacement. M3.1 remains pending independent acceptance; these checkpoints do not mark any feature complete.

### M3 — Compatible batch processing and update efficiency (P1)

- [ ] **M3.1 Contract:** Inventory the existing `/v1/batch` behavior and extend result/version reporting compatibly. Preserve ordered execution, per-item domain errors, continuation and lack of rollback.

Evidence: [ADR 0013](docs/architecture/0013-compatible-batch-contract.md); [HTTP API specification](docs/http-api.md#batches); [batch integration test suite](crates/knotrel-server/tests/batch.rs) covering 1-to-1 outcome reporting, continue-on-error without rollback, revision progression arithmetic ($\Delta V = \sum [\text{changed} == \text{true}]$), decimal carry, intra-batch query visibility, concurrency isolation, mode segregation, and timeout uncertainty under background worker execution. Feature M3.1 remains unchecked pending independent review.
- [ ] **M3.2 Failure model:** Specify feeder batch boundaries separately if introduced. A stop-on-error applied-prefix protocol requires explicit new semantics, not a replacement of the legacy endpoint. "Applied" does not mean locally durable.
- [ ] **M3.3 Retry:** Define source-prefix resume, deduplication limits and uncertain outcomes after timeout; retain generation/session fencing from 0.2.x. Do not assume exactly-once delivery or persist a cursor ahead of applied state.
- [ ] **M3.4 Execution:** Reuse the common graph mutation path across single, legacy batch and feeder execution while preserving their distinct validation/failure policies.
- [ ] **M3.5 Tests:** Failure at every index, duplicate/no-op events, concurrent queries, disconnect/restart and source rebuild; preserve existing batch isolation and error-continuation tests.
- [ ] **M3.6 Measurement:** Measure batching benefits and costs in HTTP overhead, throughput, fairness, queue depth and query tails under equivalent semantics.
- [ ] **Acceptance M3:** Clients can identify which operations were applied and how to resume/reconcile; no breaking batch change or durability claim is introduced.

### M4 — Operational lifecycle, configuration and deployability (P1)

- [ ] **M4.1 Liveness:** Retain independent process liveness through loading, catch-up and source failure.
- [ ] **M4.2 Readiness:** Operationalize 0.2.5 with documented degraded/freshness policies and diagnostics; do not reimplement basic bootstrap.
- [ ] **M4.3 Shutdown:** Drain imports and updates with existing worker-owned accounting and deadlines; report uncertain outcomes and feeder restart requirements, not an invented disk-flush guarantee.
- [ ] **M4.4 Configuration:** Validate mode, networking, request/resource limits, timeouts, feeder ownership and freshness settings. Explicitly decide any reloadable subset; startup configuration is the baseline, not arbitrary per-request configuration.
- [ ] **M4.5 Deployment:** Supply an external-feeder/container example, resource and trust-boundary guidance, source retention requirements, restart/rebuild runbook and health transition tests. Storage-volume requirements apply only if optional persistence later ships.
- [ ] **Acceptance M4:** Operators can distinguish running, synchronized, lagging, rebuilding and draining states and recover through the authoritative source.

### M6 — Integration-aware observability (P1)

- [ ] **M6.1 Vocabulary:** Requests/status, graph counts, pending jobs, active imports, applied source progress, bootstrap duration, duplicate/gap/conflict counters and feeder health. Report lag only when the source protocol supplies a meaningful comparison.
- [ ] **M6.2 Prometheus metrics:** Bounded-cardinality HTTP/execution/lock-wait/import/catch-up histograms and backlog gauges. Never use raw node IDs, request IDs, generations or source cursors as metric labels. Persistence metrics are conditional on v0.4.0 capabilities.
- [ ] **M6.3 Logs:** Structured operation/request correlation, generation/revision and sanitized synchronization diagnostics without graph payloads or credentials.
- [ ] **M6.4 Alerts:** Readiness flapping, gaps, feeder loss, slow bootstrap, queue saturation and repeated errors; local storage alerts only for an enabled persistence capability.
- [ ] **M6.5 Budgets:** Verify instrumentation overhead and bounded series counts under churn, rebuilds and repeated source sessions.
- [ ] **Acceptance M6:** Operators can explain synchronization and performance failures from bounded telemetry, without inferring freshness solely from process health.

### M5 — Measured contention, query concurrency and isolation (P1)

- [ ] **M5.1 Instrument:** Separate parse/serialize, queue, lock, graph work, candidate build and publication costs. Optional storage I/O is not a prerequisite.
- [ ] **M5.2 Workloads:** Mixed reads/writes, batch sizes, rebuild/import overlap and feeder catch-up; report tails, fairness and memory with reproducible workloads.
- [ ] **M5.3 Alternatives:** Keep the coarse mutex if adequate; otherwise evaluate shorter critical sections, read locks only where query implementations permit them, or single-writer execution. No speculative redesign.
- [ ] **M5.4 Invariants:** Preserve coherent graph/generation/revision/cursor observations, legacy batch isolation and import publication; no update to an obsolete generation.
- [ ] **M5.5 Regression gate:** Require demonstrated benefit without correctness or unacceptable tail-latency regressions. Record a justified no-change decision if the current model meets approved objectives.
- [ ] **Acceptance M5:** The chosen synchronization design is supported by evidence and the v0.2.0 integration contract remains intact.

---

## v0.4.0 — Optional Local Persistence & Fast Recovery

**Tentative placement:** After v0.3.0 is appropriate because stable source-position semantics, operational telemetry and measured rebuild costs are prerequisites for judging local persistence. A measured customer need can move a selected capability earlier only through an explicit scope decision; it must not block v0.2.0 or bypass synchronization correctness. Persistence remains OPTIONAL and disabled by default.

**Goal:** Reduce startup cost and dependency on expensive full source reconstruction, while the external store remains authoritative. Local state can be stale. This milestone does not freeze a storage engine, codec, fsync policy, crate boundary or cross-platform durability guarantee.

Redis's [RDB and AOF approaches](https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/) are architectural references for periodic full images and append-only replay, not promises of Redis format/protocol compatibility or identical durability. Knotrel must evaluate its own graph reconstruction, source retention, memory and synchronization costs.

| Candidate configuration | Potential benefit for Knotrel | Cost / decision to evaluate |
|---|---|---|
| **Snapshot-only** | Start from a canonical graph and cursor, then replay the external suffix | Snapshot capture/I/O and temporary resources; snapshot age versus external history retention; still rebuild the engine |
| **AOF-only** | Recover an ordered local history before resuming external consumption | Initial baseline must be represented or externally available; unbounded replay/log growth is unacceptable; rotation/rewrite and source-position integrity add complexity |
| **Snapshot plus AOF** | Load a bounded baseline plus local suffix, then catch up externally | Most complex publication, checkpoint, compaction and failure interactions; extra writes/disk budget; benefit must exceed snapshot plus external replay |

These are alternatives to evaluate, not a requirement to ship all three. Start evaluation with snapshot-only as the simplest candidate, without committing to implement it. An authoritative external event log may already provide all necessary replay: reject or defer AOF if its measured startup benefit does not justify duplicate I/O, retention and failure handling. A local AOF is not a backup of the authoritative database and cannot establish source freshness by itself.

### P1 — Product value and persistence contract (OPTIONAL capability gate)

- [ ] Compare full external rebuild, snapshot plus external replay, AOF-only and combined recovery on the same source history. Record source availability/retention, graph sizes, time-to-ready, RAM, disk, write overhead and operational burden.
- [ ] Select only justified configurations; explicitly allow a decision to defer AOF or all persistence. Confirm whether any standalone-mode persistence is in scope separately; it is not implied by source-managed fast recovery.
- [ ] Define enabled-mode acknowledgment and visibility: distinguish in-memory applied position from locally recoverable checkpoint and from authoritative source commit. Choose synchronization policy and OS/filesystem assumptions only with explicit approval; never imply every successful HTTP mutation is locally durable.
- [ ] **Acceptance:** Approved scope and measurable objectives, including source-log redundancy analysis, resource budgets and documented failure behavior. No unmeasured RPO/RTO guarantee.

### P2 — Canonical local checkpoint and RDB-style snapshot evaluation

- [ ] Evaluate complete engine-independent graph images: all vertices including isolates, canonical undirected edges, schema, engine/configuration, limits, graph identity, generation, local revision, source identity/epoch, applied position and integrity metadata. Do not persist private ETT/HDT trees as portable truth.
- [ ] Define deterministic serialization, bounded validation and rebuilding on BFS/ETT/HDT; distinguish a canonical graph export from an automatically managed local checkpoint.
- [ ] Evaluate snapshot frequency/size, capture consistency, query disruption, temporary memory/disk, schema migration, integrity checks, atomic file publication, file/directory synchronization and retaining a known-good generation.
- [ ] **Acceptance if selected:** Round-trip topology and future mutations across engines; source tuple captured with the graph; fault injection at publication boundaries; no partially written checkpoint accepted.

### P3 — AOF-style append-only log evaluation

- [ ] Establish why external replay alone is insufficient before implementing AOF. Define baseline availability for AOF-only and prerequisites for snapshot-plus-log replay.
- [ ] Evaluate ordered mutation records with source epoch/position, generation and revision, no-op progress, deduplication consistency, versioned framing/checksums, commit/ack boundaries and deterministic replay. Reuse the integration protocol rather than creating a second source ordering model.
- [ ] Define truncated-tail versus corruption behavior, log growth limits, rotation, rewrite/compaction, snapshot boundaries and storage failure/backpressure. Never remove the only history needed by a retained checkpoint.
- [ ] **Acceptance if selected:** Every promised locally recoverable prefix is coherent; replay produces equivalent graph and source progress; crashes, duplicate retries and interrupted rotation cannot silently advance the cursor or mix generations. Document platform assumptions and what remains uncertain after a lost acknowledgment.

### P4 — Source-aware recovery, retention and lifecycle

The recovery principle is mandatory for any selected local persistence strategy:

1. Restore local state only if available, valid and compatible.
2. Recover its graph generation, local revision and corresponding source identity/epoch/position; fence previous process sessions.
3. Ask the external feeder to resume consumption after that applied position.
4. Detect gaps, expired history, changed source epochs/projections or unavailable source history.
5. Rebuild from the authoritative source when safe incremental recovery is impossible. Remain unready if the source cannot provide a valid rebuild; never turn corruption into a silently ready empty graph.
6. Become ready only after the synchronization contract, including its completion barrier/freshness policy, is satisfied.

- [ ] Treat local persistence checkpoint, live source position and authoritative state as distinct. A valid local restore is not synchronization proof; do not publish a ready state simply because replay ended.
- [ ] Define store locking, restart identities, disabled-mode behavior, incompatible files, disk-full/permission errors, cleanup, retention, shutdown deadlines and diagnostics. Fallback/rebuild policy must be explicit and observable, not silently discard corruption.
- [ ] **Acceptance if selected:** Recovery and full external rebuild converge to equivalent source-derived state. Failed catch-up remains unready. Corrupt/missing local state produces an explicit rebuild or actionable failure, never false synchronization.

### P5 — Fault injection, migration and recovery-cost evidence

- [ ] Cover clean restart, process kill, partial writes, checksum errors, disk full, permission failure, canceled snapshot, schema mismatch, stale generations, expired external history and recovery on each supported engine/platform.
- [ ] Separate process-kill evidence from power-loss assumptions; test storage ordering with deterministic fault injection and qualify OS/filesystem support explicitly.
- [ ] Version fixtures and migration policy; reject unknown formats safely. Compare total time-to-ready against external rebuild, including graph reconstruction and source catch-up, not merely file load time.
- [ ] **Acceptance if selected:** Safety tests and independent protocol review pass; recovery benefits and write/resource costs are reproducible; documentation distinguishes guarantees, assumptions and measurements.

**Exit gate:** Ship only selected, validated optional modes with a source-reconciliation runbook. External authority and the supported no-persistence deployment remain intact. Deferral is a valid product decision, not grounds to relabel the v0.2.0 service incomplete.

---

## v1.0.0 — Production Readiness and Optional Expansion

### M8 — Security and access control (conditional earlier release gate)

- [ ] **M8.1** Document trusted clients, feeder identities, gateway boundaries and supported exposure models.
- [ ] **M8.2** Integrate standardized authentication where required; avoid custom cryptography.
- [ ] **M8.3** Authorize reads, direct mutations, source events, import/replace and bootstrap completion separately; include persistence administration only if shipped.
- [ ] **M8.4** Bound resources, rate controls and audit output; sanitize payloads and credentials.
- [ ] **M8.5** Test unauthorized replacement/progress claims, stale feeder ownership, malformed inputs and cross-graph access only if multi-graph exists.
- [ ] **Acceptance M8:** No supported untrusted exposure without matching authentication, authorization and abuse tests.

### M9 — Packaging, compatibility and operations

- [ ] **M9.1** Version REST/import/event schemas and source-position semantics; add persistence schema/migration policy only for shipped modes.
- [ ] **M9.2** Publish library, loading, feeder, retry and restart/rebuild examples; add local recovery examples conditionally.
- [ ] **M9.3** Cross-platform CI for claimed capabilities, including integration fault tests; storage qualification applies only to persistence-enabled platforms.
- [ ] **M9.4** Release validated binaries/crates and selected deployment artifacts with version and license metadata.
- [ ] **M9.5** Runbook: capacity, source retention, feeder ownership, rebuild, upgrade and rolling-update limits; optional local backup/repair never replaces authoritative backup policy.
- [ ] **Acceptance M9:** Another engineer can deploy, integrate, upgrade and operate the supported modes from the documentation.

### M7 — Multi-graph management (OPTIONAL, demand-gated)

- [ ] **M7.1** Decide namespaces/lifecycle only with demonstrated demand; one graph's identity must not be mistaken for implemented multi-graph support.
- [ ] **M7.2** Define per-graph resource limits and source/feeder isolation.
- [ ] **M7.3** Partition generations, revisions and source positions; partition local persistence only if enabled.
- [ ] **M7.4** Evaluate scheduling/fairness for independent imports and update streams.
- [ ] **M7.5** Test concurrent lifecycle, restart/rebuild, quotas and isolation.
- [ ] **Acceptance M7:** Independent synchronization and resource isolation; single-graph mode remains supported. Distributed storage and general-purpose graph database features remain excluded.

## Dependencies and sequencing

1. Preserve the released v0.1.0 contract and historical evidence; reconcile old checklist bookkeeping separately rather than claiming new validation here.
2. **First handoff: Feature 0.2.1 — External source-of-truth integration contract.** Produce the contract/ADR, API examples, state machine and deterministic acceptance-test specification; no import or persistence implementation in that handoff. Resolve D1–D6 before dependent coding.
3. Implement 0.2.2 using the identity/precondition design from 0.2.1; complete shared metadata in 0.2.3, ordered events in 0.2.4, bootstrap/readiness in 0.2.5 and feeder/evidence in 0.2.6. Keep incomplete source-managed functionality experimental/unready until the release gate passes.
4. v0.3.0 builds operational controls and measurements on that contract; it does not depend on a local WAL.
5. v0.4.0 first evaluates value using the source/rebuild contract and operational evidence. Implement only approved persistence strategies; full external rebuild remains the fallback.
6. Move security forward whenever exposure requires it. Multi-graph and concurrency redesign remain demand/evidence-gated.

**Planning migration:** The previous durability-first Feature 0.2.1 and M1.1–M1.6 are superseded as v0.2.0 requirements; their storage concerns are retained under P1–P5. Previous M2.1–M2.6 import concerns now map to 0.2.1–0.2.3, 0.2.5 and 0.2.6, with durability-before-ack replaced by coherent in-memory publication plus source synchronization. M3–M9 retain their purpose with integration-first dependencies. This remapping does not mark any former or future task complete.

## Release-aware acceptance matrix

| Area | v0.1.0 Preview | v0.2.0 Integration | v0.3.0 Operational | v0.4.0 Optional Persistence | v1.0.0 Production |
|---|---|---|---|---|---|
| Correctness | Existing graph/HTTP oracle tests | Atomic import, ordered source prefix, identity and gaps | Batch compatibility and mixed-load isolation | Selected-mode round trips and replay | Supported compatibility and long-running stability |
| Authority/storage | Volatile | External authority; no local persistence required | Same contract | Local checkpoints/logs optional; external authority unchanged | Guarantees match deployed mode |
| Restart | Empty standalone graph | Source-managed rebuild and catch-up | Operational rebuild/runbook | Restore then verify source synchronization or rebuild | Supported recovery/upgrade procedures |
| Readiness | Basic lifecycle | Bootstrap barrier, gaps and freshness policy | Diagnostics, alerts and drain | Local recovery alone never means ready | Documented deployment guarantees |
| Security | Trusted boundary | Protect feeder/import/progress control | Auth required for untrusted exposure | Protect storage/admin if enabled | Qualified threat model |
| Performance | No blanket claims | Full load/catch-up and end-to-end evidence | Measured update/query/queue objectives | Compare total time-to-ready and write costs | Reproducible capacity envelope |
| Compatibility | Released APIs | Standalone and Rust preserved | Legacy batches preserved | Opt-in formats/version policy | Supported migrations and deprecation |

## Cross-cutting test matrix

| Dimension | Required scenarios for applicable capabilities |
|---|---|
| Engines/topology | BFS, ETT, HDT; empty, isolates, trees, components, cycles, dense bridges, churn, full-width IDs |
| Operations | Duplicate/reversed edge, self-loop, unknown nodes, limits, no-ops, legacy batch continuation |
| Integration | Image/cursor coherence, gaps, reordering, exact/conflicting/expired duplicates, epoch change, source history expiration |
| Identity/consistency | Instance/generation fencing, coherent revision/cursor, concurrent replace/update/query, stale bootstrap barrier |
| Lifecycle | Timeout/caller cancellation, worker panic, drain, process kill, restart and external rebuild |
| Resources | Body/item limits, concurrent imports, candidate memory, queue saturation, bounded duplicate retention |
| Optional persistence | Snapshot/log interruption, corruption, disk full, missing files, schema migration, source reconciliation |
| Compatibility/evidence | Existing public API and benchmark fingerprints, versioned fixtures/seeds, unchanged historical raw results |

**Evidence rule:** Every claimed improvement requires before/after comparison on the same revisioned workload with raw artifacts, environment metadata, fixed stopping rules and uncertainty. Separate kernel performance from HTTP, feeder, loading, catch-up and optional storage costs. Do not infer freshness or durability from speed, and do not block correctness-first work on unresolved small-effect measurements.

## Definition of Done for each feature issue

- [ ] REQUIRED/OPTIONAL/DEFERRED scope and dependencies explicit; no future feature implicitly marked complete.
- [ ] Public behavior, authority, acknowledgment, version scopes and failure semantics documented, including compatibility.
- [ ] Core invariants and coherent graph/generation/revision/source-position observations verified where applicable.
- [ ] Deterministic unit/integration/failure tests cover the newly promised contract; reference/differential oracles used where relevant.
- [ ] Resource bounds, cancellation, restart/rebuild, source unavailability and observability considered; storage faults only for applicable persistence features.
- [ ] Relevant formatting, Clippy, tests and warning-denied Rustdoc pass for code changes; documentation-only changes receive scoped consistency/link/diff checks.
- [ ] English docs, examples and operational notes updated; Rust conventions and Elastic-2.0 preserved.
- [ ] Performance, synchronization and durability claims distinguish measured evidence from assumptions and guarantees.
- [ ] Source/evidence changes scoped; no historical benchmark artifacts rewritten; independent review completed before feature acceptance.

## Architectural decision log

The strategic direction (external authority, preserved volatile mode, integration-first v0.2.0 and optional future persistence) is approved. The following implementation decisions remain open; unchecked items are not completed features.

### Resolve in 0.2.1 before dependent implementation

Proposed resolutions documented in [ADR 0012](docs/architecture/0012-external-source-of-truth-integration-contract.md) and [docs/integration-contract.md](docs/integration-contract.md#12-resolution-of-roadmap-decisions-d1d6); items remain unchecked pending maintainer approval before dependent coding.

- [ ] **D1 — Identity and ownership:** One implicit graph with what stable identity, generation/incarnation tokens and feeder fencing? How are source identity, source epoch and projection version bound to a generation?
- [ ] **D2 — Modes and acknowledgment:** How is source-managed mode selected while preserving default standalone behavior? How are untracked direct writes rejected, and timeout/unknown outcomes reported? No v0.2.0 local durability implied.
- [ ] **D3 — Ordering and retry:** Which source-independent continuity protocol is required? Which duplicate verification window/content identity and stale-event response? How are source errors, transactions and noncontiguous cursors handled?
- [ ] **D4 — Capacity and freshness:** What graph/body/candidate-memory limits, source retention assumptions, bootstrap objectives and post-bootstrap lag/disconnection policy are approved? No numerical SLO or benchmark result is assumed.
- [ ] **D5 — Import and publication:** Create-only, conditional replace or both? Define generation/revision transitions, precondition token, read availability during rebuild and the S-to-B handoff.
- [ ] **D6 — Trust boundary:** Who may feed events, replace graphs and complete bootstrap? If clients are untrusted, bring M8 forward before exposure.

### Resolve when expanding operations or persistence

- [ ] **D7 — Batch evolution:** Preserve legacy continue-after-error behavior; define a separate feeder batch failure/retry model if needed. Is multi-operation event atomicity actually required?
- [ ] **D8 — Multi-graph demand:** Is there a demonstrated need beyond one implicit graph, or should this remain deferred indefinitely?
- [ ] **D9 — Persistence value:** Does snapshot-only suffice? Does AOF add measurable value over the authoritative event log? Select zero, one or more configurations explicitly; v0.4.0 timing remains tentative.
- [ ] **D10 — Storage guarantees:** For selected modes, choose supported OS/filesystems, sync/ack policy, local checkpoint semantics, loss window, disk budget and error behavior. Distinguish power-loss assumptions from process-kill evidence.
- [ ] **D11 — Recovery and migration:** Define compatible schemas, safe fallback, source-history expiration, restore identity/fencing and operator intervention. Local restore never bypasses source synchronization.
