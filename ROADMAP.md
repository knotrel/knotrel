# Knotrel Roadmap: Dynamic Connectivity as a Service

**Status:** Proposed, release-oriented execution plan  
**Scope:** `knotrel` (independent Rust library and HTTP server) and `knotrel-benchmarks` (correctness/performance evidence)  
**Product direction:** Specialized service for undirected dynamic graph connectivity, not a general-purpose graph database.  
**Baseline:** The ordered `ConnectivityGraph::try_from_edges` API and optional benchmark-harness integration have been implemented and reviewed; verify the current repository state before marking any checklist item complete.

> **Execution rule:** Work from top to bottom within a release. All **v0.1.0** tasks come first. A task is checked only when verified against the current repository, not because a feature was discussed previously. Version assignments are planning targets and can change after review.

## Release overview and scope

| Release | Goal | Required scope | Explicitly not required |
|---|---|---|---|
| **v0.1.0 — Public Preview** | An installable and testable Rust library plus an explicitly **volatile, experimental** HTTP service | Reduced service contract, default-safe networking, basic liveness/readiness and shutdown, documentation, CI, release artifacts | Persistence, recovery, HTTP bulk import, batch operations, multi-graph, concurrency optimization, Prometheus |
| **v0.2.0 — Durable Service** | Single-graph service with documented restart/crash guarantees and bounded HTTP import | Full durability contract; WAL, snapshots and recovery; atomic or contractually safe HTTP import | Multi-graph, advanced concurrency tuning |
| **v0.3.0 — Operational Service** | Efficient request-level updates and diagnosable operations | Ordered batch mutations, full lifecycle, metrics/tracing, measured concurrency work where justified | Multi-graph unless demanded |
| **v1.0.0 — Production Readiness** | Stable, supported deployment with verified operational/security properties | Upgrade/compatibility policy, fault injection, deployment security, release hardening, operational guidance | General-purpose graph DB and distributed storage |

**Exposure rule:** v0.1.0 is meant for localhost or a trusted, access-controlled environment only. If any release exposes the server to untrusted clients or networks, authentication/authorization and the corresponding abuse tests become *blocking requirements for that release*, even if listed later.

---

## Feature Progress Dashboard

Track high-level feature readiness across releases. Check off each feature when all its underlying sub-tasks, tests and acceptance criteria are satisfied:

### v0.1.0 — Public Preview
- [x] **Feature 0.1.1:** Inventory and freeze minimum release scope (P0)
- [x] **Feature 0.1.2:** Document v0.1.0 service contract (M0-min, P0)
- [x] **Feature 0.1.3:** Establish a safe deployment boundary (M8-min, P0)
- [x] **Feature 0.1.4:** Minimal health, readiness and lifecycle (M4-min, P0)
- [ ] **Feature 0.1.5:** Rust library adoption, API and examples (P0)
- [ ] **Feature 0.1.6:** CI, packaging and release candidate validation (M9-min, P0)

### v0.2.0 — Durable Single-Graph Service
- [ ] **Feature 0.2.1:** Complete durability-specific service contract (M0)
- [ ] **Feature M1.1:** Canonical durable graph representation
- [ ] **Feature M1.2:** Write-ahead mutation log (WAL) design and implementation
- [ ] **Feature M1.3:** Atomic snapshot publication
- [ ] **Feature M1.4:** Startup recovery and replay
- [ ] **Feature M1.5:** Snapshot lifecycle and compaction
- [ ] **Feature M1.6:** Recovery fault-injection and migration tests
- [ ] **Feature M2.1:** Bulk import API contract
- [ ] **Feature M2.2:** Bounded import request handling
- [ ] **Feature M2.3:** Build-then-publish isolated import
- [ ] **Feature M2.4:** Durability-integrated import path
- [ ] **Feature M2.5:** End-to-end import tests and fixtures
- [ ] **Feature M2.6:** Total import cost instrumentation

