# Threat model

**Status:** proposed for the v1.0.0-rc security gate (readiness criterion 7,
[v1.0 readiness](releases/v1.0-readiness.md)); written 2026-11-22 against
`main` at `580af3f` (v0.9.0 plus documentation). Not accepted until the
maintainer accepts it. It describes the supported exposure only and promises
nothing beyond the evidence cited.

This document states what Knotrel protects, from whom, where the trust
boundaries are, which threats apply at each boundary, and, for every
mitigation, the test or CI check that validates it. Gaps are listed with their
status and a proposed fix; none is closed by this document alone.

## 1. Scope and supported exposure

Knotrel has **no authentication, authorization or TLS of its own**
([trust model](http-api.md#trust-model)). The supported exposures for 1.0 are:

| Exposure | Supported | Who is trusted |
| --- | --- | --- |
| **E1 Loopback**: server on `127.0.0.1`/`[::1]` (the default), clients on the same host | Yes | Every process and user on the host that can open a loopback connection |
| **E2 Trusted network**: non-loopback bind with `KNOTREL_ALLOW_NON_LOOPBACK=true`, reachable only from a network whose every host is trusted (for example a private container network) | Yes | Every host on that network, with full read/write access |
| **E3 Behind a gateway**: server on loopback (or on a network only the gateway reaches); a gateway terminates TLS and authenticates and authorizes every request ([deployment guide](deployment.md#gateway-with-tls-and-authentication), [`examples/gateway/`](../examples/gateway/compose.yaml)) | Yes | The gateway and the identities it admits, per route class |
| **E4 Untrusted exposure**: any client that is not trusted can reach the Knotrel listener directly (public internet, shared network) | **No** | — |

Native authentication is not part of 1.0; E4 stays unsupported. Opting in to a
non-loopback bind is an operator acknowledgement, not a hardening step.

Out of scope: the security of the authoritative source itself, of the
operator's identity provider, of the container runtime and host, and of
third-party feeders other than the reference PostgreSQL feeder. Side channels
and physical access are not considered.

## 2. Assets

| ID | Asset | Why it matters |
| --- | --- | --- |
| A1 | **Graph integrity**: nodes, edges, lifecycle flags, generation and revision | Wrong connectivity answers propagate to every consumer |
| A2 | **Synchronization integrity**: feeder session, epoch, lease, delivery sequence, source frontier, deduplication window | A takeover or a desynchronization makes the graph diverge silently from the source |
| A3 | **Availability**: the graph lock, admission lanes, connections, memory and CPU | Knotrel is an accelerator on a query path; stalls and crashes are the main operational risk |
| A4 | **Graph confidentiality**: topology and node IDs, readable through queries, inspection, stats and metrics | Topology can be sensitive (for example network or grid layout) |
| A5 | **Credentials around Knotrel**: gateway credentials, TLS keys, the PostgreSQL feeder role's password | Knotrel holds none itself; the deployment around it does |
| A6 | **Release artifacts**: binaries, crates, console archive and image | A tampered artifact compromises every deployment |

Knotrel stores nothing on disk ([state and lifetime](http-api.md#state-and-lifetime)):
a restart loses A1/A2 by design and is recovered by a rebuild from the source.

## 3. Actors

| Actor | Trust | Normal capabilities |
| --- | --- | --- |
| **Operator** (human or automation) | Trusted inside the boundary | Configures and starts the server; in standalone mode mutates the graph; reads everything |
| **Query client** (application) | Trusted inside the boundary | Queries; in standalone mode also mutates |
| **Feeder** (the reference PostgreSQL feeder or another) | Trusted inside the boundary | Owns the feeder session; imports and streams events in source-managed mode |
| **Console user** | Trusted, authenticated by the gateway (E3) or the single local user (E1) | Reads; in standalone mode mutates through the console's allowlisted routes |
| **Gateway** | Trusted component | Terminates TLS, authenticates, authorizes per route class, forwards |
| **PostgreSQL** (reference source) | Trusted authority for the graph content | Supplies snapshots and the logical replication stream |
| **Misbehaving insider**: a buggy or compromised client inside the boundary | Inside the boundary, not malicious by assumption, but considered for availability and for blast radius | Anything the boundary grants |
| **Web attacker**: a page the operator's browser visits | Untrusted | Can make the browser send requests to loopback and private addresses |
| **Network attacker** outside the boundary | Untrusted | Can reach the gateway's listener (E3) but not Knotrel directly |
| **Supply-chain attacker** | Untrusted | Targets dependencies, CI or release artifacts |

## 4. Trust boundaries and data flows

```text
                         untrusted                 |            trusted boundary
                                                   |
 browser (console user) --TLS--> [ gateway ] ------+--> console (127.0.0.1) --> Knotrel (127.0.0.1)
 query client --------------TLS--> [ gateway ] ----+--------------------------> Knotrel
 feeder -------------------TLS--> [ gateway ] -----+--------------------------> Knotrel /v1/feeder/*
                                                   |
                          PostgreSQL <--replication-- reference feeder  (same trusted network, no TLS)
```

| ID | Boundary | Crossing | Controls |
| --- | --- | --- | --- |
| B1 | **Network → Knotrel listener** | Every HTTP request | Loopback default and explicit opt-in for other binds; request bounds; no authentication |
| B2 | **Gateway** (E3) | Client → gateway → Knotrel or console | TLS 1.2/1.3, HTTP Basic (example) or the organization's identity provider, route classes per credential |
| B3 | **Console container** | Browser → console → Knotrel | Fixed upstream, route allowlist, `Host`/`Origin`/`Sec-Fetch-Site` checks, CSP, no credential forwarding |
| B4 | **Feeder routes** | Feeder → `/v1/feeder/*` | Mode check, instance ID, session/epoch fencing, lease; separate gateway credential (E3) |
| B5 | **PostgreSQL → reference feeder** | Snapshot and logical replication | Least-privilege role, password from a file, slot retention bounds |
| B6 | **Build and release** | Source → CI → artifacts | Locked dependencies, advisory checks, pinned actions, checksums, signed tags |

## 5. Threats and mitigations per boundary

Each mitigation names its validation. "Doc only" means the mitigation is a
documented operator duty with no automated check. Gaps are cross-referenced
to section 6 (`G#`) and section 7 (`N#`).

### B1 — Network to the Knotrel listener

| ID | Threat (STRIDE) | Mitigation | Validation |
| --- | --- | --- | --- |
| T1.1 | Accidental exposure on a non-loopback address (S, T, I) | Default bind `127.0.0.1:8080`; a non-loopback `KNOTREL_ADDR` fails startup unless `KNOTREL_ALLOW_NON_LOOPBACK=true`; a warning is logged when set | `security.rs::non_loopback_listener_requires_a_separate_constructor`; `startup.rs::invalid_environment_exits_promptly_with_the_setting_name` (includes `KNOTREL_ADDR=0.0.0.0:0` without the opt-in) |
| T1.2 | Any reachable client reads and mutates the graph (S, T, I, E) | **None in Knotrel by design**: restricted to E1–E3; E3 adds authentication at the gateway | Gateway smoke test (B2); E1/E2 are a deployment duty (doc only, [trust model](http-api.md#trust-model)) |
| T1.3 | Oversized bodies exhaust memory before parsing (D) | 1 MiB body limit on every route except import, enforced before parsing; import bounded by `KNOTREL_MAX_IMPORT_BODY_BYTES` (64 MiB) and admitted (one permit) before its body is consumed | `security.rs::request_size_bounds_are_enforced_before_parsing_on_every_engine`; `http.rs::oversized_bodies_are_rejected_before_mutation`; `import.rs::test_1_admission_before_body_consumption_and_ceiling_at_one`; import-limit case in the `api.rs` unit tests |
| T1.4 | Graph growth without bound (D) | Default `KNOTREL_MAX_NODES=100000`, `KNOTREL_MAX_EDGES=500000` on every engine; `unlimited` is an explicit opt-out; rejections do not mutate | `security.rs::configured_servers_default_to_finite_graph_limits_on_every_engine`, `node_limit_boundary_is_exact_and_rejections_do_not_mutate`, `edge_limit_boundary_is_exact_and_unlimited_is_an_explicit_opt_out`, `batches_cannot_exceed_limits_and_report_per_operation_rejections`; `http.rs::graph_limits_apply_to_batches_and_concurrent_requests` |
| T1.5 | Slow or idle connections hold resources (slowloris) (D) | Header read / keep-alive timeout (5 s), request timeout (30 s) covering body receipt, connection limit (1024) | `lifecycle.rs::silent_and_partial_header_connections_time_out`, `idle_keep_alive_connection_times_out`, `request_timeout_bounds_a_stalled_body`, `connection_limit_rejects_extra_connections_and_recovers` |
| T1.6 | Work flooding the graph worker (D) | Admission lanes: shared `KNOTREL_MAX_PENDING_JOBS` (32), feeder data/control lanes and one import permit; excess gets 503 before graph work | `http.rs::invalid_admission_limits_are_rejected_before_router_construction`; feeder-lane tests from 0.9.3 ([feeder admission](integrations/feeder-admission-design.md)); **not a CPU or lock-time bound** → G2 |
| T1.7 | Lock-holding requests delay every mutation, feeder event and import (D) | Read-write lock (queries share it); batch ceiling 1024; inspection pages bounded by a scan budget | Inspection: `inspection.rs::invalid_parameters_are_rejected_before_admission` and [inspection costs](console-inspection-costs.md); per-request worst-case lock time **not measured** → G2 |
| T1.8 | Import memory amplification (D) | Streaming import parse; one import at a time; deployment sizing for `old graph + import body + new graph` | Measured in the v0.6.0 qualification ([topology lifecycle, resource model](topology-lifecycle.md)); `import.rs::test_16_replaced_graph_released_after_lock_inside_worker` → G1 |
| T1.9 | Information disclosure through errors and logs (I) | Error messages never quote input, parser positions or internal paths; logs never contain bodies, node IDs or parser messages | `security.rs::client_errors_are_stable_and_do_not_leak_internals_or_payloads`, `schema_violations_never_echo_client_supplied_names_or_values`; `logs.rs::logs_correlate_requests_and_never_expose_payloads` |
| T1.10 | Metric label cardinality explosion (D) | Only bounded labels; route templates, not raw paths; metrics off by default | `metrics.rs::exposition_is_valid_and_uses_only_bounded_labels`, `every_registered_route_is_labelled_with_its_template`, `series_count_is_constant_under_sustained_churn`, `metrics_route_is_absent_when_disabled` |
| T1.11 | Cross-site request forgery from a web page against a loopback server (T) | Mutating routes require a JSON media type (a cross-site form cannot send one without a CORS preflight, which Knotrel never grants); feeder routes also require the `X-Knotrel-Instance-Id` header | `http.rs::content_type_requires_a_json_media_type_before_mutation`; the absence of CORS headers is not asserted by a test → N1 |
| T1.12 | DNS rebinding: a web page rebinds its name to `127.0.0.1` and becomes same-origin with the server (S, T, I) | **None in the server**: Knotrel does not check `Host`. The console container does (B3) | Open → N1 |
| T1.13 | Memory-unsafe code (T, E) | `unsafe_code = "forbid"` in the workspace lints | Workspace lints (Clippy in CI) |
| T1.14 | Uncertain outcome after a timeout is mistaken for "not applied" (T) | Documented: a timed-out mutation's outcome is unknown; mutations are idempotent; the console never retries mutations | `lifecycle.rs::http_request_timeout_leaves_worker_active_and_shutdown_waits_for_it`; console `test/ui/workbench.test.tsx` ("marks a timed-out mutating batch as uncertain and never retries") |
| T1.15 | **Any reachable worker panic becomes permanent unavailability** (D). The graph worker sets `graph.failed` before running a job and clears it only when the job returns; after a panic every later request gets 503 `unavailable` and `/ready` reports `unavailable` until the process is restarted and rebuilt from the source ([probes](http-api.md#probes)). This fail-stop is deliberate (integrity over availability: a graph left half-mutated by a panic is never served), but it turns any panic reachable from request input into a denial of service that outlasts the request, for queries, mutations and feeder events alike | Fail-stop is tested; the absence of input-reachable panics is **not**: no audit of `unwrap`/`expect`/`unreachable!` sites on request paths and no fuzz or property test of arbitrary feeder envelopes against each synchronization state. Defect D1 (T4.6) is a concrete instance | Fail-stop: `lifecycle.rs::worker_panic_after_caller_cancellation_marks_ready_unavailable`, `bootstrap.rs::test_17_caller_cancellation_and_worker_panic_preserve_lifecycle`, `service.rs` unit test `panicked_writer_rejects_queued_and_future_work`; absence of reachable panics: open → N7 |

### B2 — Gateway (E3)

Validated by [`examples/gateway/smoke-test.sh`](../examples/gateway/smoke-test.sh),
run by the `gateway` CI workflow (`.github/workflows/gateway.yml`) on every
change to the server, the container images or the example.

| ID | Threat | Mitigation | Validation (smoke-test check) |
| --- | --- | --- | --- |
| T2.1 | Bypassing the gateway to reach Knotrel directly (S, E) | Knotrel binds `127.0.0.1:9080` (`KNOTREL_ALLOW_NON_LOOPBACK=false`) in a namespace shared only with the console and the gateway; only 8443 is published | "Knotrel not reachable from the host"; "Knotrel not reachable from another container on the network"; "console not reachable from another container on the network" |
| T2.2 | Eavesdropping or tampering on the wire (I, T) | TLS 1.2/1.3 only; plain HTTP refused; HSTS | "TLS 1.2 accepted", "TLS 1.3 accepted", "gateway allows only TLS 1.2 and 1.3", "plain HTTP on the TLS port refused" |
| T2.3 | Unauthenticated access (S) | HTTP Basic on every location, probes included | "no credentials refused", "wrong password refused", "console UI refused without credentials" |
| T2.4 | A query client or console user takes over the feeder session, or a feeder mutates through the operator API (E) | Separate credential files: operators for `/v1/*` except feeder routes, `/metrics` and the console; feeders for `/v1/feeder/*`; probes for either | "operator cannot use feeder routes", "feeder cannot use the operator API", "feeder credentials reach the feeder routes", "feeder can read readiness" |
| T2.5 | Key compromise via the gateway container (I) | The local CA key is deleted after signing and never mounted; files mounted read-only; containers read-only, all capabilities dropped, `no-new-privileges` | "CA key not present in the gateway"; Compose settings reviewed, not tested |
| T2.6 | Credential leakage in process listings or logs (I) | Smoke test passes credentials to curl in files; Knotrel never logs headers | Smoke-test design; `logs.rs` (no payloads) |
| T2.7 | Request flooding through an authenticated or unauthenticated client (D) | Gateway body limits (1 MiB, 64 MiB for feeder routes) and read timeout; **no rate or connection limiting in the example** | Not tested → N3 |
| T2.8 | Weak authentication in production (S) | The example states that HTTP Basic with static files is an example, not a recommendation | Doc only ([deployment guide](deployment.md#gateway-with-tls-and-authentication)) |

### B3 — Console container

Validated by [`console/scripts/smoke-container.sh`](../console/scripts/smoke-container.sh),
run in the `console` workflow and before each console release
(`.github/workflows/console.yml`, `console-release.yml`), and by the console
unit and browser tests (`npm run check`, `npm run test:e2e`). The container's
forwarding rules are a security surface; changing them follows
[AGENTS.md](../AGENTS.md) (update `docs/console.md`, `console/deploy/nginx-gateway.conf`
and the smoke script together).

| ID | Threat | Mitigation | Validation |
| --- | --- | --- | --- |
| T3.1 | Console used as an open proxy to arbitrary upstreams (S, E) | Upstream fixed by `KNOTREL_UPSTREAM`; credentials in it are refused; startup fails without it | smoke-container: "credentials in KNOTREL_UPSTREAM accepted" / "missing KNOTREL_UPSTREAM accepted" must fail |
| T3.2 | Feeder takeover through the console (E) | Route allowlist; feeder routes never forwarded | smoke-container: "feeder route hidden" (404), "unknown node subpath rejected", "non-numeric node path rejected", "method check" |
| T3.3 | DNS rebinding (S) | Only accepted `Host` names are served (421 otherwise) | smoke-container: "Host check" |
| T3.4 | Cross-site request forgery (T) | `Origin` must match; `Sec-Fetch-Site` must be `same-origin` or absent | smoke-container: "Origin check", "Sec-Fetch-Site check" |
| T3.5 | Script injection through server-provided text (T, E) | Server text is rendered as text; ESLint forbids `dangerouslySetInnerHTML` and `innerHTML`; strict CSP (`script-src 'self'`, `frame-ancestors 'none'`) | `npm run lint` (`console/eslint.config.js`); CSP headers in `console/container/knotrel/security-headers.conf` are **not asserted** by a test → N4 |
| T3.6 | Precision loss on IDs, revisions or counts leads to wrong mutations (T) | Lossless JSON parsing; IDs never converted to `number` | Console unit tests and smoke-container "API forwarding with exact IDs" (a full batch with ID `18446744073709551615`) |
| T3.7 | Credential forwarding to Knotrel (I) | `Cookie` and `Authorization` stripped; `Set-Cookie` hidden; no forwarding headers | Console container configuration; not asserted by a test (low risk: Knotrel ignores and never logs headers) |
| T3.8 | Oversized requests through the console (D) | 64 KiB body limit (1 MiB for batches) | smoke-container: "body limit", "full batch accepted" |
| T3.9 | Static archive deployed with an unsafe hand-written gateway (S, E) | `console/deploy/nginx-gateway.conf` mirrors the container's allowlist, CSP and stripping, and states that TLS and access control are required | Packaged and checksummed by `package-static.mjs --verify`; **not syntax- or behavior-tested** → N4 |

### B4 — Feeder routes

| ID | Threat | Mitigation | Validation |
| --- | --- | --- | --- |
| T4.1 | Stale feeder overwrites newer state after a takeover or lease expiry (T) | Session/epoch fencing, lease on the monotonic clock, instance ID on every request, `If-Match` replacement preconditions | `import.rs::test_3_exact_ownership_fencing_both_directions`, `test_9_session_takeover_and_lease_expiry_before_publication`, `test_14_strict_replacement_preconditions_negative_cases`; `events.rs::test_13_session_takeover_and_lease_expiry_before_execution`, `test_24_whole_request_fencing_rejections_apply_nothing`; `bootstrap.rs::test_14_session_retries_and_takeovers_cannot_bypass_synchronization_recovery`, `test_20_delayed_barrier_fenced_across_lease_expiry`; `e2e_rebuild.rs::test_15_lost_ack_combined_with_competing_ownership_takeover`; PostgreSQL integration tests (`postgres-feeder` workflow, PostgreSQL 15–19) |
| T4.2 | **Hostile client inside the boundary takes over the feeder session** (S, E) | **None in Knotrel by design**: fencing protects against stale feeders, not against a hostile client. Any client that can reach `/v1/feeder/session` (the instance ID is public through `GET /v1/info`) can take over. Mitigated only by the boundary: in E3 by a separate feeder credential at the gateway | Gateway smoke test T2.4; console smoke test T3.2 → G3 |
| T4.3 | Direct mutations bypass the source in source-managed mode (T) | Mutations refused with 409 `direct_mutation_disabled`; reads and inspection allowed | `batch.rs::test_05_source_managed_mode_rejects_mutating_batches_allows_query_only`; `inspection.rs::source_managed_reads_are_available_without_mutation` |
| T4.4 | Replayed or reordered events corrupt state (T) | Delivery sequence with predecessor check and bounded exact deduplication | `events.rs` and `bootstrap.rs` sequence tests; [integration contract §5–§7](integration-contract.md) |
| T4.5 | Feeder routes used in standalone mode (E) | 409 mode conflict | Gateway smoke test ("feeder credentials reach the feeder routes", 409 from Knotrel) |
| T4.6 | **Defect D1, found by the R3 soak: a feeder event with `delivery_sequence: 0` right after an import panics the graph worker** (D). After an import and before the first event of the generation is applied (`applied_delivery_sequence == 0`), `check_sequence` returns `DedupLookup::Future` for sequence 0 and the duplicate branch reaches `DedupLookup::Future => unreachable!()` in `crates/knotrel-server/src/service.rs`. By T1.15 the server then answers 503 `unavailable` to every request until it is restarted and rebuilt. Any client able to register a feeder session (in E3: any holder of the feeder credential) can make the server permanently unavailable with one request | **Fixed** in knotrel/knotrel#104 (merged on `main` as `0fe35f6`): sequence 0 is classified as evicted in every state and rejected with 409 `unverifiable_duplicate` without state mutation; no feeder input reaches an `unreachable!()` arm in `apply_events` | `events.rs::test_29` (sequence 0 after `create_only` and `replace` imports, server stays ready, later events accepted) and `soak.rs::regression_d1_sequence_zero_at_generation_start`; the soak sends sequence 0 in every state → N6 (closed) |

### B5 — PostgreSQL and the reference feeder

| ID | Threat | Mitigation | Validation |
| --- | --- | --- | --- |
| T5.1 | Over-privileged feeder role used to modify the source (E) | Role with `LOGIN REPLICATION` and `SELECT` on the two tables only ([`sql/roles.sql`](../integrations/postgres-feeder/sql/roles.sql)) | PostgreSQL integration tests run as an equivalent role created inline (`integrations/postgres-feeder/tests/common/mod.rs`; `postgres-feeder` workflow); `roles.sql` itself is not executed by a test |
| T5.2 | Password exposure on the command line or in images (I) | `KNOTREL_PG_PASSWORD_FILE` preferred; never a command-line argument; Compose secrets; generated secrets ignored by git | Code review of `integrations/postgres-feeder/src/config.rs`; no automated test; doc ([reference feeder](../integrations/postgres-feeder/README.md)) |
| T5.3 | WAL retention exhausts the source's disk (D) | `max_slot_wal_keep_size` required (unbounded retention refused unless explicitly allowed); slot checks and cleanup | `integrations/postgres-feeder/tests/history.rs::unbounded_retention_is_refused` and the other feeder integration tests |
| T5.4 | Eavesdropping on PostgreSQL traffic or the feeder's HTTP traffic (I, T) | **No TLS**: the feeder connects to PostgreSQL with TLS disabled and its HTTP client is built without TLS support; supported only on a trusted network | Open → N2 |
| T5.5 | A compromised container on the reference Compose network controls Knotrel (E) | The reference example is E2: every service on its private network is trusted, and only the console is published, on loopback | Doc only (`examples/postgres-feeder/compose.yaml` header) |

### B6 — Build and release

| ID | Threat | Mitigation | Validation |
| --- | --- | --- | --- |
| T6.1 | Vulnerable or malicious dependency (T, E) | Committed `Cargo.lock` and `package-lock.json`; exact npm versions; `cargo deny` (advisories; the feeder also licenses, bans, sources); `npm audit --audit-level=high` | `vulnerabilities` workflow (every push, PR and weekly); `postgres-feeder` workflow; `console` workflow |
| T6.2 | Compromised CI action (T) | Actions pinned by commit SHA; `permissions: contents: read` by default | Workflow files; reviewed, not tested |
| T6.3 | Tampered release artifacts (T) | `SHA256SUMS` over the whole bundle, build provenance receipts, signed tags, console archive reproducible on three platforms, SBOM workflow | `release` workflow (inventory and checksum validation), `console` workflow (`reproducibility` job), `sbom` workflow; the release binary's reproducibility is a separate rc gate (criterion 6) |
| T6.4 | Base image drift (T) | Container base images pinned by digest | Compose and Dockerfiles; reviewed, not tested |

## 6. Concrete security gaps from the readiness assessment

The four gaps listed in [v1.0 readiness](releases/v1.0-readiness.md#concrete-security-gaps-within-the-trust-model):

### G1 — Import memory amplification

**Status: mitigated and documented; one sizing term missing.**

- Evidence: v0.6.0 replaced the import parse with a streaming parse; a
  100,000-node import peaked at 31–33 MiB (create) and 50 MiB (replace, old
  graph still live) against 111–130 MiB in v0.5.0
  ([deployment guide](deployment.md), resource bullets;
  [resource model](topology-lifecycle.md)). Only one import runs at a time and
  it is admitted before its body is read (`import.rs::test_1_…`). The deployment
  guide says to size memory for `old graph + import body + new graph`, as the
  gate requires.
- Remaining: a body that fails the streaming parse is parsed again by the former
  parser with the v0.5.0 working set (documented in the resource model), so a
  malformed 64 MiB import can still reach the old amplification once. Request
  bodies on the other routes are buffered before admission, bounded by
  `KNOTREL_MAX_CONNECTIONS × 1 MiB` (up to 1 GiB by default;
  [admission](http-api.md#admission-and-cancellation)); the deployment
  guide's memory sizing does not mention this term.
- **Proposed fix (documentation):** add `KNOTREL_MAX_CONNECTIONS × 1 MiB` of
  buffered bodies and the malformed-import fallback to the memory sizing in
  `docs/deployment.md`, and advise lowering `KNOTREL_MAX_CONNECTIONS` or
  `KNOTREL_MAX_IMPORT_BODY_BYTES` for small memory limits. Optional code
  proposal: drop the second parse for bodies above a size threshold.

### G2 — Lock-holding work per request

**Status: open (documented qualitatively, not measured).**

- Evidence: the costs are documented — a `connected` is an `O(V + E)` BFS,
  a batch has at most 1024 operations, query-only requests share the lock but
  delay mutations, feeder events and imports
  ([concurrency and order](http-api.md#concurrency-and-order)); compaction is an
  `O(V log V + E)` rebuild and catalog growth an `O(V)` rebuild under the lock
  ([topology lifecycle](topology-lifecycle.md)); a timed-out request does not
  stop its worker ([timeouts](http-api.md#timeouts-and-connection-bound)).
- Missing: the per-request **worst-case lock time** for the supported envelope
  (for example a 1024-`connected` batch and a compaction at the default limits
  of 100,000 nodes and 500,000 edges) and a decision on a configurable batch
  ceiling.
- **Proposed fix:** measure both as part of the declared supported envelope
  (criterion 4, rc) with declared criteria, publish them in
  `docs/http-api.md` and the deployment guide; propose an additive setting
  (for example `KNOTREL_MAX_BATCH_OPERATIONS`, default 1024, lower values
  allowed) as a maintainer decision. Both are server work outside this
  document.

### G3 — Feeder takeover is unauthenticated by design

**Status: met for documentation by this document (T4.2); mitigated in E3 and
validated.**

- Session fencing protects against stale feeders, not against a hostile
  client inside the boundary. Any client that can reach the feeder routes can
  take over (the instance ID is public). The deployment guide states that a
  second feeder takes over the session; this document states explicitly that
  this is not an authentication mechanism.
- Validation of the mitigation: in E3, feeder routes require a separate
  credential (gateway smoke test, T2.4); the console never forwards them
  (console smoke test, T3.2).
- **Proposed follow-up (documentation):** one sentence in
  [integration contract §4](integration-contract.md) and the
  [trust model](http-api.md#trust-model) linking here.

### G4 — `SECURITY.md` is stale

**Status: document fixed on this branch; rc gate still open (enable and
verify the reporting channel).**

- [`SECURITY.md`](../SECURITY.md) now lists the supported versions by policy
  (latest minor release line; 0.9.x today, 1.0.x after 1.0.0), names GitHub
  Private Vulnerability Reporting as the only channel (the unaddressed email
  option is removed), and links this document.
- **Maintainer decision (2026-11-22):** GitHub Private Vulnerability
  Reporting is the security channel, to be enabled and verified before the rc.
- **Open rc gate:** it is **disabled** on `knotrel/knotrel` (GitHub API,
  2026-11-22: `"enabled": false`). Before the rc the maintainer enables it
  (Settings → Code security → Private vulnerability reporting) and verifies it:
  the API reports `"enabled": true` and the Security tab shows
  **Report a vulnerability**. Until then the channel published in
  `SECURITY.md` does not work. Repository settings were not changed by this
  review.

## 7. Further findings of this review

Found while mapping mitigations to validations. None changes server behavior
here; each is a proposal.

| ID | Finding | Severity within the trust model | Proposed fix |
| --- | --- | --- | --- |
| N1 | **DNS rebinding against a loopback server (E1).** Knotrel accepts any `Host`, so a web page visited by a browser on the same host can rebind its name to `127.0.0.1` and read and mutate the graph, including taking over the feeder session after reading the instance ID from `/v1/info`. The console container defends against this (T3.3); a server reached directly does not. CSRF without rebinding is blocked by the JSON content-type check (T1.11) | Medium: E1 is the default exposure and trusts processes, not web pages | Code proposal: an optional `Host` allowlist (for example `KNOTREL_ALLOWED_HOSTS`, default loopback names and the bind address; 421 otherwise), additive, with a security test; until then, document in the trust model that browsing on a host with a loopback Knotrel is a risk |
| N2 | **Reference feeder has no TLS** to PostgreSQL (`NoTls`, `TlsConfig::disabled()`) or to Knotrel (`reqwest` without a TLS feature), so it cannot be used through the gateway example and sends the PostgreSQL password and graph data in clear text | Low for the documented local reference setup; blocks E3 for the reference feeder | Code proposal for the feeder: `sslmode` (`require`/`verify-full`) with a CA file for PostgreSQL; a rustls feature and a credential file for the Knotrel URL; extend the gateway smoke test with the reference feeder |
| N3 | **Gateway example has no rate or connection limiting**, although the trust model lists rate limiting among the gateway's duties | Low (authenticated clients only) | Add `limit_req`/`limit_conn` per credential to `examples/gateway/nginx.conf` with a smoke-test check, or state the omission in the deployment guide |
| N4 | **Unvalidated security configuration:** `console/deploy/nginx-gateway.conf` (static archive) is packaged but never run or syntax-checked; the console container's CSP and `nosniff` headers are not asserted by `smoke-container.sh`; the absence of CORS headers on Knotrel is not asserted | Low | Add `nginx -t` and an allowlist check for the archive config in the `console` workflow; header assertions in `smoke-container.sh`; a CORS-preflight assertion in `security.rs` |
| N5 | **Trust-model wording is narrower than the 1.0 exposure:** `docs/http-api.md` says "a single trusted local client" while E2 and E3 are supported | Documentation | Align the trust-model paragraph with section 1 of this document |
| N6 | **Defect D1 (T4.6), found by the R3 soak:** a feeder event with `delivery_sequence: 0` before the first event of a generation panics the graph worker and leaves the server permanently unavailable | High for availability: one request from a registered feeder; in E3 limited to feeder-credential holders | **Closed:** fixed in knotrel/knotrel#104 with regression tests that pass in CI. To list in the rc release notes |
| N7 | **Fail-stop amplifies every reachable panic (T1.15).** Fail-stop itself is intended, but nothing shows that request input cannot reach a panic; D1 shows it can | Medium: depends on finding such inputs; D1 is one | Audit of panic sites on request and worker paths (replace input-reachable ones with request errors); a property or fuzz test sending arbitrary feeder envelopes and operations in every synchronization state and asserting no `unavailable`; keep the soak's reference-model check. Optional maintainer decision: let a successful replace import clear the failed state instead of requiring a restart (behavior change, needs its own review) |

## 8. Residual risks accepted by the trust model

- Any client inside the boundary has full read access and, per mode, write and
  feeder access (no per-client authorization in Knotrel).
- CPU per request is not bounded beyond the batch ceiling, graph limits and
  admission; a timed-out request keeps its worker; there are no write
  timeouts or slow-reader protection ([non-guarantees](http-api.md#non-guarantees)).
- A worker panic is fail-stop: the server stays unavailable until restarted
  and rebuilt (T1.15); this is accepted, input-reachable panics are not (N7).
- Process RSS has no hard bound (allocator retention); a container memory
  limit is the operator's control.
- The graph and its synchronization state are lost on restart; integrity after
  a restart depends on a rebuild from the authoritative source.

## 9. Maintenance

Update this document with any change to: routes, modes or the trust model;
the console's forwarding rules; the gateway example; resource bounds; the
reference feeder's connections; the release pipeline. Each new mitigation names
its validation; a mitigation without one is listed as "doc only" or as a gap.