### v0.3.0 — Operational Service
- [ ] **Feature M3.1:** Batch request/response contract
- [ ] **Feature M3.2:** Ordered committed-prefix failure model
- [ ] **Feature M3.3:** Durable replay and retry handling
- [ ] **Feature M3.4:** Common mutation executor for single and batch ops
- [ ] **Feature M3.5:** Partial failure and concurrent query tests
- [ ] **Feature M3.6:** Practical batch throughput measurement
- [ ] **Feature M4.1:** Recovery-aware liveness probe
- [ ] **Feature M4.2:** Recovery-aware readiness probe
- [ ] **Feature M4.3:** Graceful termination and connection draining
- [ ] **Feature M4.4:** Validated runtime configuration
- [ ] **Feature M4.5:** Deployability, storage checks and containerization
- [ ] **Feature M6.1:** Metric vocabulary definition
- [ ] **Feature M6.2:** Prometheus metrics export
- [ ] **Feature M6.3:** Structured logging and trace propagation
- [ ] **Feature M6.4:** Actionable alerts and operational dashboards
- [ ] **Feature M6.5:** Overhead and cardinality budget verification
- [ ] **Feature M5.1:** Lock wait and hold time instrumentation
- [ ] **Feature M5.2:** Mixed-load contention and throughput tests
- [ ] **Feature M5.3:** Concurrency architecture evaluation
- [ ] **Feature M5.4:** Revision consistency preservation
- [ ] **Feature M5.5:** Load-testing and regression gates

### v1.0.0 — Production Readiness
- [ ] **Feature M8.1:** Deployment threat model documentation
- [ ] **Feature M8.2:** Standardized authentication integration (JWT/mTLS)
- [ ] **Feature M8.3:** Action and graph-level authorization
- [ ] **Feature M8.4:** Resource endpoint rate-limiting and audit events
- [ ] **Feature M8.5:** Security abuse and privilege boundary testing
- [ ] **Feature M9.1:** Public REST API and schema versioning policy
- [ ] **Feature M9.2:** Comprehensive service and client examples
- [ ] **Feature M9.3:** Automated cross-platform CI matrix
- [ ] **Feature M9.4:** Deployable artifact publication (binaries/containers)
- [ ] **Feature M9.5:** Operations runbook and disaster recovery guidance
- [ ] **Feature M7.1:** Multi-graph namespaces and lifecycle (optional)
- [ ] **Feature M7.2:** Per-graph resource quotas and isolation
- [ ] **Feature M7.3:** Partitioned storage and recovery streams
- [ ] **Feature M7.4:** Fair scheduling and starvation prevention
- [ ] **Feature M7.5:** Multi-graph lifecycle and compatibility tests

---

## Guiding principles

1. **Correctness before throughput.** Never weaken the existing graph, error, limit or dynamic-connectivity semantics to improve a benchmark.
2. **Separate the concerns.** Core algorithms, persistence, protocol handling and operations live behind explicit boundaries. Avoid embedding HTTP/storage concerns in `knotrel-core`.
3. **Durability must be defined, not implied.** An acknowledged mutation has a documented recovery guarantee; otherwise the endpoint is clearly marked volatile.
4. **Ordered mutations, explicit consistency.** Mutations have one defined serialization order. Concurrent reads must observe a coherent graph revision.
5. **Observable, reproducible, bounded.** Resource caps, reproducible failure tests and service-level metrics precede aggressive optimization.
6. **Backward compatibility.** Existing public APIs and request/response formats are preserved unless an explicit versioning and migration plan is accepted.
7. **No speculative performance claims.** Measure end-to-end throughput, p95/p99 latency, memory, startup and recovery on controlled workloads; distinguish measurement noise from effects.

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
stays open during the drain only to answer such requests). Limits: tests ran in debug
mode locally only, so release-mode smoke tests per target, Windows signal behavior and
CI on the exact revision remain unevidenced; there is no write timeout or slow-reader
protection, and a timed-out mutation has an unknown outcome.

### 0.1.5 — Rust library adoption and examples (P0)

- [ ] **Feature Complete: 0.1.5 — Rust library adoption and examples**
- [ ] **Document public library API:** graph construction (`try_from_edges`), `add_node`, `link`, `cut`, `connected`, supported backends, limits, errors and update behavior. Verify names/signatures against source.
- [ ] **Add runnable Rust examples:** minimal connected/disconnected graph, dynamic bridge cut/relink and bulk construction, with expected output. Ensure examples compile in CI.
- [ ] **Explain engine choice:** simple, evidence-grounded guidance on BFS/ETT/HDT trade-offs; link to benchmark evidence while clearly stating limits and avoiding unsupported blanket performance claims.
- [ ] **Audit public docs and API hygiene:** crate descriptions, license/README links, rustdoc examples, exposed types/traits, and whether package metadata is suitable for publication. Do not promise API stability before 1.0.
- [ ] **Cross-check standalone library use:** examples should not require the HTTP server, private benchmark tools or a development-only directory structure.
- [ ] **Acceptance:** A new Rust user can copy the README example into a fresh project and use core connectivity operations without understanding the internal algorithms.

### 0.1.6 — CI, packaging and release candidate (M9-min, P0)

- [ ] **Feature Complete: 0.1.6 — CI, packaging and release candidate**
- [ ] **Automate correctness gates:** `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and warning-denied Rustdoc for supported workspaces, plus any relevant Python collector tests where the benchmark repo participates.
- [ ] **Pin reproducible build inputs:** commit/revision identifiers, lockfile policy, compiler/toolchain information and supported build instructions. Avoid conflating the benchmark repo's independent version with the service release.
- [ ] **Build/test the release binary:** clean build, startup, health/readiness, an API mutation/query, graceful shutdown and restart-empty behavior on every claimed supported platform.
- [ ] **Review legal and publishing metadata:** license and third-party notices, package naming/availability, dependency licenses, release artifacts, GitHub README/repository links and any security contact policy.
- [ ] **Publish documentation:** quick start, Rust example, curl/API examples, config reference, HTTP error table, architecture overview and **known limitations** prominently describing volatile state.
- [ ] **Prepare release notes and compatibility policy:** summarize supported behavior, known gaps, preview status, breaking-change expectations before 1.0 and upgrade expectations (none for in-memory state).
- [ ] **Perform a final clean-tree / diff / artifact review** for source, datasets and personal paths. Decide explicitly what benchmark raw evidence should be published; do not silently commit thousands of experimental files.
- [ ] **Create release artifacts only after approval:** tag `v0.1.0`, publish binary/container and optionally crate(s) only if the build/publish audit passes. Tagging/pushing/publishing is a separate authorized action, not implicit in completing this checklist.
- [ ] **Acceptance / v0.1.0 release gate:** Contract tests, examples, security boundary, smoke tests, documentation and supported-platform CI are green. The service is clearly labeled **experimental and in-memory**; there is no unsubstantiated production-readiness claim.

---

## v0.2.0 — Durable single-graph service

**Order:** First expand the M0 contract for durability and revisions, then implement M1 recovery, then M2 HTTP import. Existing v0.1.0 routes remain supported unless an approved compatibility change is documented.

### 0.2.1 — Complete durability-specific service contract (remaining M0)

- [ ] **Feature Complete: 0.2.1 — Complete durability-specific service contract**
- [ ] Define revision/sequence numbers, mutation acknowledgment and crash consistency (M0.2–M0.3).
- [ ] Finalize error/limit semantics for storage failure, replay and imports (M0.4).
- [ ] Confirm representative correctness workloads and record deterministic provenance (M0.5).
- [ ] Establish provisional graph scale, RTO/RPO, disk/memory and latency objectives before making durability promises (M0.6).
- [ ] Record storage/FS assumptions and the supported security boundary in ADRs.

### M1 — Persistence, snapshots and recovery (P0)

**Goal:** Restore an equivalent graph after controlled shutdown, process crash and interrupted writes without relying on the caller to rebuild it.

- [ ] **Feature Complete: M1 — Persistence, snapshots and recovery**
- [ ] **M1.1 Define a canonical durable graph representation.** Persist vertices and undirected edges, engine selection/configuration, graph limits, schema version, revision and integrity metadata. Canonicalize undirected edge endpoints; retain isolated vertices. Do not serialize private AVL/ETT/HDT pointers as the portable source of truth.
  - [ ] **Acceptance M1.1:** Round-trip preserves connectivity, node/edge counts, disconnected components, isolated vertices and future mutation behavior on all supported engines.
- [ ] **M1.2 Design the write-ahead mutation log (WAL).** Choose record framing, checksum, sequence/revision, ordering, versioning and commit boundary. Define treatment of truncated tails, corruption, duplicate request IDs, log retention and backpressure. Explicitly decide whether WAL writes occur before applying a mutation or via another recoverable protocol.
  - [ ] **Acceptance M1.2:** Replay is deterministic; no partially acknowledged mutation is silently treated as committed.
- [ ] **M1.3 Implement atomic snapshot publication.** Write to a temporary file, validate and sync it, atomically publish where supported, and sync directory metadata as required by the supported OS/filesystem. Include checksum, schema and last included WAL revision. Retain a known-good generation until replacement succeeds.
  - [ ] **Acceptance M1.3:** Crash injection at every snapshot phase restores either the prior valid generation or the new complete generation, never a partial file.
- [ ] **M1.4 Implement startup recovery.** Load newest valid snapshot, validate limits/integrity, replay subsequent committed records in order, and reject or quarantine irrecoverable state with clear operator diagnostics. Avoid silently starting an empty graph after recovery failure.
  - [ ] **Acceptance M1.4:** Replay reproduces a reference graph after mixed link/cut sequences; corruption scenarios have deterministic results.
- [ ] **M1.5 Snapshot lifecycle and compaction.** Configure snapshot triggers by WAL size, mutation count and elapsed time. Bound disk growth and retain a safe number of generations; expose maintenance status.
  - [ ] **Acceptance M1.5:** Sustained workloads do not grow storage without bound; restart time is measured and meets the agreed RTO target.
- [ ] **M1.6 Recovery and migration tests.** Cover clean restart, process kill, power-loss-like truncated WAL, checksum mismatch, disk full, insufficient permissions, canceled snapshot, schema mismatch and replay across supported engine configurations.
  - [ ] **Acceptance M1.6:** Automated fault-injection tests pass on supported platforms and document untestable power-loss assumptions.
- [ ] **Exit gate M1:** Acknowledged operations meet the defined durability contract, and recovery of every supported graph topology is deterministic and observable.

### M2 — Bulk graph import over HTTP (P0)

**Goal:** Load initial topology efficiently and safely without one request per edge.

- [ ] **Feature Complete: M2 — Bulk graph import over HTTP**
- [ ] **M2.1 Publish an import API contract.** Decide create-versus-replace semantics, graph identity, accepted JSON format (or versioned streaming format), graph limits, explicit isolated vertices, duplicate/self-loop rules, implicit endpoint behavior and error precedence. Match the core `try_from_edges` contract where appropriate; distinguish service preconditions from core semantics.
  - [ ] **Acceptance M2.1:** API spec contains success/failure examples and rejects malformed/oversized payloads deterministically.
- [ ] **M2.2 Apply bounded request handling.** Enforce request byte limit, max vertices/edges, parse limits and cancellation. For v1, a bounded fully buffered request is acceptable; streaming is a later enhancement if scale justifies it.
  - [ ] **Acceptance M2.2:** Oversized or malformed imports do not cause uncontrolled memory growth or leave a partial published graph.
- [ ] **M2.3 Implement build-then-publish.** Construct a candidate graph away from the live graph via `try_from_edges`; validate it and publish only after the durability path is complete. Define whether reads continue against the old revision during construction, and specify replacement conflicts using expected revision/ETag or another explicit mechanism.
  - [ ] **Acceptance M2.3:** Failed imports leave the previous graph and revision unchanged; successful imports become visible atomically.
- [ ] **M2.4 Integrate import with durability.** Store a complete committed import or recovery-safe equivalent before acknowledgment; ensure recovery never exposes a half-imported graph. Define WAL reset/checkpoint interaction.
  - [ ] **Acceptance M2.4:** Crash tests at parse, build, persist and publish boundaries recover old or new complete state under the specified acknowledgment rules.
- [ ] **M2.5 Add end-to-end tests and import fixtures.** Empty graph, isolates, duplicates, reversed duplicates, self-loops, limits, disconnected/cyclic/dense graphs, retries and concurrent readers. Compare imported state and future operations against ordered incremental construction.
  - [ ] **Acceptance M2.5:** Black-box HTTP tests plus oracle comparisons pass, with versioned fixtures.
- [ ] **M2.6 Instrument total import cost.** Separately report request parsing, graph construction, durable publication and response time, with memory peak. Do not conflate this with old benchmark setup timings.
  - [ ] **Acceptance M2.6:** Import observability permits diagnosing slow paths without claiming speedup from bulk core construction.
- [ ] **Exit gate M2:** An operator/client can safely create or replace a graph via one bounded API call, with predictable behavior across crashes.

---

## v0.3.0 — Operational service

**Order:** Batch mutations, full lifecycle and observability first. Concurrency changes are contingent on measured contention, not a prerequisite for release if the existing design meets the agreed capacity target.

### M3 — Ordered batch mutation API (P1)

**Goal:** Apply many graph changes per request while retaining an explicit consistency and error contract.

- [ ] **Feature Complete: M3 — Ordered batch mutation API**
- [ ] **M3.1 Design batch request/response.** Ordered `link`/`cut` operations; operation index, status and resulting revision; maximum batch length/bytes; define duplicate/no-op treatment.
- [ ] **M3.2 Choose initial failure model.** Recommend **ordered, stop-on-first-error with committed-prefix semantics** for v1, not all-or-nothing. Document that successful prefix updates remain committed when a later operation fails. Evaluate per-operation results versus one batch result. Do not advertise atomic rollback.
- [ ] **M3.3 Define durable replay and retry.** WAL format must preserve batch order and acknowledgment boundaries, and define behavior if a client retries after timeout. Consider request idempotency keys with retention policy, but do not assume exactly-once network delivery.
- [ ] **M3.4 Implement a common mutation executor.** Share code between single-operation and batch endpoints to keep validation, limits and revision semantics consistent. Avoid a second graph mutation implementation.
- [ ] **M3.5 Test partial failures and concurrent queries.** Test failure at every operation index, ordering, repeated operations, disconnect/reconnect, restart during batch, and observable prefix behavior.
- [ ] **M3.6 Measure practical batch advantages.** Compare HTTP overhead and throughput at varying batch sizes only after correctness; characterize fairness and impact on query tail latency.
- [ ] **Exit gate M3:** Clients can submit ordered modifications safely, understand exactly what persisted, and resume after interruption.

### M4 — Health, readiness and operational lifecycle (P1)

**Goal:** Extend v0.1.0 health/shutdown with recovery-aware readiness, durability-safe draining and deployability; do not reimplement the already verified minimum.

- [ ] **Feature Complete: M4 — Health, readiness and operational lifecycle**
- [ ] **M4.1 Liveness endpoint.** Respond when the process/event loop is alive; do not couple liveness to storage recovery or graph size.
- [ ] **M4.2 Readiness endpoint.** Remain unready during recovery, schema incompatibility, fatal persistence problems or resource saturation that prevents serving the promised contract. Specify degraded vs unavailable conditions.
- [ ] **M4.3 Graceful termination.** Stop admission of new mutations, finish or safely interrupt in-flight operations, finalize durability boundary, and close resources within a configurable drain timeout.
- [ ] **M4.4 Runtime configuration.** Bind address, storage location, data retention, request limits, timeouts, logs and mode via validated config; fail fast on unsafe combinations.
- [ ] **M4.5 Deployability checks.** Document storage-volume expectations, permissions, backups/restores, resource limits and local container example. Test restart with persisted data and health transitions.
- [ ] **Exit gate M4:** Operators can tell when the service is ready, drain it safely and recover its state after restart.

### M6 — Observability (P1)

**Goal:** Make production behavior diagnosable with bounded overhead and cardinality.

- [ ] **Feature Complete: M6 — Observability**
- [ ] **M6.1 Define a metric vocabulary.** Requests by route/status, mutation/query counts, graph nodes/edges, queue depth, active imports, WAL bytes, snapshot age, recovery state and failures.
- [ ] **M6.2 Expose Prometheus metrics.** Histograms for HTTP, graph execution, lock wait, durable acknowledgment, import and recovery time; counters for errors and WAL operations; gauges for current graph size and backlog. Never use raw graph IDs, node IDs, edge IDs or request IDs as metric labels.
- [ ] **M6.3 Add structured logs and trace propagation.** Stable request ID, revision, operation type, duration and errors, with sensitive input redaction. Support trace correlation without logging entire graph payloads.
- [ ] **M6.4 Create actionable alerts and dashboards.** Recovery failure, readiness flapping, WAL growth, snapshot failures, disk exhaustion, p99 latency, mutation backlog and repeated 5xx errors.
- [ ] **M6.5 Set overhead and cardinality budgets.** Exercise metrics under high churn and large graphs; verify bounded time series and low instrumentation overhead.
- [ ] **Exit gate M6:** An operator can explain service health, failures, saturation and recovery behavior from telemetry.

### M5 — Contention, query concurrency and isolation (P1)

**Goal:** Increase useful concurrency only when evidence identifies lock/queueing overhead.

- [ ] **Feature Complete: M5 — Contention, query concurrency and isolation**
- [ ] **M5.1 Instrument lock wait/hold time.** Distinguish HTTP parse/serialize, queue wait, lock wait, graph operation, WAL/snapshot I/O and response time. Confirm actual current lock scope from source.
- [ ] **M5.2 Create mixed-load tests.** Vary reader/writer ratios, graph size and batch length; report throughput, p50/p95/p99, fairness, queue depth and memory. Use a dedicated test environment where possible.
- [ ] **M5.3 Evaluate alternatives.** Keep coarse mutex if sufficient; otherwise compare short critical sections, `RwLock` where reads are truly read-only, actor/single-writer execution, or immutable-version snapshots. Check whether ETT/HDT query paths mutate internal state before adopting read locks.
- [ ] **M5.4 Preserve consistency.** Every query must observe a documented revision and never a partially applied batch/import. Ensure mutation ordering, cancellation and shutdown cannot violate the WAL/state contract.
- [ ] **M5.5 Load-test new design and regressions.** Use mixed and update-heavy workloads plus long operations; require meaningful improvements without correctness or tail-latency regressions.
- [ ] **Exit gate M5:** Either the existing synchronization is supported by evidence, or an alternative has demonstrated better behavior without violating consistency.

---

## v1.0.0 — Production readiness and optional expansion

**Order:** Close supported security and operational guarantees, harden packaging/compatibility, then decide whether multi-graph is justified by real consumers. Security moves into earlier releases whenever deployment trust assumptions require it.

### M8 — Security and access control (conditional release gate)

**Goal:** Prevent unauthorized reads, writes, replacement and destructive operations.

- [ ] **Feature Complete: M8 — Security and access control**
- [ ] **M8.1 Document deployment threat model.** Trusted internal network, reverse-proxy boundary, internet exposure, operators and client identities; decide supported security assumptions.
- [ ] **M8.2 Integrate authentication at a clear boundary.** Prefer standardized gateway/OIDC/JWT or mTLS where appropriate; avoid custom token cryptography. Define secure local-development mode.
- [ ] **M8.3 Authorize by action and graph.** Distinguish read, mutate, import/replace, delete, snapshot/restore and administration. Deny by default where access control is enabled.
- [ ] **M8.4 Protect resource endpoints.** Request/body caps, timeouts, rate controls, cancellation, audit events and sanitized errors; never expose untrusted payloads or secrets in logs/metrics.
- [ ] **M8.5 Test abuse cases.** Unauthenticated/unauthorized access, cross-graph isolation, malformed tokens, replay/retry behavior, large requests, and privilege boundaries.
- [ ] **Exit gate M8:** No endpoint is deployed outside the explicitly trusted boundary without matching authentication and authorization controls.

### M9 — Packaging, compatibility and release (P2)

- [ ] **Feature Complete: M9 — Packaging, compatibility and release**
- [ ] **M9.1 Version public REST API and persistence schemas.** Compatibility policy, deprecation rules and migration plan for WAL/snapshots.
- [ ] **M9.2 Document service examples.** HTTP import, connectivity query, single update, batch update, restart/recovery and error handling. Keep standalone Rust examples for the library.
- [ ] **M9.3 Automate cross-platform CI.** Correctness, fault-injection, schema migration, container smoke test and security linting; define supported OS/filesystems.
- [ ] **M9.4 Publish deployable artifacts.** Reproducible image/binary builds, version metadata, SBOM/license review where appropriate and release notes.
- [ ] **M9.5 Write an operations runbook.** Capacity planning, volume backup/restore, WAL/snapshot repair policy, observability and rolling-update limitations.
- [ ] **Exit gate M9:** Another engineer can deploy, upgrade, restart and operate Knotrel from written instructions.

---

### M7 — Multi-graph management (P2)

**Goal:** Host independent graphs in one process if actual users need it.

- [ ] **Feature Complete: M7 — Multi-graph management**
- [ ] **M7.1 Define graph namespaces/IDs and lifecycle.** Create, list, inspect, replace and delete; decide what deletion means for durable storage and recovery.
- [ ] **M7.2 Per-graph limits and isolation.** Apply memory, node/edge, disk and request quotas; ensure one graph cannot starve the rest.
- [ ] **M7.3 Partition storage and revisions.** Unique WAL/snapshot streams or an explicitly namespaced format; independent recovery and corruption handling.
- [ ] **M7.4 Scheduling/fairness.** Ensure large imports or writes in one graph do not block unrelated graphs; benchmark starvation and contention.
- [ ] **M7.5 Lifecycle and compatibility tests.** Concurrent create/delete, restart, ID collisions, quota enforcement and per-graph metrics without unbounded labels.
- [ ] **Exit gate M7:** Graphs are independently recoverable, observable and resource-bounded. Keep single-graph mode as a supported low-complexity option.

---

## Release-aware acceptance matrix

| Area | v0.1.0 Public Preview | v0.2.0 Durable | v0.3.0 Operational | v1.0.0 Production |
|---|---|---|---|---|
| Graph correctness | Deterministic oracle/differential and HTTP contract tests | Recovery round trips and fault injection | Batch prefix/retry and mixed-load correctness | Compatibility and long-running stability |
| Storage | Explicitly volatile | Snapshot/WAL recovery with documented ack guarantees | Maintenance, compaction and recovery telemetry | Upgrade and migration guarantees |
| Lifecycle | Basic liveness, readiness, graceful shutdown | Recovery-aware readiness | Drain/maintenance and operational alerts | Deploy/upgrade runbook |
| Security | Loopback/trusted boundary only | Reassess exposure on each new endpoint | Auth if any untrusted exposure | Supported authn/authz for claimed deployment models |
| Performance | No unsupported claims; normal correctness gates | Bounded load/import measurement | Concurrency/queue and p95/p99 targets | Published capacity envelope with reproducible evidence |
| Packaging | Preview docs, release binaries/crates as validated | Versioned storage formats | Operational deployment packaging | Backward compatibility and supported upgrades |

## Cross-cutting test matrix (select applicable dimensions by release)

| Dimension | Minimum scenarios |
|---|---|
| Engines | BFS, ETT, HDT on the public facade; document exclusions explicitly |
| Topology | Empty, isolates, path/tree, multiple components, cycles, dense bridge, high churn |
| Mutations | Duplicate/reversed edge, self-loop, missing endpoints, repeated link/cut, edge/node limits |
| Lifecycle | Clean restart, kill during mutation/import/snapshot, replay, truncated or corrupted storage |
| Consistency | Concurrent query vs write, revision monotonicity, failed import unchanged state, ordered batch prefix |
| Resources | Request size, graph limits, disk full, memory pressure, timeout, cancellation, graceful shutdown |
| Compatibility | Old API responses, old snapshot/WAL versions when supported, existing benchmark fingerprints |

**Evidence rule:** Every claimed performance improvement requires a measured before/after comparison on the same revisioned workload, with raw artifacts, environment metadata, fixed stopping rules and an explanation of uncertainty. Failure to establish small-effect precision does not block correctness-first product work.

## Definition of Done for each feature issue

- [ ] Public behavior and failure semantics documented, including backward compatibility.
- [ ] Core invariants preserved, with reference/differential tests where relevant.
- [ ] Unit, integration and failure/recovery tests added for newly promised behavior.
- [ ] Resource limits, cancellation, disk/memory failure behavior and observability considered.
- [ ] Formatting, Clippy, tests and warning-denied Rustdoc green in affected workspaces.
- [ ] User-facing API docs, examples and operational notes updated.
- [ ] No unsupported speedup, consistency or durability claims.
- [ ] Source/evidence changes are scoped; no historical benchmark artifacts rewritten.

## Decision log: resolve before implementing M1

- [ ] **D1:** Is v1 a single named graph, or one implicit graph per process?
- [ ] **D2:** Is the v1 durability default synchronous acknowledgment after durable write, or explicitly configurable volatile mode?
- [ ] **D3:** What are the supported OS/filesystem crash-consistency assumptions and durability primitives?
- [ ] **D4:** What are the acceptable RPO/RTO, disk budget and maximum initial graph size?
- [ ] **D5:** Will import replace a live graph, create only, or support both with revision-based conflict detection?
- [ ] **D6:** Is service access restricted to a trusted internal network? If not, M8 is a release prerequisite.
- [ ] **D7:** Do batch failures persist a committed prefix, or is atomicity truly a product requirement? (Recommended v1: committed prefix.)
- [ ] **D8:** Is multi-graph a real deployment requirement, or can it be deferred indefinitely?
