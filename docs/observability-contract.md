# Observability contract

**Status:** Vocabulary for roadmap M6.1, nine M6.2 increments
(sections 7, 8, 10, 11, 12, 13, 14, 15 and 16) and the M6.3 structured logs
(section 17), released in v0.4.0. M6 is not
part of v0.3.0. M6.2 passed an independent review (section 6) with the
corrections applied; alert rules (M6.4) are in [alerting.md](alerting.md);
overhead and series count (M6.5) are qualified in section 18. M6 was accepted
by the owner on 2026-11-15. Rows marked *pending* in
section 4 are vocabulary only: no endpoint, metric or log field exists for
them yet. The opt-in `/metrics` endpoint does not change any existing route,
status code or body of the [HTTP API](http-api.md), the
[integration contract](integration-contract.md) or the
[deployment runbook](deployment.md). Section 19 adds the metrics of the M10
node and edge lifecycle support (v0.5.0), which exist only when
`KNOTREL_TOPOLOGY_LIFECYCLE=true`. Section 20 covers the inspection routes
(v0.8.0) and section 21 the per-lane admission metrics (v1.0.0, unreleased).

Scope: service health, workload pressure and feeder synchronization of one
`knotrel-server` process, including structured logs (section 17) and the
M6.5 qualification (section 18). Alert rules (M6.4) are documented
separately in [alerting.md](alerting.md). Out of scope: persistence metrics
and anything requiring a contention redesign (M5).

Code references below name symbols in
[`crates/knotrel-server/src`](../crates/knotrel-server/src) as observed at
the v0.3.0 release.

## 1. What exists today

| Signal | Where | Exposure | Notes |
| --- | --- | --- | --- |
| Liveness | `GET /health` ([`lib.rs`](../crates/knotrel-server/src/lib.rs) router) | Exposed | Always 200 `ok` while the listener serves. Takes no lock and says nothing about the graph. |
| Readiness | `GET /ready`, `lifecycle::readiness` | Exposed | Precedence: `unavailable` > `draining` > `starting` > published sync word. Lock-free (reads `PublishedReadiness`). In standalone mode `ready` means accepting work. In source-managed mode it is 200 only for `ready` with an unexpired lease. |
| Static configuration | `GET /v1/info` | Exposed | Engine, mode, `max_pending_jobs`, limits, `instance_id`. No lock, no permit. |
| Graph snapshot | `GET /v1/stats`, `Service::stats` | Exposed | `node_count`, `edge_count`, limits, remaining capacity, `state_version`. In source-managed mode it also reports `generation`, `source_position` and effective `sync_status`. **Runs as a graph job:** consumes an admission permit and takes the graph lock. |
| Response metadata | `api::inject_metadata_headers` | Exposed | `X-Knotrel-Instance-Id`, `-Graph-Id`, `-Generation`, `-State-Version`, `-Source-Position`, `-Sync-Status`, `ETag`. |
| Request log | `api::logging_middleware` | Exposed (tracing) | Runs inside a `request` span (`request_id`, `method`, `route`, and when known `generation`, `state_version`, `sync_status`); the line records `uri`, `status`, `latency_us`. `info` level; probes (and `/metrics` when enabled) at `trace`. Outermost layer, so it sees `request_timeout`, `shutting_down` and handler responses. A request future dropped before a response logs `request cancelled before response` (section 17). |
| Rejection and failure logs | `Service::run`, `Service::execute_feeder_worker`, `Service::acquire_import_permit`, `lifecycle::request_timeout`, `declare_gap`, worker `JoinError` handlers | Exposed (tracing) | Messages for capacity exhaustion, import capacity, request time limit, declared gap (`failed_delivery_sequence`, caller-supplied `reason`), worker panic and unavailable service. |
| Process lifecycle logs | [`main.rs`](../crates/knotrel-server/src/main.rs), [`serve.rs`](../crates/knotrel-server/src/serve.rs) | Exposed (tracing) | Startup configuration, shutdown requested, draining (`open_connections`, `outstanding_jobs`), shutdown deadline (`abandoned`), accept backoff, connection limit. |
| Outstanding worker jobs | `Lifecycle::outstanding_jobs`, `JobGuard` | Logged when draining; not exported since v1.0.0 | Queued plus running graph jobs, including import workers. Until v0.9.x it was `/metrics`' `graph_jobs_outstanding`; since v1.0.0 that gauge reports held admission permits per lane instead (section 21). |
| Admission capacity | `Service` permit semaphores: shared (`max_pending_jobs`), feeder data (`KNOTREL_FEEDER_RESERVED_JOBS`), feeder control (2); import semaphore (1) | Exposed via opt-in `/metrics` | Rejections are logged and counted by `requests_rejected_total` (section 8). |
| Lifecycle phase, worker failure | `Phase`, `unavailable` flag | Exposed via `/ready` and opt-in `/metrics` | `readiness_state` and `service_unavailable` gauges. |
| Synchronization state | `GraphState` (`generation`, `version`, `sync_status`, `applied_delivery_sequence`, `source_position`, session, epoch, `lease_deadline`) | Internal | Partly visible through `/v1/stats`, headers and `/ready`; lease time remaining via opt-in `/metrics` (section 14). |
| Event outcome counts | `FeederEventsResponse.events_applied` | Exposed per response; cumulative via opt-in `/metrics` | Fifth M6.2 increment; section 12. |
| Import preparation and catch-up timing | `import_duration_seconds`, `catchup_duration_seconds` (`metrics.rs`) | Exposed when `KNOTREL_METRICS_ENABLED=true` (source-managed mode) | Eighth and ninth M6.2 increments; sections 15 and 16. |
| HTTP counts, duration, backlog, capacity, unavailable flag, readiness, build, graph node and edge counts | `GET /metrics` (`metrics.rs`) | Exposed when `KNOTREL_METRICS_ENABLED=true` | M6.2 increments; sections 7, 8 and 10. |
| Graph-job queue, lock-wait and execution time | `JobClock` and `lock_timed` (`metrics.rs`), recorded by workers | Exposed when `KNOTREL_METRICS_ENABLED=true` | Fourth M6.2 increment; section 11. |
| Rejections before execution, uncertain-outcome observations | `ServiceTelemetry` (`metrics.rs`), owned by `Service` | Exposed when `KNOTREL_METRICS_ENABLED=true` | Second M6.2 increment; section 8. |
| Graph-operation and feeder-event outcomes | `ServiceTelemetry::count_operation`, `count_feeder_event` (`metrics.rs`), called by workers | Exposed when `KNOTREL_METRICS_ENABLED=true` | Fifth M6.2 increment; section 12. |
| Feeder rejections and desynchronizations | `ServiceTelemetry::count_feeder_rejection`, `count_desynchronization` (`metrics.rs`), called where the error or transition is produced | Exposed when `KNOTREL_METRICS_ENABLED=true` (source-managed mode) | Sixth M6.2 increment; section 13. |
| Feeder lease remaining, last-applied age | Published readiness word and `ServiceTelemetry::mark_applied` (`metrics.rs`), read by `HealthCollector` at scrape | Exposed when `KNOTREL_METRICS_ENABLED=true` (source-managed mode) | Seventh M6.2 increment; section 14. Inactivity, not source lag. |

### Concrete operator gaps

1. No cumulative counts for applied and duplicate events, gaps, conflicts and imports: they are visible only as individual log lines or response bodies. (Request counts by route and status class, pre-execution rejections by reason, and graph-operation and feeder-event outcomes are now available from the opt-in `/metrics`; section 12. Feeder rejections by code and desynchronizations by cause, including gaps and conflicts, are counted too; section 13. Imports remain uncounted.)
2. No backlog signal outside drain. (Now exported as `graph_jobs_outstanding` via the opt-in `/metrics`.)
3. Graph counts are readable only through `/v1/stats`, which competes for the same admission permits and graph lock as the work being diagnosed. (The opt-in `/metrics` now exposes worker-published counts without either; section 10.)
4. No breakdown of where time goes: caller latency cannot be split into queueing, lock wait and execution. (The opt-in `/metrics` now records worker queue, lock-wait and execution time per job kind; these intervals do not add up to HTTP latency; section 11.)
5. There is no record of how long bootstrap or replacement catch-up took. (The opt-in `/metrics` now records import phases and catch-up duration; sections 15 and 16.)
6. A dropped request future leaves no request log line. (The opt-in `/metrics` endpoint counts it as `cancelled`, and the request log now writes `request cancelled before response`; section 17.)
7. The status code alone cannot distinguish the `503` causes: `busy`, `request_timeout`, `shutting_down`, `unavailable`, and `import_in_progress` from saturated feeder calls. (The opt-in rejection and uncertain-outcome counters now record the internal cause; section 8.)

## 2. Semantic rules

These rules hold for every signal defined below.

**HTTP lifetime vs. worker lifetime.** `Service::run` and
`Service::execute_feeder_worker` move the admission permit and `JobGuard` into
a `spawn_blocking` closure. A caller timeout (`lifecycle::request_timeout`) or
a dropped request future ends the HTTP request; it does **not** cancel the worker.
HTTP signals describe the caller; job signals describe the worker; the two
are never merged. Tested in `lifecycle.rs`
`http_request_timeout_leaves_worker_active_and_shutdown_waits_for_it`.

**Cancelled is not "client disconnected".** The server observes only that
the request future was dropped before a response was constructed. A closed
client connection is one cause, but the connection task can also end for
other reasons, such as process shutdown. The transport gives no
reliable cause, so the classification is the neutral `cancelled`. A response
that was constructed but whose body was not fully delivered counts as
completed with its status class.

**Where server-side latency ends.** HTTP duration runs from entry into the
metrics middleware to the moment the inner service returns a response
(headers and an un-sent body). It excludes body transmission, the client's
network time and connection setup, and is therefore **not** client-observed
latency. All Knotrel bodies are buffered before the response is returned, so
the excluded part is transmission only.

**Admission, queue, lock wait, execution.** Admission is a non-blocking
`try_acquire_owned`, so there is no admission wait to measure. Admission is
either accepted or rejected. Once accepted, time is split into:

- **queue:** from `spawn_blocking` until the closure starts (blocking-pool scheduling);
- **lock wait:** from the graph-lock request until it is acquired (shared for
  query-only `operation`/`batch` jobs and `stats`, exclusive for every other
  kind; since v0.7.0);
- **execution:** time holding the graph lock.

Import preparation (parse and candidate construction) runs on the worker
before its lock request and belongs to none of the three; it is timed by
`import_duration_seconds{phase="prepare"}` (section 15). Exact boundaries are
in section 11.

**Not executed vs. outcome uncertain.** A status code alone does not say
whether work executed; observations are classified at the boundary that
produced them.

Definitely not executed:

- `busy`, `import_in_progress` (admission refused) and `shutting_down` (`lifecycle::shutdown_gate`);
- `unavailable` produced *before* work starts: the entry checks in
  `Service::run` / `execute_feeder_worker` and `api::check_service_available`,
  and the in-worker check that finds `graph.failed` or the unavailable flag
  set before calling the job;
- validation errors (4xx before execution);
- whole-request feeder rejections such as `stale_generation`, `fenced_feeder_session` and `lease_expired`.

Outcome uncertain:

- `503 request_timeout` while the handler was waiting for an admitted
  state-changing graph job: the worker keeps running and may still apply the
  mutation. A timeout before admission (for example during body receipt) or
  while waiting for a read-only job leaves no graph effect in doubt;
- `cancelled` (dropped request future) in the same position: same as above;
- `unavailable` produced by a worker failure (the `JoinError` mapping after
  `spawn_blocking` in `Service::run`, `execute_feeder_worker` and
  `Service::import`): the job may have started, and a batch or event stream
  may have applied a prefix before the panic. The service then stays
  unavailable until restart, so no later request can observe or repeat it.

Metrics count the two groups separately and never report an uncertain
outcome as "did not execute". The HTTP slice (section 7) only classifies
status classes; it makes no execution claim.

**Import preparation vs. publication.** `Service::import` parses and builds
the candidate graph off-lock, then validates session, lease, mode and
`If-Match` under the lock, and only then publishes. A completed preparation
may still be rejected at publication. Only publication changes `generation`.

**Effective mutation vs. no-op vs. source advancement.** `state_version`
advances only on a `changed` mutation. No-op mutations and `progress` events
advance `applied_delivery_sequence` and `source_position` but not
`state_version`. Exact duplicates advance nothing.

**Liveness vs. synchronized readiness.** `/health` says the process serves
HTTP. Only `/ready` (and the derived status gauge below) says whether the graph
is synchronized. No signal infers freshness from liveness.

**Synchronization state vs. lag.** `source_position` is an opaque cursor that
Knotrel never compares, and the protocol carries no source high-water mark.
Knotrel therefore has no authoritative frontier to subtract from, and
**defines no lag metric**. Elapsed time since the last applied event is
exposed under a name that does not say "lag": it measures inactivity, which
may equally mean an idle source.

**Lazy staleness.** No timer moves a graph to `stale`. Lease expiry is
observed by `/ready` (`Service::try_mark_stale_from_expired_lease`), feeder
calls (`validate_session_and_lease`) or effective-status reads. Telemetry
must therefore derive `stale` at observation time from the lease deadline.
It must not count recorded transitions, which would depend on who happened to
look.

**`import_in_progress` from feeder saturation.** Feeder session, heartbeat,
events, gap and barrier calls that fail admission return `503 import_in_progress`
(`FeederError::from(ServiceError::Busy)`), although the log says
`busy`. This is accepted compatibility debt (closure plan D7). Telemetry
classifies by the internal cause (`busy`) and leaves the HTTP code unchanged.

## 3. Counting units

| Unit | Counted once per | Batch / multi-event behavior |
| --- | --- | --- |
| HTTP request | request that reached the router and either produced a response or was cancelled | A batch counts as one request regardless of size or per-operation errors. Each request is counted exactly once. A scrape of `/metrics` counts itself after it completes, so it appears in the next scrape. Requests refused by the connection limit or malformed before routing never reach the router and are not counted. |
| Graph job | accepted worker job (operation, batch, stats, feeder call, import publication) | A batch is one job. |
| Graph operation | operation in `/v1/operations` or `/v1/batch` that reached the graph | Every executed batch entry is counted once with its own outcome; a batch is never counted as a single failure. Operations rejected before execution (size limit, malformed request, admission, the source-managed synchronization gate) are not graph operations (section 12). |
| Feeder event | event evaluated in `/v1/feeder/events` | Applied prefix: each event counted by outcome. A retained exact duplicate counts as `duplicate`. The first failing event counts as `rejected`. Events after it were never evaluated and are not counted. Whole-request rejections count zero events. |
| Import | import request that obtained the import permit | Counted exactly once, with a terminal result, by whichever of handler or worker owns it when it ends. A body read abandoned by its caller, and an import whose worker closure is dropped without running, count `abandoned` (section 15). |
| Rejected feeder request | session, heartbeat, events, gap or barrier request for which the service produced a feeder-protocol error | Once per request, under the underlying error code; a partial event failure is one rejection, not one per event and not `partial_failure` (section 13). Admission refusals, HTTP-adapter checks and imports are not feeder rejections. |
| Desynchronization | transition of the graph into `desynchronized` | Once per transition; a request on a graph already `desynchronized` adds none (section 13). |
| Rejected request | request refused before any graph job or import ran for it (`shutting_down` also covers requests that would not create one) | At most once, at the first refusing boundary (section 8). Partial batch or event failures are not rejections. |
| Uncertain-outcome observation | caller abandonment of, or worker panic in, an admitted state-changing job | Not deduplicated per request: a timeout followed by a worker panic is two observations (section 8). |

## 4. Minimum vocabulary

The "M6.2" column gives the proposed Prometheus name (prefix `knotrel_`).
`E` marks essential first delivery and `D` marks deferred. **Implemented**
means served by one of the M6.2 increments (sections 7, 8 and 10 to 16);
**pending** means vocabulary only. Every series is
per process and resets to zero on process restart, because Knotrel holds no
durable state. "Replacement" means publication of a new generation by import.

| Name (M6.2) | Type, unit | Operator question | Meaning and producing boundary | Labels (bounded) | On replacement | Today | M6.2 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `http_requests_total` | counter | What are callers sending and receiving? | One per request (section 3), recorded by `metrics::track_http` when the inner service returns a response, or by its drop guard as `cancelled` if the future is dropped first. Never both. | section 7 | continues | **implemented** | E |
| `http_request_duration_seconds` | histogram, s | How long does the server take to produce a response? | Server-side, from middleware entry to response construction (section 2). Includes `request_timeout` and `shutting_down` responses. Excludes body transmission, cancelled requests and worker time after the caller has gone. | `route` | continues | **implemented** | E |
| `requests_rejected_total` | counter | Is work refused before execution, why, and on which admission lane? | One per request refused before its graph job or import runs, at the boundary that refuses it (section 8). At most one per request. `lane` names the admission lane whose permits were exhausted (section 21). | `reason` ∈ {`busy`, `import_capacity`, `shutting_down`, `unavailable`}; `lane` ∈ {`shared`, `feeder_data`, `feeder_control`, `import`, `none`}, only the 6 combinations of section 21 | continues | **implemented** | E |
| `requests_outcome_uncertain_total` | counter | How often may a state change have happened without its caller learning the result? | One per **observation**, not per distinct request (section 8): a caller stopped waiting for an admitted state-changing job (`request_timeout`, `cancelled`), or such a job's worker panicked (`worker_panic`). One request can contribute a caller observation and later a `worker_panic`. `http_requests_total{status_class="cancelled"}` is unchanged and counts every dropped request, including probes and reads. | `cause` ∈ {`request_timeout`, `cancelled`, `worker_panic`} | continues | **implemented** | E |
| `graph_jobs_outstanding` | gauge, jobs | How deep is each lane's backlog? | Admission permits of the lane held at scrape (section 21): jobs admitted on the lane and not yet finished by their worker, queued or running, including jobs whose caller timed out or was cancelled; for the import lane, the import from permit acquisition (before body receipt) to its end. Never above `graph_job_capacity` of the same lane. The process-wide drain count (`Lifecycle::outstanding_jobs`) is not exported. | `lane` ∈ {`shared`, `feeder_data`, `feeder_control`, `import`} | unaffected | **implemented** | E |
| `graph_job_capacity` | gauge, jobs | What is each lane's admission limit? | Permit limit of the lane, constant after startup: `shared` = `max_pending_jobs`, `feeder_data` = `KNOTREL_FEEDER_RESERVED_JOBS`, `feeder_control` = 2, `import` = 1 (section 21). | `lane`, same values and series as `graph_jobs_outstanding` | constant | **implemented** | E |
| `graph_job_queue_seconds`, `graph_lock_wait_seconds`, `graph_job_execution_seconds` | histograms, s | Is latency queueing, contention or work? | Section 2 split, recorded inside the worker closure, so it is observed even when the caller has gone. Exact boundaries in section 11. | `kind` ∈ {`operation`, `batch`, `stats`, `feeder_session`, `feeder_heartbeat`, `feeder_events`, `feeder_gap`, `feeder_barrier`, `import_publish`, `inspect`} | continues | **implemented** | E |
| `graph_operations_total` | counter | What mix of operations runs, and how many change the graph? | One per executed operation (section 3), counted by the worker when `GraphState::apply` returns, under the graph lock (section 12). Feeder events and imports are never counted here. | `op` ∈ {`add_node`, `link`, `cut`, `connected`, and with lifecycle support `set_node_active`, `set_edge_enabled`, `remove_node`, `node_state`, `edge_state`}; `outcome` ∈ {`changed`, `unchanged`, `error`} for mutations, {`true`, `false`, `error`} for `connected`, {`registered`, `unregistered`, `error`} for `node_state` and `edge_state` (section 19) | continues | **implemented** | E |
| `graph_nodes`, `graph_edges` | gauges | How big is the graph? | Published by the worker, under the graph lock, when a state-changing job's graph work returns and at import publication; initialized at construction. Retains the last published values; not rolled back by a later worker panic. Read at scrape without the graph lock. Not a coherent pair (section 10). With lifecycle support they are registered counts, whatever the flags (section 19). | none | jump to published import counts | **implemented** | E |
| `graph_active_nodes`, `graph_enabled_edges`, `graph_effective_edges` | gauges | How much of the registered topology takes part in connectivity? | Registered nodes whose flag is active; registered edges whose flag is enabled; edges in the effective graph (enabled, both endpoints active). Published together with `graph_nodes` and `graph_edges` at the same boundaries; same failure and coherence rules. Lifecycle support only (section 19). | none | jump to published import counts | **implemented** | E |
| `service_unavailable` | gauge 0/1 | Has a worker failure disabled the service? | The `unavailable` flag. Becomes 1 after a worker panic and stays 1 until process restart. | none | unaffected | **implemented** | E |
| `readiness_state` | gauge, one-hot | What would `/ready` say? | `lifecycle::readiness_state`: same precedence as `lifecycle::readiness`, computed read-only at scrape. Stale is derived from the published deadline, without the compare-and-swap. | `state` ∈ {`unavailable`, `draining`, `starting`, `ready`, and in source-managed mode `uninitialized`, `loading`, `catching_up`, `stale`, `desynchronized`} | follows state | **implemented** | E |
| `feeder_lease_remaining_seconds` | gauge, s | Is the feeder alive? | Lease deadline from the published readiness word minus the monotonic clock, derived at scrape (section 14). Positive while no check reports the lease expired; zero throughout the deadline millisecond, when feeder calls may already fail with `lease_expired`; negative once every check, including `/ready`, reports it expired. Scraping never renews the lease or publishes staleness. Absent before the first session and in standalone mode. | none | session kept | **implemented** | E |
| `feeder_events_total` | counter | Is the source making progress, and how much is redundant? | Per evaluated event (section 3) in `GraphState::apply_events`, under the graph lock (section 12). `unchanged` covers no-op mutations and `progress`. | `result` ∈ {`changed`, `unchanged`, `duplicate`, `rejected`} | continues | **implemented** | E |
| `feeder_rejections_total` | counter | Why did a feeder call fail? | One per session, heartbeat, events, gap or barrier request the service rejected, where the `FeederError` is produced (section 13). Labelled with the underlying error code, not `partial_failure`. An event domain error (any core code) collapses to `event_failed`. Excluded: admission `busy` (HTTP `import_in_progress`) and `unavailable` (counted in `requests_rejected_total`), timeouts, cancellations and panics (`requests_outcome_uncertain_total`), HTTP-adapter checks and malformed bodies, and imports (`imports_total`). Source-managed mode only. | `code` ∈ {`session_required`, `fenced_feeder_session`, `lease_expired`, `invalid_lease_duration`, `graph_uninitialized`, `graph_desynchronized`, `stale_generation`, `source_mismatch`, `source_epoch_mismatch`, `out_of_order_sequence`, `source_predecessor_mismatch`, `conflicting_duplicate`, `unverifiable_duplicate`, `sequence_exhausted`, `invalid_request`, `topology_lifecycle_disabled`, `event_failed`, `barrier_position_mismatch`, `barrier_not_reached`, `superseded_barrier`} | continues | **implemented** | E |
| `desynchronizations_total` | counter | Why did the graph require a rebuild? | One per transition into `desynchronized`, at the sites in `GraphState::apply_events` and `declare_gap` that set it. A predecessor mismatch covers both `prev_delivery_sequence` and `prev_source_position`, which share one wire code. Out-of-order sequences and evicted duplicates do not desynchronize. | `cause` ∈ {`gap_declared`, `source_epoch_mismatch`, `source_predecessor_mismatch`, `conflicting_duplicate`}. Source-managed mode only (section 13). | continues | **implemented** | E |
| `feeder_last_applied_age_seconds` | gauge, s | When did the graph last move forward? | Monotonic time since a worker, under the graph lock, last applied a non-duplicate event (including no-op mutations and `progress`) or published an import (section 14). Not reset by duplicates, rejected events or imports, gaps, barriers, sessions, heartbeats or queries. Absent before the first application and in standalone mode. **Not lag** (section 2). | none | reset by publication | **implemented** | E |
| `import_in_progress` | gauge 0/1 | Is an import running? | Import permit held: from acquisition, through body receipt, worker queue, preparation and publication, until the import is recorded, also after its caller has gone. Source-managed mode only (section 15). | none | — | **implemented** | E |
| `imports_total` | counter | Do imports succeed? | Exactly one per permitted import, classified by the phase reached (section 15). Source-managed mode only. | `result` ∈ {`published`, `payload_rejected` (body read, parse, request identity, limits and candidate), `publication_rejected` (after the lock request: unavailable, fencing, lease, mode, precondition, exhausted), `worker_failed` (panic before publication), `abandoned` (ownership ended before the worker started: abandoned body read, worker closure dropped unrun)} | continues | **implemented** | E |
| `import_duration_seconds` | histogram, s | Where does import time go? | `receive` (permit acquired to body read returned), `prepare` (worker parse and build, off-lock), `publish` (lock request to worker end: lock wait, validation, swap). Disjoint; a failed phase is observed up to its error, a phase interrupted by a panic or an abandoned body read is not (section 15). Source-managed mode only. | `phase` ∈ {`receive`, `prepare`, `publish`} | continues | **implemented** | E |
| `catchup_duration_seconds` | histogram, s | How long did bootstrap or replacement take to become ready? | From import publication (entering `catching_up`) to the first `synchronized` barrier (B2) in that generation (section 16). Abandoned if the generation is replaced or desynchronized first. | `kind` ∈ {`initial`, `replacement`} | new interval starts | **implemented** | E |
| Recovery-from-stale duration | histogram | — | The start of `stale` is lazy (section 2); defining it needs a decision on lease-deadline anchoring. | — | — | none | D |
| Session churn (`registered`, `takeover`, `renewed`) | counter | — | Fencing failures are already covered by `feeder_rejections_total`. | — | — | none | D |
| Dedup window occupancy, applied delivery sequence | gauges | — | Not needed to explain failures in the first delivery. | — | — | none | D |
| Connection-level counts (open, limit rejects) | gauge/counter | — | Server-loop concern, already logged. | — | — | logs | D |

`state_version` and `generation` stay in `/v1/stats` and response headers.
They are not metric values: `state_version` is arbitrary precision and
generation identity is a correlation key, not a quantity.

## 5. Labels and correlation

- Allowed label values are only the enumerated sets above and in section 7,
  all fixed at compile time. Raw paths never become labels: only matched
  route templates, or `unmatched`.
- Never use as label values: node IDs, request or event IDs, `instance_id`,
  generation, `state_version`, `source_id`, `source_epoch`, session IDs,
  `source_position` or other cursors, delivery sequences, caller-supplied
  text (for example the gap `reason`), or engine-internal identifiers.
- The engine and mode are startup constants and belong in one
  `knotrel_build_info{version, engine, mode}` gauge set to 1, not on every
  series.
- Correlation belongs in structured logs (M6.3): a request, operation or event
  log line may carry `instance_id`, generation, `state_version`, `source_id`,
  sequence and a sanitized cursor. It never carries graph payloads or
  credentials. Metrics answer "how much and how often"; logs answer "which
  one".

## 6. M6.2 handoff

**First slice (done, section 7):** `GET /metrics`, HTTP counter and
duration, outstanding jobs, capacity, unavailable flag, readiness state and
build info.

**Second increment (done, section 8):** rejection and uncertain-outcome
counters.

**Third increment (done, section 10):** graph node and edge gauges.

**Fourth increment (done, section 11):** graph-job queue, lock-wait and
execution histograms.

**Fifth increment (done, section 12):** graph-operation and feeder-event
outcome counters.

**Sixth increment (done, section 13):** feeder rejection and
desynchronization counters.

**Seventh increment (done, section 14):** feeder lease remaining and
last-applied age gauges.

**Eighth increment (done, section 15):** import in-progress gauge, result
counter and phase durations.

**Ninth increment (done, section 16):** catch-up duration.

**Remaining essential (`E`):** none.

**Independent review (2026-11-15).** An acceptance review of every
implemented row against sections 2 to 16 and the invariants below found no
code defects. Its corrections are applied: the last-applied stamp is
documented as once per events request (section 14), session and heartbeat
calls are listed as republishing graph counts (section 10), stale status and
series-count passages are updated, and two tests were added
(`histogram_bucket_bounds_are_exact`, and label domains keyed per metric
family in `exposition_is_valid_and_uses_only_bounded_labels`). M6.2 is
accepted on that basis; Acceptance M6 still requires M6.4 and M6.5.

**Instrumentation locations for the remaining rows:**

| Signal | Location |
| --- | --- |
| Operations | `GraphState::apply`, called per operation for single and batch requests. |
| Imports | `api::feeder_import` and `Service::import` (implemented, section 15). |
| Catch-up | Start instant stored in `GraphState` at publication; observed in `declare_barrier` B2 and recorded on worker drop (implemented, section 16). |
| `/metrics` reads | Atomics (including the lane semaphores' available permits), the published readiness word and the registry. No graph lock, admission permit or import permit; registry synchronization still applies. |

These can reuse the per-instance `ServiceTelemetry` handle that `Service`
now owns (section 8).

**Invariants (all slices):**

1. `/metrics` takes no admission permit, no graph lock and no import permit. It is never rejected by `busy`, and it reports during saturation, draining (treated like the probes in `lifecycle::shutdown_gate`) and worker failure.
2. Instrumentation never changes status codes, response bodies, headers, admission order or the compare-and-swap semantics of `/ready`.
3. Label sets are exactly those listed; series count is independent of graph size, request count, generations and sessions.
4. Counters are monotonic within a process; graph replacement never resets a counter.
5. Worker-side observations are recorded even when the caller timed out or was cancelled; HTTP observations never imply that a worker finished.
6. Registries are owned per router instance; no global registry or recorder.
7. With metrics disabled, no accounting or timing work is added to the request path.

**Focused tests for the remaining rows:**

- A batch with a mid-batch domain error yields one request and per-operation outcomes.
- A partial feeder failure counts the prefix as applied, the failing event as `rejected` and later events not at all.
- Duplicates and `progress` events do not count as `changed`.
- A gap increments `desynchronizations_total{cause="gap_declared"}`.
- Import rejected at publication shows `prepare` observed and `result="publication_rejected"`.
- Replacement keeps counters and restarts catch-up.
- The label-value set after a churn of generations and sessions is unchanged.

**Overhead qualification (M6.5).** Budget approved by the owner on
2026-11-15, before measurement: per qualification cell, metrics enabled
versus disabled on one release binary, the paired median slowdown must be at
most **3%** and the upper bound of its 95% interval at most **6%**. A cell
outside that is re-run once with a fixed extension; a cell still outside
without a confirmed regression is disclosed as INCONCLUSIVE, and a confirmed
regression beyond the budget blocks Acceptance M6. The series count must be
exactly constant after churn. Results: section 18. The approach:

- paired runs of one release build with metrics disabled versus enabled, on
  the existing qualification cells with the balanced collector;
- a budget and stopping rules agreed in advance;
- a series-count check after repeated imports and session takeovers.

No campaign has been run.

## 7. First M6.2 slice (released in v0.4.0)

**Enablement.** `KNOTREL_METRICS_ENABLED` is exactly `true` or `false`
(default `false`). Any other value fails startup. The embedded equivalent is
`ServerConfig::with_metrics_enabled`; `router()` keeps it disabled. When
disabled, the `/metrics` route is not registered (404, or `503 shutting_down`
while draining, exactly as before) and the HTTP metrics layer is not added,
so no timing or accounting work runs.

**Exposure.** `GET /metrics` on the existing listener, under the same trust
boundary as the API and **without authentication** (see
[deployment](deployment.md)). Response: `200`,
`Content-Type: text/plain; version=0.0.4; charset=utf-8`,
`Cache-Control: no-store`. A scrape reads atomics (including the
outstanding-job count), the published readiness word and the in-memory
registry. It takes no graph lock and no admission or import permit. It is
not lock-free: it uses synchronization internal to the `prometheus-client`
registry (the metric-family maps and histogram state, which request
accounting also updates). The readiness gauge
itself is computed from atomics and the clock, like `/ready`. While draining it is
served like the probes, with `Connection: close`.

**Middleware order** (outermost first): request logging, `metrics::track_http`,
`lifecycle::shutdown_gate`, `lifecycle::request_timeout`, handler. HTTP
metrics therefore see `shutting_down` and `request_timeout` responses.

**Labels.**

- `route`: the matched router template — `/health`, `/ready`, `/metrics`,
  `/v1/info`, `/v1/stats`, `/v1/operations`, `/v1/batch`,
  `/v1/feeder/session`, `/v1/feeder/heartbeat`, `/v1/feeder/import`,
  `/v1/feeder/events`, `/v1/feeder/gap`, `/v1/feeder/barrier`, `/v1/nodes`,
  `/v1/nodes/{node}`, `/v1/nodes/{node}/edges`, `/v1/edges` — or
  `unmatched` (18 values; the four inspection templates were added by
  section 20). Node IDs never appear in the label: the template is used. A known path with an unsupported method (`405`)
  keeps its template.
- `method`: `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`,
  `TRACE`, `PATCH`, or `other` for any extension method (10 values).
- `status_class`: `1xx`, `2xx`, `3xx`, `4xx`, `5xx`, `other` (a status
  outside 100–599), or `cancelled` (7 values).

**Series.**

| Series | Type | Meaning |
| --- | --- | --- |
| `knotrel_http_requests_total{route,method,status_class}` | counter | Section 4. Exactly one increment per request: a drop guard records `cancelled` only if no response was constructed. |
| `knotrel_http_request_duration_seconds{route}` | histogram, s | Section 4. Buckets: 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30, `+Inf`. The upper buckets cover `request_timeout` (default 30 s, configurable to 1 h; longer values fall in `+Inf`). |
| `knotrel_graph_jobs_outstanding{lane}` | gauge | Section 4; `lane` since v1.0.0 (section 21). |
| `knotrel_graph_job_capacity{lane}` | gauge | Section 4; `lane` since v1.0.0 (section 21). |
| `knotrel_service_unavailable` | gauge 0/1 | Section 4. |
| `knotrel_readiness_state{state}` | gauge, one-hot | Section 4. Standalone exposes 4 states, source-managed 9. Computed once per scrape, so the output is never torn. |
| `knotrel_build_info{version,engine,mode}` | gauge = 1 | Crate version, configured engine (`/v1/info` value) and `standalone` or `source_managed`. Constant for the process. |

HTTP counters and histograms are created on first use. All series reset
only on process restart; graph replacement does not affect them.

**Maximum series count** (one process, including every histogram bucket,
`_sum` and `_count`):

| Family | Bound |
| --- | --- |
| `http_requests_total` | 14 × 10 × 7 = 980 |
| `http_request_duration_seconds` | 14 × (16 buckets + `_sum` + `_count`) = 252 |
| Health gauges | 1 + 1 + 1 = 3 |
| `readiness_state` | 4 standalone, 9 source-managed |
| `build_info` | 1 |
| **Total** | **1240 standalone, 1245 source-managed** |

The library adds no other series (no `_created`, process or runtime
metrics). Most route/method combinations can only yield `405`, so real
counts are far below the bound. The second increment adds 7 series and the
third adds 2. The fourth adds 153 standalone and 459 source-managed
(section 11). The fifth adds 12 standalone and 7 source-managed (section
12). The sixth adds 23 source-managed only (section 13). The seventh adds
at most 2 source-managed only (section 14). The eighth adds 57
source-managed only (section 15). The ninth adds 32 source-managed only, for a
maximum of **1414 standalone, 1834 source-managed** (section 16). The M10
lifecycle metrics raise the current maximum to **1414 standalone and 1835
source-managed without lifecycle support, 1432 standalone and 1844
source-managed with it** (section 19).

**Dependency.** [`prometheus-client`](https://crates.io/crates/prometheus-client)
0.26, the Prometheus project's Rust client, with default features disabled.
It was chosen because:

- a `Registry` is an ordinary value with no default global instance, so
  owning one per router keeps embedded users and parallel tests isolated
  without extra discipline. (The `prometheus` crate also supports explicit
  per-instance registries; only its default registry and convenience
  macros are global.)
- it encodes Prometheus text format 0.0.4 directly into a `String`, with no
  protobuf dependency;
- its `Collector` trait lets the service-health gauges be read from
  `Service` and `Lifecycle` at scrape time, without background updates or
  duplicated state;
- `Family` creates labelled series lazily, matching the bounded label sets
  in this section.

It adds `prometheus-client-derive-encode`, `dtoa`, and `parking_lot` 0.12
(a second, older copy alongside the workspace's 0.13).

**Tests.** [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)
covers:

- disabled and enabled configuration;
- exposition validity and bounded labels, including unknown routes and
  extension methods;
- route templates;
- saturation, draining and worker failure;
- completion, timeout and cancellation accounting;
- registry isolation between instances.

`lifecycle.rs` `observed_readiness_matches_probe_without_transitioning`
checks `readiness_state` against `/ready` for every phase and sync status,
including expired leases, without changing the published word.
`configuration.rs` covers strict parsing.

## 8. Second M6.2 increment: rejection and uncertain outcomes (released in v0.4.0)

Two counters separate work that was refused before it ran from work whose
effect its caller did not learn. Each is recorded where the decision is
made, never derived from an HTTP status or error code.

**Accounting.** "Caller" is the request future; "worker" is the
`spawn_blocking` closure. A job is *state-changing* unless it is
`/v1/stats` or an `/v1/operations` / `/v1/batch` request containing only
`connected` queries. Session, heartbeat, events, gap, barrier and import
jobs are state-changing.

| Case | Observation boundary | Series | Unit | Recorded by |
| --- | --- | --- | --- | --- |
| Admission capacity exhausted | `try_acquire_owned` failure of the request's lane in `Service::admit_in` (shared, feeder data or feeder control) | `requests_rejected_total{reason="busy",lane=<that lane>}` (section 21). HTTP stays `503 busy`, or `503 import_in_progress` for feeder session, heartbeat, events, gap and barrier calls (section 2); telemetry still records the internal cause `busy`. | request | caller |
| Import permit exhausted | `Service::acquire_import_permit` | `requests_rejected_total{reason="import_capacity",lane="import"}` | request | caller |
| Shutdown | `lifecycle::shutdown_gate` refusing a non-probe request while draining. This includes requests that would never create a graph job, such as `/v1/info` and unmatched routes. | `requests_rejected_total{reason="shutting_down",lane="none"}` | request | caller |
| Service already unavailable | The first check that refuses: `api::check_service_available`; the entry checks of `Service::run` and `execute_feeder_worker`; or the in-worker check under the graph lock (`graph.failed` or the unavailable flag) in `run`, `execute_feeder_worker` and `Service::import`. That check refuses before any change is applied to the live graph or a candidate is published; for import, JSON parsing and candidate construction may already have happened off-lock. | `requests_rejected_total{reason="unavailable",lane="none"}` | request | caller, or worker for the in-worker check |
| Request timeout | `lifecycle::request_timeout` drops a caller that is still waiting for an admitted state-changing job | `requests_outcome_uncertain_total{cause="request_timeout"}` | observation | caller-side drop guard |
| Request-future cancellation | The same caller is dropped for any other reason | `requests_outcome_uncertain_total{cause="cancelled"}` | observation | caller-side drop guard |
| Worker panic | `WorkerPanicGuard` dropped while panicking in a state-changing job's worker, including after its caller has gone | `requests_outcome_uncertain_total{cause="worker_panic"}` | observation | worker |

**Counting units.**

- `requests_rejected_total` counts requests. Each request stops at its first
  refusal, so it is counted at most once.
- `requests_outcome_uncertain_total` keeps the name proposed in M6.1, but
  its unit is an **uncertainty observation by cause**, not a distinct
  request. A request that times out and whose worker later panics adds one
  `request_timeout` and one `worker_panic`, so the sum over `cause` can
  exceed the number of affected requests. Do not read it as a request count.
- The two counters can both see one request. A caller that times out while
  its job is still queued adds `request_timeout`. If that worker later finds
  the service unavailable, it adds `rejected{unavailable}`. Each observation
  is true at its own boundary.

**Exactly once through layers.** Busy and import-capacity rejections are
counted only where the permit is refused. The service and API error mappings
that follow never count. The `JoinError` mappings never count: a panic is
counted inside the worker, and a caller that receives the `JoinError` has
already disarmed its drop guard.

**Not counted.** The following produce no observation:
- validation and domain errors, and feeder protocol rejections (fencing,
  lease, generation, sequence); these are counted by `feeder_rejections_total`
  (section 13);
- partial batch or event failures;
- timeouts and cancellations before admission or while waiting for a
  read-only job;
- panics in read-only jobs. These still set `service_unavailable`;
- worker-side observations of a blocking task cancelled by runtime shutdown
  before it starts. Its caller's drop guard is dropped with the runtime too
  and may still record `cancelled` for a state-changing job, consistent
  with the caller semantics above.

**Mechanism and ownership.**
- `Service` owns an `Option<Arc<ServiceTelemetry>>`. It is created only when
  metrics are enabled, and `shutdown_gate` receives a clone. `Metrics`
  registers its two families.
- Neither `ServiceTelemetry` nor its counters reference `Service` or the
  registry, so there is no ownership cycle.
- Every series is created at startup, so the families report zeros before
  the first event. The counters are atomics; recording takes no registry lock.
- When metrics are disabled, `Service` holds `None`. No counter is
  registered or updated. `request_timeout` keeps its previous code path
  without a timeout marker. The service still builds an inert
  `PendingOutcome` on the stack, but no uncertainty guard is armed, and it
  performs no heap allocation or task-local access.
- When enabled, `request_timeout` runs the handler inside a task-local
  timeout marker. It sets the marker before dropping the timed-out future,
  which lets the caller-side drop guard tell `request_timeout` from
  `cancelled`.

**Series.**

| Series | Type | Values |
| --- | --- | --- |
| `knotrel_requests_rejected_total{reason,lane}` | counter | 4 (`lane` added in v1.0.0; 6 series in source-managed mode: section 21) |
| `knotrel_requests_outcome_uncertain_total{cause}` | counter | 3 |

These add 7 series. The maximum becomes **1247 standalone, 1252
source-managed**.

**Tests.** The deterministic cases are in
[`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs) (module
`outcomes`), plus a unit test in `service.rs` for the in-worker unavailable
check. They cover:
- every rejection reason, each counted exactly once;
- the feeder `import_in_progress` mapping counted as `busy`;
- a timeout with the worker outstanding, followed by success;
- a worker panic after caller cancellation;
- a timeout followed by a worker panic;
- an already-unavailable rejection versus a worker failure;
- read-only timeouts and cancellations not counted;
- disabled telemetry and instance isolation.

## 9. Decisions

1. **Exposure of `/metrics`.** Resolved in section 7: existing listener and
   trust boundary, disabled by default, enabled by a startup option,
   documented as unauthenticated.
2. **Graph counts without the lock.** Resolved in section 10:
   worker-published gauges rather than a lock-taking scrape. The values
   trail an in-flight job until it completes; the metric help states the
   publication boundary.
3. **Cancellation accounting.** Resolved in section 7: counted as the
   neutral `cancelled`, not `client_closed`. Resolved for logs in section 17:
   `logging_middleware` writes one `request cancelled before response` line.
4. **Lag.** Recommendation: no lag metric until the protocol carries a
   comparable source frontier. That would be an integration-contract change,
   not an observability change.
5. **Overhead budget.** Resolved on 2026-11-15 (section 6): median at most
   3%, 95% interval upper bound at most 6%, one fixed extension.

## 10. Third M6.2 increment: graph node and edge gauges (released in v0.4.0)

**Series.** `knotrel_graph_nodes` and `knotrel_graph_edges`, unlabelled
gauges. They report the backend's maintained `node_count()` and
`edge_count()`, which are O(1); no graph traversal is performed for
telemetry. `/v1/stats` is not used.

**Publication boundary.** The gauges are published once per job, not per
operation, by the blocking worker while it still holds the graph lock, at
the graph-work boundary below. This is not necessarily after the whole
worker has completed: the worker still releases the lock, runs any
post-lock steps and drops its guards afterwards.

| Path | Published | Not published |
| --- | --- | --- |
| Construction (`Service::with_config`) | From the supplied graph, including a nonempty embedded graph. | — |
| `Service::run` (single operations, legacy batches, feeder session and heartbeat) | After the job's work returns, for state-changing jobs. Includes the successful operations of a batch that contained domain errors. Session and heartbeat calls run as state-changing jobs and republish unchanged counts, which is not observable. | Read-only jobs (`stats`, query-only requests): counts cannot change. |
| `execute_feeder_worker` (events, gap, barrier) | After the operation returns, for mutating feeder calls (events, gap), whether it succeeded, partially failed or was rejected. A partial failure publishes its applied prefix. | Barrier calls: no graph change. |
| `Service::import` | At atomic publication, after the candidate replaced the live graph. | Any import rejected before publication (parse, limits, fencing, precondition, mode, unavailable): its candidate counts are never published. |

Consequences:
- No-ops, duplicates and read-only work leave the values unchanged. A
  republication of identical counts is not observable.
- During a batch or event request the gauges keep the previous job's values
  until the job's graph work returns. During an import they keep the previous
  live graph's values until publication; the off-lock candidate is never
  visible.
- Publication runs inside the blocking worker, so it happens even when the
  HTTP caller timed out or was cancelled.
- Every publication happens under the graph lock, so publications follow
  graph-lock order and an older job cannot overwrite a newer job's counts.

**Failure.** The gauges always retain the last published counts.
- A worker that panics before its publication point publishes nothing; the
  previous counts remain.
- A worker that panics after publishing (for example after releasing the
  graph lock) does not roll the published counts back.
- After a worker failure the gauges therefore do not guarantee the current
  state of the failed service's graph. `knotrel_service_unavailable`
  reports the failure.

**Coherence.** Nodes and edges are two separate atomic stores read
independently at scrape time, without the graph lock. A scrape that overlaps
a publication may pair one job's node count with the next job's edge count.
They are not a coherent version tuple with each other, nor with readiness,
outstanding jobs, `/v1/stats` or any other independently sampled metric.

**Scraping.** A scrape reads the two gauges like other registered metrics.
It takes no graph lock, admission permit or import permit, and does not wait
for a worker that holds the graph lock.

**Ownership and disabled mode.** The gauges are fields of the per-`Service`
`ServiceTelemetry` (section 8), registered by `Metrics`. There is no new
reference to `Service` or the registry, and no process-global state. When
metrics are disabled, `Service` holds `None`; nothing is registered,
allocated or published.

**Series.** 2 more series. The maximum became **1249 standalone, 1254
source-managed** (now 1414 and 1834; section 16).

**Tests.** In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)
(module `graph_counts`) and `service.rs` (module `tests::graph_counts`):
- a fresh empty server in both modes and an embedded nonempty graph;
- effective `add_node`, `link` and `cut`, repeated no-ops, a query and
  `/v1/stats`;
- a legacy batch with a domain error between successful operations;
- feeder events with progress, an exact duplicate and a partial failure;
- a rejected replacement import (precondition failure) preserving the live
  counts, then a published replacement;
- worker completion after caller timeout and after caller cancellation;
- independent server instances and disabled telemetry;
- a scrape that completes while a real `Service::run` worker holds the graph
  lock, checked with `try_lock` before and after the scrape.

## 11. Fourth M6.2 increment: graph-job queue, lock-wait and execution histograms (released in v0.4.0)

**Series.** Three histograms in seconds, each labelled by `kind`:
`knotrel_graph_job_queue_seconds`, `knotrel_graph_lock_wait_seconds` and
`knotrel_graph_job_execution_seconds`. `kind` is fixed by the code path that
creates the job (`metrics::JobKind`), never by request content:

| `kind` | Request | Worker |
| --- | --- | --- |
| `operation` | `POST /v1/operations` | `Service::run` (`execute_operation`) |
| `batch` | `POST /v1/batch` | `Service::run` (`execute`) |
| `stats` | `GET /v1/stats` | `Service::run` |
| `feeder_session`, `feeder_heartbeat` | `POST /v1/feeder/session`, `/heartbeat` | `Service::run` |
| `feeder_events`, `feeder_gap`, `feeder_barrier` | `POST /v1/feeder/events`, `/gap`, `/barrier` | `Service::execute_feeder_worker` |
| `import_publish` | `POST /v1/feeder/import` | `Service::import`, publication lock only |
| `inspect` | `GET /v1/nodes`, `/v1/nodes/{node}`, `/v1/nodes/{node}/edges`, `/v1/edges` | `Service::run_read`, shared lock (section 20) |

Series exist only for kinds reachable in the configured mode. In standalone
mode the feeder and import endpoints refuse with `mode_conflict` before
admission, so only `operation`, `batch` and `stats` are created; every kind
exists in source-managed mode.

**Boundaries.**

| Phase | Starts | Ends | Recorded |
| --- | --- | --- | --- |
| Queue | In the caller, immediately before `spawn_blocking`, after admission | First statement of the worker closure | By the worker at that point; no graph lock held |
| Lock wait | In the worker, immediately before it requests the graph lock | Lock acquired | By the worker, after it releases the lock |
| Execution | Lock acquired | Lock released, on normal release, early return or unwinding | By the worker, immediately after release |

Execution covers everything inside the critical section: the
failed/unavailable check, validation, graph work, the readiness-word store,
graph-count publication and, for imports, session and lease fencing, mode,
precondition and generation checks, the atomic swap and response
construction. Durations are measured at these boundaries, but the
histograms are updated only after the lock is released, so their internal
mutexes are never taken inside the graph critical section. As a
consequence, while a worker holds the lock a scrape already shows its queue
observation but not yet its lock-wait or execution observation.

**Not a decomposition of HTTP latency.** The three intervals cover only
worker-side graph-job time. They exclude:
- HTTP middleware, body reading, JSON parsing and response serialization;
- the async admission checks and the caller's wake-up after the worker
  finishes;
- worker steps outside the lock: before the lock request (test hooks; for
  imports, JSON parsing and off-lock candidate construction) and after
  release (post-lock steps and guard drops);
- for imports, the import-permit acquisition, the body read and the caller's
  brief pre-spawn graph-lock read that decides whether the import is the
  initial load. Import `receive` and `prepare` are timed separately by
  `import_duration_seconds` (section 15).

A caller that timed out stops its HTTP duration while the worker continues,
so HTTP duration can be shorter than the sum of the phases.

**Rejection, cancellation and failure.** Only phases a worker actually
reaches are recorded, each at most once. A phase that is not reached is
absent, never recorded as zero.

| Situation | Queue | Lock wait | Execution |
| --- | --- | --- | --- |
| Admission refused (`busy`, `import_capacity`, `shutting_down`, entry `unavailable`) | — | — | — |
| Worker never starts (runtime shut down before the blocking pool runs it) | — | — | — |
| Caller timed out or cancelled; worker completes | ✓ | ✓ | ✓ |
| Panic before the lock request | ✓ | — | — |
| Import rejected before the lock (parse, `instance_id`, `graph_id`, candidate limits) | ✓ | — | — |
| Rejected under the lock (failed graph, fencing, precondition, mode, generation) | ✓ | ✓ | ✓ |
| Panic under the lock | ✓ | ✓ | ✓ (until unwinding releases the lock) |
| Panic after release | ✓ | ✓ | ✓ |

Once every worker has finished, each kind therefore satisfies
`queue _count ≥ lock-wait _count = execution _count`.
Admission-refused requests are counted only by
`requests_rejected_total` (section 8).

**Coherence.** The histograms are sampled independently at scrape time.
They form no atomic tuple with each other (see above), with HTTP durations,
outstanding jobs, readiness, graph counts or any other metric. Observations
of a job become visible when they are recorded, not when its HTTP response
is sent.

**Buckets.** Upper bounds 10 µs, 50 µs, 100 µs, 500 µs, 1 ms, 5 ms, 10 ms,
50 ms, 100 ms, 500 ms, 1 s, 5 s, 10 s and 30 s, plus `+Inf`. Two steps per
decade starting below the HTTP buckets' 0.5 ms floor, because uncontended
acquisition and small critical sections are typically microseconds; the top
matches the 30 s default request timeout. The buckets resolve orders of
magnitude; they do not define or imply a service-level objective.

**Ownership and disabled mode.** The families and per-kind histogram
handles are fields of the per-`Service` `ServiceTelemetry` (section 8),
created at construction and registered by `Metrics`. A `JobClock` that
holds an `Arc<ServiceTelemetry>` moves into each worker. Telemetry holds no
reference to `Service`, the registry or workers, so there is no cycle and no
process-global state. When metrics are disabled, `Service` holds `None`: no
`JobClock` is created, no clock is read, no histogram is updated and nothing
is allocated; the lock helper only wraps the guard.

**Series.** Each histogram series has 14 buckets, `+Inf`, `_sum` and
`_count`, which is 17 series:
- standalone: 3 families × 3 kinds × 17 = 153;
- source-managed: 3 × 9 × 17 = 459.

The maximum becomes **1402 standalone, 1713 source-managed** (now 1414 and 1834; section 16). The test
`series_exist_only_for_kinds_reachable_in_the_mode` checks the per-mode
timing series counts against actual exposition.

**Tests.** In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)
(module `job_timing`):
- exact per-kind counts for operation, batch, stats and every feeder kind;
- import success, rejection before the lock (queue only) and under it;
- admission rejections (`busy`, then entry `unavailable`) adding no
  observations;
- completion after caller timeout and after cancellation, observed once;
- panic before the lock (queue only) and after release (all phases);
- independent instances and the per-mode series set.

In `service.rs` (module `tests::job_timing`), a `cfg(test)` timing clock
(separate from the lease clock) is advanced at one controlled point per test
to prove each boundary:
- work under the lock and panicking work count as execution;
- a pause after release is excluded from execution;
- a pause in the pre-lock hook is excluded from both queue and lock wait;
- a pause between the lock-wait start and the lock request, with the lock
  held elsewhere, counts as lock wait;
- with one blocking thread, a job queued behind a paused job counts the
  wait as queue;
- a rejection under the lock records all phases, and admission rejection none;
- a scrape completes while a worker holds the lock, before that worker's
  lock-wait and execution observations exist.

The lock-wait proof uses a new hook, `Lifecycle::pause_next_lock_request`,
which sits inside the lock-wait interval by design. Like the other worker
hooks, it exists only with the `test-support` feature: default builds have
no such method, and its internal call site compiles to an empty inline
function. Dropping its handle releases the worker.

**Overhead.** Not qualified. Section 6 applies unchanged.

## 12. Fifth M6.2 increment: graph-operation and feeder-event outcome counters (released in v0.4.0)

**Series.** Two counters, with pre-created series and fixed labels:
`knotrel_graph_operations_total{op,outcome}` and
`knotrel_feeder_events_total{result}`. The unit is one operation or one
event, never a request. Labels come from the operation variant and the
evaluated outcome (`metrics::GraphOp`, `metrics::FeederEventResult`), never
from node IDs, event IDs or caller text.

**Accounting.** All counts are taken by the worker, under the graph lock,
when an operation or event outcome is known:

| Population | Counted when | Value |
| --- | --- | --- |
| Direct operation (`/v1/operations`, each `/v1/batch` entry) | `GraphState::apply` returns, in `Service::execute_with` | `add_node`, `link`, `cut`: `changed` or `unchanged` when applied, `error` for a domain error. `connected`: `true`, `false` or `error`. |
| Feeder event (`/v1/feeder/events`) | the event loop in `GraphState::apply_events` reaches a decision for that event | `changed`: applied and changed the graph. `unchanged`: applied no-op mutation or `progress` event. `duplicate`: exact retained duplicate, skipped. `rejected`: the event at which evaluation stopped the request (`fail_batch_or_error`, or the defensive `SequenceExhausted`). |

Zero counts:

| Refused before evaluation | Where |
| --- | --- |
| Malformed body, invalid node ID, empty or oversized batch | HTTP handler, before admission |
| Direct mutation in source-managed mode (`direct_mutation_disabled`) | HTTP handler, before admission |
| Admission rejection (`busy`, `unavailable`, `shutting_down`) | admission; counted by `requests_rejected_total` |
| Source-managed synchronization gate (`uninitialized`, `loading`, `desynchronized`) | `execute_with`, before any entry is applied; every entry fails without evaluation |
| Event request checks before the loop: empty or oversized, session and lease, `graph_uninitialized`, `stale_generation`, `graph_desynchronized`, source and epoch mismatch, event-field validation (`invalid_request`) | `GraphState::apply_events`, before the loop |
| Events after the first rejected event | never reached |
| Gap, barrier, session, heartbeat, stats and import | not operations or events |

The same HTTP status can therefore count differently. A `409` for an
out-of-order sequence comes from the loop and counts one `rejected` event. A
`409 stale_generation` comes from the checks before it and counts nothing.

**Separation.** Feeder events mutate the graph through `CanonicalOp` inside
`apply_events`, never through `GraphState::apply`. They therefore cannot reach the
operation counter. Import publication swaps the prepared graph and calls
neither path. Counts are never inferred from revisions, response summaries or
HTTP completion.

**Partial success.** A batch counts every entry, including entries after a
domain error (continue-after-error is unchanged). A partial feeder failure
counts its applied prefix by result, the failing event as `rejected`, and
the untouched suffix not at all.

**Cancellation and failure.** Counting happens in the worker, so a worker
that finishes after its caller timed out or was cancelled still counts every
outcome once. A panic keeps the outcomes already counted. The interrupted
operation or event gets no outcome, because its effect is unknown.

**Coherence.** Each increment is an independent atomic. A scrape during a
job can see part of that job's outcomes. Counters do not form a coherent
tuple with revisions, graph gauges or other series.

**Disabled mode and ownership.** Without telemetry, the worker clones a `None`,
calls `GraphState::apply` directly and does not count. Nothing is allocated,
and the event loop does no counting work. Counters belong to the
per-instance `ServiceTelemetry`, which holds no reference to the service, so
there is no ownership cycle.

**Series.**

| Mode | Operations | Events |
| --- | --- | --- |
| standalone | 12: three mutations × {`changed`, `unchanged`, `error`}, plus `connected` × {`true`, `false`, `error`} | 0: family not registered (feeder endpoints refuse with `mode_conflict`) |
| source-managed | 3: `connected` × {`true`, `false`, `error`}; direct mutations are refused before execution | 4 |

With lifecycle support these become 27 standalone and 9 source-managed
(section 19).

The maximum becomes **1414 standalone, 1720 source-managed** (now 1414 and 1834; section 16). The test
`series_exist_at_zero_only_for_reachable_combinations` checks these sets
against actual exposition.

**Tests.** In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)
(module `outcome_counters`):
- per-mode series sets at zero;
- every operation and outcome combination, including a batch with entries
  before and after a domain error;
- zero counts for malformed requests, invalid nodes, empty batches, the
  source-managed gate and refused direct mutations;
- feeder changed and no-op mutations, `progress`, exact duplicates, a partial
  failure with an untouched suffix, in-loop rejections versus checks before
  the loop, and no operation counts from events;
- import publication counting nothing;
- independent instances.

With `test-support`, further tests cover:
- admission rejection;
- completion after caller timeout and after cancellation;
- a worker panic after counting.

`service.rs` (`tests::graph_counts`) checks that disabled telemetry counts
nothing.

**Overhead.** Not qualified. Section 6 applies unchanged.

## 13. Sixth M6.2 increment: feeder rejection and desynchronization counters (released in v0.4.0)

**Series.** Two counters, registered only in source-managed mode, with
pre-created series and fixed labels:
`knotrel_feeder_rejections_total{code}` and
`knotrel_desynchronizations_total{cause}`. Labels come from
`metrics::FeederRejection` and `metrics::Desynchronization`, never from
session IDs, cursors, generations, event IDs, caller text or error messages.

**Endpoint scope.** Session, heartbeat, events, gap and barrier. Import is
excluded: its terminal results belong to `imports_total` (section 15), whose
taxonomy (`payload_rejected`, `publication_rejected`, `worker_failed`)
differs.

**Rejection accounting.** One count per rejected request, where the typed
`FeederError` is produced, so it is never counted again as it propagates
through `Service` and the HTTP adapter:

| Request outcome | Counted at | Code |
| --- | --- | --- |
| Events, gap or barrier job returns `Err` | `Service::execute_feeder_worker`, in the worker under the graph lock, as soon as the job returns | underlying code |
| Multi-event request stopped at a failing event (`partial_failure`, HTTP 409/422) | `GraphState::fail_batch_or_error`, batch branch | underlying code (`event_failed` for any domain error), never `partial_failure` |
| Session or heartbeat state check returns `Err` | the worker closure in `Service::register_session` or `Service::heartbeat` | underlying code |
| Lease duration outside 1000–60000 ms | `Service::register_session`, before admission | `invalid_lease_duration` |

`fail_batch_or_error` counts only its batch branch. In the single-event
branch it returns `Err`, which the worker counts instead. Each request is
therefore counted once.

Not counted:

| Refusal | Where | Counted instead |
| --- | --- | --- |
| Admission `busy` (HTTP `503 import_in_progress`) and `unavailable`, including the worker's own pre-job check and the barrier's `failed` check | admission or worker | `requests_rejected_total` |
| Request timeout, cancellation, worker panic | lifecycle, worker join | `requests_outcome_uncertain_total` |
| `mode_conflict`, missing or invalid instance header (`invalid_instance_id`), header or body `stale_instance_id`, `invalid_graph_id`, malformed or unprocessable JSON | HTTP adapter, before the service is called; malformed bodies never become a typed `FeederError`. `Service::register_session` and `Service::heartbeat` also return `ModeMismatch` and `StaleInstanceId` from their entry checks (as `Service::import` does with `StaleInstanceId` and `InvalidGraphId`); these are not counted either | `http_requests_total` only |
| Import, including its fencing and precondition failures | import path | `imports_total` (section 15) |

`FeederRejection::of` is total over `FeederError`. Mode and request-identity
variants (`ModeMismatch`, `StaleInstanceId`, `InvalidInstanceId`,
`InvalidGraphId`), whether returned by the HTTP adapter or by a service entry
check, import-only variants and admission variants map to `None`. The unit test
`api::tests::feeder_rejection_labels_match_wire_codes` checks that every
counted label equals the response `code` that `api::feeder_error` produces,
except `Domain`, which collapses to `event_failed`.

**Desynchronization accounting.** One count per transition, by the helper
`desynchronize`, which counts only if the previous status was not
`desynchronized`:

| Cause | Sites |
| --- | --- |
| `source_epoch_mismatch` | `apply_events`, request epoch differs from the live source epoch |
| `source_predecessor_mismatch` | `apply_events` loop, `prev_delivery_sequence` or `prev_source_position` mismatch |
| `conflicting_duplicate` | `apply_events` loop: retained sequence with a different `event_id`, retained sequence with different content or position, or a new sequence reusing a retained `event_id` |
| `gap_declared` | `declare_gap` |

Out-of-order sequences and evicted (unverifiable) duplicates are rejected
without desynchronizing. Event and barrier requests on a desynchronized graph
fail with `graph_desynchronized`, a rejection, not a transition. A gap on a
desynchronized graph is acknowledged again, but its transition was already
counted, so it adds none. A replacement import restores `catching_up`; a later
genuine desynchronization counts again. Neither counter is reset by graph
replacement.

**Units.** `feeder_events_total{result="rejected"}` counts events;
`feeder_rejections_total` counts requests. A rejected single-event request
or a partial failure advances both, once each. A whole-request rejection
before the loop advances only the request counter. A desynchronizing event
in the loop advances `rejected`, the request rejection and the transition
counter; an epoch mismatch, checked before the loop, advances only the
request rejection and the transition counter.

**Cancellation and failure.** Every observation is made inside the worker
(or, for `invalid_lease_duration`, synchronously before admission), so a
worker that finishes after its caller timed out or was cancelled still
counts its rejection and transition once. A panic keeps observations already
made, including a transition counted before the panic. An interrupted job
gets no rejection, because its result is unknown.

**Disabled mode and ownership.** Without telemetry the helpers receive
`None` and do nothing: no counting, no allocation and no registry lookup.
The counters belong to the per-instance `ServiceTelemetry`, which holds no
reference to the service. They are pre-created arrays indexed by enum, so
counting is a single atomic increment.

**Series.**

| Mode | Rejections | Desynchronizations |
| --- | --- | --- |
| standalone | 0: family not registered (feeder endpoints refuse with `mode_conflict`) | 0 |
| source-managed | 19 (20 since M10, section 19) | 4 |

The maximum becomes **1414 standalone, 1743 source-managed** (now 1414 and 1834; section 16). The test
`feeder_rejections::series_exist_at_zero_only_in_source_managed_mode` checks
these sets against actual exposition.

**Tests.** In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)
(module `feeder_rejections`):
- per-mode series sets at zero;
- session and heartbeat rejections (`session_required`,
  `invalid_lease_duration`, `fenced_feeder_session`);
- whole-request event and barrier rejections, counting no events;
- partial and single-event domain failures and an out-of-order sequence
  inside a batch, each one request rejection by underlying code, with event
  outcomes unchanged;
- an unverifiable duplicate without a transition;
- each desynchronization cause, including both predecessor fields and all
  three conflicting-duplicate sites;
- repeated gap, events and barrier on a desynchronized graph without another
  transition;
- a replacement import followed by a new desynchronization, with cumulative
  counts;
- HTTP-adapter refusals and a rejected import counting nothing;
- independent instances.

With `test-support`, further tests cover:
- admission saturation that keeps `503 import_in_progress` and counts only
  `requests_rejected_total{reason="busy"}`;
- a rejection and transition counted after caller cancellation;
- a gap transition counted after caller timeout.

In `src/service.rs` (`tests::feeder_rejection_counts`): an expired lease (one
`lease_expired` rejection, no transition), and disabled telemetry with
unchanged gap and rejection behavior. `sequence_exhausted` is a defensive
variant that the u64 wire protocol cannot reach; it is covered only by the
classification test.

**Overhead.** Not qualified. Section 6 applies unchanged.

## 14. Seventh M6.2 increment: feeder lease remaining and last-applied age (released in v0.4.0)

**Series.** Two unlabelled gauges, emitted only in source-managed mode by the
scrape-time `HealthCollector`, and only once they have a value:
`knotrel_feeder_lease_remaining_seconds` and
`knotrel_feeder_last_applied_age_seconds`. Neither stores absolute time in a
label. Neither measures source-relative lag or proves synchronization:
only `/ready` (and `readiness_state`) says whether the graph is synchronized
(section 2).

**Clock.** Both use the monotonic lease clock (`service::now_to_millis`,
milliseconds since a process-local start instant), never wall-clock time.
Values are computed at scrape in signed milliseconds and exposed in seconds,
so their resolution is 1 ms. No timer task updates them.

**Lease remaining.**

| Aspect | Behavior |
| --- | --- |
| Source | `deadline_millis` of the published readiness word, which feeder and import workers republish from `GraphState::lease_deadline` under the graph lock; the initial-import `loading` store and the `/ready` staleness compare-and-swap carry the deadline over unchanged. No separate lease state. |
| Value | `(deadline_millis − now_millis) / 1000`, signed. |
| Boundary | Let `D` be the true deadline truncated to whole milliseconds since the start instant (`deadline_millis`) and `N` the scrape time truncated likewise (`now_millis`). **Positive** while `N < D`: the true time is before `D` ms, so no expiry check reports expiry. **`0`** for the whole deadline millisecond, `N = D` (exactly 1 ms of lease-clock time): `/ready` and `readiness_state` (which compare `N > D`) still treat the lease as live, while worker lease validation (`monotonic_now() > lease_deadline`, unrounded) reports `lease_expired` once the true deadline inside that millisecond has passed, for less than 1 ms before `N` reaches `D + 1`. `0` therefore means "deadline reached", not "still valid". **Negative** from `N = D + 1`: the true time is past the true deadline, so `/ready`, `readiness_state` and worker validation all report expiry. |
| Absent | Standalone mode; source-managed mode before the first successful session registration (published deadline `0`). Nothing clears a deadline afterwards, so the series stays once present. |
| Session and takeover | A successful registration (same session, higher epoch or takeover) publishes `now + lease_duration`; a refused one (for example `fenced_feeder_session`) leaves it unchanged. |
| Heartbeat | A successful heartbeat renews the deadline; a rejected one leaves it. |
| Import | Does not change the deadline, including on replacement. |
| Expiry | Keeps decreasing below zero. Scraping does not renew the lease, does not mark the graph `stale` and does not run the `/ready` compare-and-swap. |
| Unavailable, draining | The last published deadline is still reported. After a worker failure no feeder call can renew it, so it runs negative; it is an observation, not a statement that a feeder exists. |

**Last-applied age.**

| Aspect | Behavior |
| --- | --- |
| Reset by | A feeder events request that applied at least one event (`changed` or `unchanged`, including no-op mutations and `progress`), stamped **once per request** when `AppliedStamp` drops at the end of `GraphState::apply_events`, still under the graph lock (also on partial failure, early error and unwinding); and each successful import publication (`Service::import`, after the new generation is live). |
| Not reset by | Exact duplicates, rejected events, rejected requests, rejected or unpublished imports (the candidate is never stamped), declared gaps, barriers, session registration, heartbeats, `/v1/stats` and `connected` queries. |
| Partial success | A request that fails after an applied prefix is stamped when its evaluation ends (at the failing event), not at its last applied event. The difference is at most that request's remaining time under the lock. |
| Value | Time since the stamp. An idle source legitimately makes it large: it measures inactivity, not lag. |
| Absent | Standalone mode; source-managed mode before the first qualifying application. |
| Replacement | Reset by the published import; never removed afterwards. Counters are unaffected (sections 12 and 13). |

**Storage and ownership.** The age stamp is one `AtomicU64` in the
per-instance `ServiceTelemetry`, holding lease-clock milliseconds plus one
(`0` means none). Workers write it while holding the graph lock, so writes are
ordered; `fetch_max` also prevents an older stamp from replacing a newer one.
Lease remaining reuses the published readiness word and adds no storage.

**Scrape.** Both are read lock-free by `HealthCollector`: no graph lock,
admission permit or import permit. Each value is sampled independently, so it
is not coherent with `readiness_state`, the graph gauges or each other within
one scrape.

**Cancellation and failure.** Stamps and deadlines are written by the worker,
so a job that applies events or publishes an import after its caller timed
out or was cancelled still updates them. After a worker panic both keep their
last published values; they describe what workers last published, not the
current graph.

**Disabled mode.** Without metrics there is no `/metrics` route, no collector
and no `ServiceTelemetry`; the stamp helper receives `None` and reads no
clock.

**Series.**

| Mode | Lease remaining | Last-applied age |
| --- | --- | --- |
| standalone | 0 | 0 |
| source-managed | 0 or 1 | 0 or 1 |

The maximum becomes **1414 standalone, 1745 source-managed** (now 1414 and 1834; section 16).
`feeder_activity::series_appear_with_a_session_and_a_first_application`
checks that exactly these two series appear in actual exposition; it does not
verify the calculated total bound.

**Tests.** In `src/service.rs` (`tests::feeder_activity`), through the
production router, with the lease clock locked and reset on entry and drop:
- the gauge reading `0` throughout the deadline millisecond, with
  constructed instants on both sides of the unrounded deadline inside it
  (published readiness live on both sides, worker comparison expired after
  it), and `±0.001` either side;
- outside that millisecond, published deadlines whose positive and negative
  gauge readings agree with `/ready`, `Service::is_lease_expired`,
  `GraphState::is_lease_expired_internal` and `validate_session_and_lease`;
- lease after session, heartbeat renewal, takeover, fenced takeover and expiry,
  with repeated scrapes not renewing it;
- age reset by an import, an effective event, a no-op mutation, a `progress`
  event and the prefix of a partial failure, and not reset by an exact
  duplicate, out-of-order and domain-rejected events, a query, stats, a
  session, heartbeats, a barrier or a gap;
- rejected create-only and replacement imports leaving age unchanged, a
  published replacement resetting it without renewing the lease;
- age reset by a worker that applies after its caller was cancelled, or timed
  out, while a real job held the graph lock;
- a scrape completing while a worker holds the graph lock;
- standalone mode and disabled telemetry.

In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs) (module
`feeder_activity`): standalone absence, presence after a session and after the
first application, unlabelled `gauge` types with HELP text that disclaims
lag, and independent instances.

**Overhead.** See section 6 and the M6.5 qualification. With metrics
enabled, each feeder events request that applied an event adds one clock
read and one atomic `fetch_max` under the graph lock, not one per event.

## 15. Eighth M6.2 increment: import gauge, results and phase durations (released in v0.4.0)

**Series.** Source-managed mode only, the only mode whose router accepts
imports. All are pre-created at zero:

- `knotrel_import_in_progress`, an unlabelled gauge;
- `knotrel_imports_total{result}`, a counter;
- `knotrel_import_duration_seconds{phase}`, a histogram on the job-phase
  buckets (14 bounds from 10 µs to 30 s; a longer phase lands only in
  `+Inf`).

Labels come only from the enums `metrics::ImportResult` and
`metrics::ImportPhase`, never from identifiers, cursors, request bodies or
error text.

**Ownership.** `Service::acquire_import_permit` returns a
`service::ImportPermit` that carries the semaphore permit together with a
`metrics::ImportObservation` when import metrics exist. The HTTP handler owns
it while it reads the body; `Service::import` moves it into the worker
closure. The observation records everything when it is dropped, in this order:

1. the open phase, unless the worker is unwinding or the phase is an
   unfinished `receive`;
2. exactly one result;
3. the end of `in_progress`.

The observation is declared before the semaphore permit, so it is dropped
first. `in_progress` therefore returns to 0 before the next import can
acquire the permit, and it never exceeds 1. `Service::import` moves the
permit and the job guard into the closure as one `service::ImportJob`, whose
fields drop in declaration order whether the closure runs or is dropped
unrun: the import is recorded and its permit released before
`graph_jobs_outstanding` falls. Shutdown therefore cannot report the jobs
drained while an import is still being recorded or still holds its permit.
In the worker, the panic guard drops before the `ImportJob`, so a panic
marks the service unavailable before the import is recorded.

The final accounting runs in destructors. Process termination, or an abort
that skips unwinding, can prevent it; the counters then end with the
process.

**Lifecycle boundaries.**

| Boundary | Where | Effect |
| --- | --- | --- |
| Refused before the permit (mode, instance header, content type, `unavailable`, import capacity) | `api::feeder_import`, `acquire_import_permit` | Nothing. Capacity and `unavailable` are counted by `requests_rejected_total` (section 8). |
| Permit acquired | `acquire_import_permit` | `in_progress` 1; `receive` starts. |
| Body read returns | `api::feeder_import`, after `to_bytes` | `receive` observed. A failed read (size limit or body I/O error, both returned as `413 payload_too_large`) is `payload_rejected`, and the import ends. |
| Caller abandons the body read (request timeout, disconnect) | handler future dropped | `abandoned`; no `receive` observation; `in_progress` 0. |
| Worker closure dropped without running (blocking pool shut down) | `spawn_blocking` task cancelled | `abandoned`; no worker phase. The caller's existing `unavailable` handling of the join error is unchanged. |
| Worker starts | `Service::import` closure, after `JobClock::start` | The worker owns the import, and a panic from here is `worker_failed`. |
| `prepare` starts | after the worker start hook | An error from here is `payload_rejected`. |
| Graph-lock request | immediately before `lock_timed` | `prepare` observed; `publish` starts. An error from here is `publication_rejected`. |
| Publication | under the lock, where the panic and initial-load guards are marked successful | The result is `published`, even if a later step panics. |
| Worker ends | end of the closure: after the response is built and the lock released | The open phase is observed and the result is counted; then `in_progress` returns to 0, the permit is released and, last, the job count falls. |

**Results.** Classified by the phase the import reached, not by error code:

| Result | Produced by |
| --- | --- |
| `published` | The candidate became the live generation. |
| `payload_rejected` | Failed body read, `malformed_json`, `unprocessable_json`, body `stale_instance_id`, `invalid_graph_id` and candidate construction (`node_limit_exceeded`, `invalid_node`, `self_loop`, `edge_limit_exceeded`, `invalid_edge`). |
| `publication_rejected` | Anything after the lock request: `unavailable` found under the lock (also counted as `requests_rejected_total{reason="unavailable"}`), session and lease fencing, `graph_already_initialized`, `If-Match` precondition failures and generation exhaustion. |
| `worker_failed` | A worker panic before publication, including one in the worker start hook. It is also counted as `requests_outcome_uncertain_total{cause="worker_panic"}` (section 8). |
| `abandoned` | Ownership ended before a terminal result was established and before the worker started: the caller abandoned the body read (request timeout, client disconnect), or the worker closure was dropped without running. Never a worker outcome, and never a payload judgement. |

Each permitted import records exactly one of these five results. Once the
worker has started, a result other than `abandoned` is always established: a
panic before publication is `worker_failed`, and an import that published
stays `published` even if a later step panics.

Imports never count in `feeder_rejections_total` (section 13), graph
operations or feeder events (section 12).

**Caller timeout and cancellation.** Once the worker has started, it records
the result regardless of the caller. An import that publishes after its caller
timed out (HTTP `503 request_timeout`) or was cancelled is counted as
`published`, never `abandoned`. The HTTP response is not the import result,
and a caller timeout is not the end of the worker.

**Phases.** The three phases are disjoint, and they do not cover the whole
request:

| Phase | Start | End |
| --- | --- | --- |
| `receive` | permit acquired | `to_bytes` returns, successfully or not; includes waiting for the client's bytes |
| `prepare` | after the worker start hook | the graph-lock request, or the end of the worker on a preparation error |
| `publish` | the graph-lock request | the end of the worker, including lock wait, validation, swap, the publication of readiness, graph counts and age, response construction, lock release and the final drops |

The phases exclude:

- the handler's checks before the permit;
- the caller's pre-spawn graph-lock read and `loading` store;
- the blocking-pool queue, which is already in
  `graph_job_queue_seconds{kind="import_publish"}`;
- the worker start hook.

`publish` overlaps the `import_publish` lock-wait and execution histograms
(section 11). Those are separate metrics and must not be summed with it. A
failed phase is observed up to its error. A phase interrupted by a panic is
not observed, but phases that completed before the panic are kept; a panic
after publication therefore leaves `published` with no `publish`
observation. A phase that never began is never observed: an `abandoned`
import has at most a completed `receive` and no worker phase.

**Scrape and disabled mode.** The metrics live in the per-instance
`ServiceTelemetry`. In `prometheus-client` 0.26 the gauge and counters are
atomics, but each histogram keeps its state behind its own
`parking_lot::Mutex`, which both an observation and a scrape take briefly,
and each family's label map is behind a `parking_lot::RwLock`, which a scrape
takes for reading. Observations use handles pre-created at startup, so they
never take a family lock. These locks are the metrics' own: scraping takes no
graph lock, admission permit or import permit, and the import histograms are
observed only after the graph lock is released (section 11). In standalone
mode, and with metrics disabled, no `ImportObservation` is created, so no
clock is read and nothing is recorded; the permit only carries an empty
`Option`.

**Series.**

| Family | Series (source-managed) |
| --- | --- |
| `import_in_progress` | 1 |
| `imports_total` | 5 |
| `import_duration_seconds` | 3 × (14 buckets + `+Inf` + `_sum` + `_count`) = 51 |
| **Total** | **57** |

The maximum becomes **1414 standalone, 1802 source-managed** (1745 + 57).
`imports::series_exist_at_zero_only_in_source_managed_mode` checks these 57
series against actual exposition; the total bound remains a calculation.

**Tests.** In `src/service.rs` (`tests::import_metrics`), through the
production router, with the lease and job-phase clocks locked:

- create-only and replacement publication;
- payload rejections (malformed JSON, self-loop, stale body instance ID)
  timed through `prepare` without `publish`;
- an oversized body observed in `receive` only;
- publication rejections (`409`, `412`, a fenced session) timed through
  `publish`;
- a capacity refusal counting no import while the held permit shows
  `in_progress` 1, and the held permit, dropped unused, counted `abandoned`;
- `receive` absorbing a streamed body's wait, and an abandoned streamed body
  counted `abandoned` with `in_progress` 0 and no phase, followed by a
  published import;
- exactly one result per permitted import across published, both rejections
  and an abandoned body, with a capacity refusal counting none;
- a worker closure dropped unrun (blocking pool shut down) counted
  `abandoned`, with `in_progress` 0, the permit released and no outstanding
  job.

The following tests require `test-support`:

- `in_progress` 1 while the worker is paused at its start hook, which no
  phase absorbs;
- `publish` starting at the lock request;
- worker panics at the start hook and at the lock request (`worker_failed`,
  with only the completed phases kept);
- a worker panic after publication and lock release, which stays `published`;
- the import recorded, `in_progress` 0 and the permit released while the
  worker's job is still counted (the job count is held to block the job
  guard), so a drain cannot finish first;
- publication after caller cancellation and after a request timeout,
  counting `published`.

Standalone mode and disabled telemetry create no observation. In
[`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs) (module
`imports`):

- series presence, absence, label sets and bucket counts;
- types and HELP;
- one result per permitted import through HTTP, without
  `import_capacity` rejections;
- independent instances.

**Overhead.** Not qualified. Section 6 applies unchanged. With metrics
enabled, the import observation itself (`ImportObservation`) reads the
monotonic clock at most six times per permitted import. A successful HTTP
import, and a publication rejection, read it six times: once at permit
acquisition, once when the body read returns, once when `prepare` starts,
twice at the graph-lock request (ending `prepare`, starting `publish`) and
once when the import is recorded. Earlier endings read it less often: two
reads for a failed body read or a worker closure dropped unrun, four for a
preparation error, one for an abandoned body read. The import also adds one
gauge increment and decrement, one counter increment and at most three
histogram observations, none in a per-node or per-edge loop. These counts
exclude other instrumentation on the same request, such as the HTTP request
metrics and the `import_publish` graph-job histograms (section 11).

## 16. Ninth M6.2 increment: catch-up duration (released in v0.4.0)

Adds the remaining essential (`E`) metric from section 4 in source-managed
mode:

- `knotrel_catchup_duration_seconds{kind}` (histogram, seconds; `kind` ∈
  {`initial`, `replacement`}).

**Interval boundaries and ownership.** The open interval (`CatchupInterval`) is
owned by the authoritative `GraphState` under the graph mutex, avoiding any
parallel state machine:

- **Start:** Under the graph lock at import publication in `Service::import`,
  when the generation advances and `sync_status` transitions to `catching_up`.
  `create_only` starts `kind="initial"`; `replace` starts `kind="replacement"`.
- **Completion:** Under the graph lock in `GraphState::declare_barrier`, on the
  first `synchronized` barrier (B2) of that generation (transitioning
  `catching_up` or `stale` to `ready`). The interval is taken from `GraphState`,
  its elapsed monotonic duration (`timing_now().saturating_duration_since(...)`)
  is computed, and the `CompletedCatchup` value is handed to a worker-owned
  `CatchupRecorder` declared before `lock_timed`.
- **Off-lock observation:** `CatchupRecorder` observes the histogram on drop,
  after the graph lock is released (even if the worker unwinds after
  completing the barrier). Scraping takes no graph lock or admission/import
  permits; histograms use `prometheus-client`'s per-histogram `parking_lot::Mutex`
  and pre-created handles.

**Incomplete, interrupted and non-completing transitions.**

- **Rejected and repeated barriers:** Any barrier that fails validation, is
  premature/superseded/fenced, or returns `already_synchronized` (B1) leaves
  `catchup` untouched and adds no observation.
- **Events and heartbeats:** Applied events, progress events and heartbeats do
  not complete a catch-up interval.
- **Replacement before barrier:** A `replace` import overwrites `state.catchup`
  with the new generation's interval; the superseded generation's unfinished
  interval is dropped unobserved and cannot be completed later (old-generation
  barriers are fenced by `generation`).
- **Desynchronization:** Transitioning to `desynchronized` (`declare_gap` or
  conflicting event) prevents any barrier from succeeding in that generation;
  the unfinished interval remains unobserved until a `replace` import starts a
  fresh generation.
- **Lease expiry and takeover:** Entering `stale` during an open catch-up
  interval does not discard it. If the feeder acquires a newer session epoch
  and completes the first `synchronized` barrier for that generation, the
  recorded duration includes the time spent `stale`. By contrast, recovering from
  `stale` *after* a generation has already completed its catch-up finds no open
  interval and adds no observation.
- **Caller timeout/cancellation and worker panic:** Once the barrier worker has
  started, it owns completion regardless of caller timeout or cancellation. A
  worker panic before completing the barrier leaves the interval unobserved; a
  panic after `declare_barrier` completes the interval still records it via
  `CatchupRecorder::drop`.

**Not source lag or ongoing freshness.** `knotrel_catchup_duration_seconds`
measures elapsed monotonic time on this server from import publication to the
first verified barrier of that generation. It does not measure how far the
graph lagged behind the external source, and a completed barrier does not imply
ongoing freshness after that instant (contrast with `knotrel_feeder_lease_remaining_seconds`
and `knotrel_feeder_last_applied_age_seconds` in section 14).

**Buckets and limitations.** Uses `CATCHUP_BUCKETS` (13 explicit upper bounds
in seconds plus `+Inf`): `[0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0,
300.0, 600.0, 1800.0, 3600.0]`. These span 10 ms to 1 hour to resolve orders of
magnitude rather than SLOs; intervals exceeding 1 hour fall into `+Inf`.

**Series and cardinality.**

| Family | Series (source-managed) |
| --- | --- |
| `knotrel_catchup_duration_seconds` | 2 × (13 buckets + `+Inf` + `_sum` + `_count`) = 32 |
| **Total** | **32** |

The revised calculated maximum across all increments is **1414 standalone, 1834
source-managed** (`1802 + 32`). `catchup::series_exist_at_zero_only_in_source_managed_mode`
checks these 32 series against actual exposition; the total bound remains a
calculation.

**Disabled mode and standalone.** Standalone mode and disabled telemetry
(`KNOTREL_METRICS_ENABLED=false`) construct no `CatchupMetrics`, so
`CatchupInterval::start` returns `None` immediately without reading any clock
or allocating.

**Overhead.** Not qualified. Section 6 applies unchanged. With source-managed
metrics enabled, each completed catch-up interval adds two monotonic clock
reads (one at import publication, one at barrier completion) and one off-lock
histogram observation.

**Tests.** In `src/service.rs` (`tests::import_metrics::catchup`, with controlled
`timing_now()` and lease clocks):

- zero-initialized series count (32) and histogram type/HELP;
- initial import through events, heartbeats, rejected barriers (`barrier_not_reached`,
  older sequence, stale generation) to first `synchronized` barrier (observed once)
  and repeated `already_synchronized` barrier (adds nothing);
- replacement after a completed catch-up (starts fresh `replacement` interval)
  and replacement of an uncompleted interval (abandons initial interval, fences
  old generation);
- desynchronization via `declare_gap` followed by rebuild;
- lease expiry and session-epoch takeover during catch-up (interval completes and
  includes stale duration) versus recovery from `stale` after completion (adds
  no observation);
- caller cancellation, caller request timeout, worker panic before barrier
  (observes nothing) and worker panic after barrier (preserves observation);
- standalone mode, disabled telemetry (no interval tracked, no clock read),
  independent instances and scraping while the graph lock is held.

In [`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs) (module
`catchup` and `exposition_is_valid_and_uses_only_bounded_labels`):

- bounded `kind` label validation (`initial`, `replacement`), series presence
  only in source-managed mode (32 series, 14 buckets per kind including `+Inf`),
  HELP/TYPE checks, HTTP initial + replacement end-to-end counts and isolated
  instances.

## 17. M6.3 structured logs (released in v0.4.0)

Logs answer "which one": they correlate a request with the worker that ran
it and with the graph coordinates it observed. They are an operational
interface, not a stable machine contract: field names below are kept stable
within a minor release, and new fields can be added.

**Format.** `LOG_FORMAT=json` selects one JSON object per line (the
`tracing-subscriber` JSON format, including the current `span` and its
parents in `spans`); any other value or none selects the human-readable
format, which prints the same span fields as a prefix. `RUST_LOG` selects the
filter (default `info`).

**Request span.** `api::logging_middleware` wraps every request in a span
named `request`:

| Field | Meaning |
| --- | --- |
| `request_id` | Unique within the process, increasing from 1. Not sent to clients and not a metric label; combine with `instance_id` (the `X-Knotrel-Instance-Id` response header and `/v1/info`) to correlate across processes. |
| `method`, `route` | HTTP method and registered path, or `unmatched`. |
| `generation`, `state_version`, `sync_status` | Recorded by `api::inject_metadata_headers` when the response carries metadata: the same values as the response headers. Absent on requests that fail before metadata exists. |

Exactly one line ends each request: `handled request` (`uri`, `status`,
`latency_us`) at `info`, or `probe` at `trace` for `/health`, `/ready` and
`/metrics`, or `request cancelled before response` (`latency_us`) at `info` if
the request future is dropped first. Unregistered paths are logged
sanitized.

**Workers.** `Service::run`, `Service::execute_feeder_worker` and
`Service::import` capture the request span and enter it on the blocking
thread, so every worker line carries the submitting request's `request_id`,
also after the caller has gone.

**Synchronization diagnostics.**

| Message | Level | Fields | Emitted |
| --- | --- | --- | --- |
| `synchronization state changed` | `warn` into `desynchronized` or `stale`, otherwise `info` | `job`, `from`, `to`, `from_generation`, `generation`, `source_position` (sanitized) | By the worker, under the graph lock, when a job changed the sync status or the generation (session, heartbeat, events, gap, barrier, import). |
| `graph published by import` | `info` | `mode`, `from_generation`, `generation`, `node_count`, `edge_count`, `source_id`, `source_position` (both sanitized) | At atomic import publication. |
| `feeder declared unrecoverable source gap; …` | `warn` | `failed_delivery_sequence`, `generation`, `reason` (sanitized) | `declare_gap`. |
| `feeder request rejected` | `info` | `code` (the `feeder_rejections_total` code) | Where a feeder rejection is produced; logged whether or not metrics are enabled. The error message is not logged. |
| `feeder lease expired; readiness is stale` | `warn` | `from`, `to` | Once per expiry, by the readiness check that publishes `stale`. |

Rejections before execution (`busy`, import capacity, unavailable), request
time limits, worker failures and drain progress keep their existing lines,
now inside the request span.

**Redaction.** Never logged: graph payloads (node IDs, edges, operations,
event bodies), feeder session IDs, error messages that quote caller input,
and panic payloads. Caller-supplied identifiers that operators need
(`source_id`, source positions, the gap `reason`, unregistered paths) are
sanitized by `logging::sanitize`: control characters are escaped and the
value is cut to 64 characters followed by `…`. Credentials do not exist in
the protocol; if added later they must not be logged.

**Cost.** With no subscriber or a filter above `info`, spans and events are
disabled and allocate nothing. At the default `info` filter each API request
creates one span and writes one line, as before plus the span fields;
synchronization lines occur only on transitions. Caller text is formatted
lazily, only when a line is written.

**Tests.** [`tests/logs.rs`](../crates/knotrel-server/tests/logs.rs) captures
JSON output of a source-managed bootstrap, events, barrier, gap, a rejected
events call, an unregistered path and a cancelled request. It checks one
completion or cancellation line per request with unique IDs, that worker
transition lines carry the submitting request's ID, the span metadata
fields, the import publication and rejection lines, warn level on
desynchronization, sanitized positions, reasons and paths, and that the
session ID, node IDs and raw multi-line caller text never appear. Unit tests
in `src/logging.rs` cover sanitization and transition detection.


## 18. M6.5 qualification: overhead and series count

**Overhead.** Campaign
[`metrics-overhead-20261115`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/metrics-overhead-20261115/README.md)
in `knotrel-benchmarks` (generated
[`REPORT.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/metrics-overhead-20261115/REPORT.md),
sha256 `f946f97a2d9fb948be6a198da72389259e825d94c47212794b1029aa8b50689e`;
[`analysis/summary.json`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/metrics-overhead-20261115/analysis/summary.json),
sha256 `0b760e055fb45ea08c092a76e7c73c4da4b687921b00b71a3ca78e22457e2dc8`).
One release build of the in-process router probe on the v0.4.0 candidate tree
(identity `6ec5f564…ae65c3c`), metrics on versus off on the same binary, with a
byte-identical A/A copy, 24 counterbalanced rounds, Apple M3. Budget of
section 6, approved before measurement.

| Scenario | Metrics on vs off, paired median [95% CI] | A/A bound | Verdict |
| --- | ---: | ---: | --- |
| Single `connected` | −12.92% [−14.09%, −9.08%] | ±4.28% | PASS |
| 8 concurrent clients, single `connected` | +0.73% [−0.26%, +1.58%] | ±1.53% | PASS |
| Batch, 1,024 `connected` | +1.28% [+0.60%, +1.59%] | ±0.65% | PASS |
| Batch, 1,024 alternating cut/link | +0.99% [+0.46%, +1.93%] | ±1.33% | PASS |
| Import, 100,000 nodes | −0.64% [−3.25%, +0.57%] | ±3.64% | PASS |
| Feeder batch, 1,024 events | +0.60% [−0.30%, +1.82%] | ±1.47% | PASS |

All six scenarios pass after the primary rounds; the extension did not run.
The single-request row is faster with metrics on, which no code path
explains; it is the first, shortest scenario in each process and most exposed
to start-up effects. It is not a speedup claim. Logs are disabled in the probe
(no subscriber); their cost is part of the v0.4.0 regression qualification
against v0.3.0.

**Series count.** `series_count_is_constant_under_sustained_churn`
([`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs)) runs 20
cycles of feeder session takeovers with rising epochs, replacement imports
with fresh node IDs and source epochs, applied, duplicate, out-of-order and
failing events, barriers, a declared gap every third cycle, queries and
unregistered paths. After the first full cycle pattern the exposition's
sample count is identical in every later cycle and stays below the
calculated maximum of 1,834 source-managed series. Together with invariant 3
(compile-time label sets), this qualifies the series budget.

**Status.** M6.5 is accepted on this evidence. With M6.1 to M6.4, M6 was
accepted by the owner on 2026-11-15.

## 19. M10 node and edge lifecycle metrics

**Scope.** Metrics added by node and edge lifecycle support
([topology-lifecycle.md](topology-lifecycle.md), v0.5.0). Without
`KNOTREL_TOPOLOGY_LIFECYCLE=true` nothing in this section exists except the
new `feeder_rejections_total` code, and `graph_nodes` and `graph_edges` keep
their meaning. No label carries a node ID, edge, source cursor or event ID.

**Operation counter.** `knotrel_graph_operations_total{op,outcome}` gains:

| `op` | `outcome` | Error series increments on |
| --- | --- | --- |
| `set_node_active` | `changed`, `unchanged`, `error` | `unknown_node` |
| `set_edge_enabled` | `changed`, `unchanged`, `error` | `self_loop`, `unknown_node`, `unknown_edge` |
| `remove_node` | `changed`, `unchanged`, `error` | never (`remove_node` cannot fail) |
| `node_state` | `registered`, `unregistered`, `error` | never (`node_state` cannot fail) |
| `edge_state` | `registered`, `unregistered`, `error` | `self_loop` |

The `error` series that never increment exist for a uniform label set.
Series exist only with lifecycle support, and the three mutations only in
standalone mode (`metrics::GraphOp::reachable_in`): source-managed mode
refuses them with `direct_mutation_disabled` before admission. Lifecycle
operations refused with `topology_lifecycle_disabled` (validation step 5a)
are never admitted and count nothing, like the other refusals of section 12.
Accounting, cancellation and coherence follow section 12 unchanged; feeder
lifecycle events never count here.

**Gauges.** `knotrel_graph_active_nodes`, `knotrel_graph_enabled_edges` and
`knotrel_graph_effective_edges`, unlabelled, registered only with lifecycle
support. `ServiceTelemetry::publish_graph_counts` sets them together with
`knotrel_graph_nodes` and `knotrel_graph_edges`, at the same publication
boundaries (section 10: construction, state-changing jobs, mutating feeder
calls, import publication), under the graph lock. Each count is maintained by
the core in `O(1)` per mutation and read in `O(1)`; no traversal is done for
telemetry. The five gauges are independent atomic stores, not a coherent
tuple, and keep the last published values after a worker failure, exactly as
in section 10. With lifecycle support, `knotrel_graph_nodes` and
`knotrel_graph_edges` report registered counts (active or inactive, enabled or
disabled, effective or not). With metrics disabled, `Service` holds no
telemetry and no count is read or published.

**Feeder rejections.** `knotrel_feeder_rejections_total` gains the code
`topology_lifecycle_disabled`, counted once per events request refused because
an event uses a lifecycle operation while lifecycle support is off. Like every
code of this closed set, its series exists at zero in source-managed mode
whatever the lifecycle setting. Imports refused for the same reason are not
counted here; `knotrel_imports_total` classifies them as `payload_rejected`
(section 15).

**Series.**

| Family | Standalone | Source-managed |
| --- | --- | --- |
| `graph_operations_total` without lifecycle support | 12 (unchanged) | 3 (unchanged) |
| `graph_operations_total` with lifecycle support | 9 ops × 3 = 27 | 3 queries × 3 = 9 |
| Lifecycle gauges (lifecycle support only) | 3 | 3 |
| `feeder_rejections_total` | 0 | 20 (was 19) |

The calculated maximum becomes:

| Lifecycle support | Standalone | Source-managed |
| --- | --- | --- |
| off | 1414 | 1835 (1834 + 1) |
| on | 1432 (1414 + 15 + 3) | 1844 (1835 + 6 + 3) |

The total remains a calculation. The M6.5 series-count qualification
(section 18) predates these additions.

**Tests.** In [`tests/topology.rs`](../crates/knotrel-server/tests/topology.rs),
`lifecycle_metrics_are_bounded_and_published_only_when_enabled` checks that
neither the gauges nor the lifecycle `op` values appear without lifecycle
support, and checks the published registered and lifecycle counts, the new
operation outcomes at their expected values and that no identifier appears as
a label value.

**Overhead.** Not qualified. Section 6 applies unchanged.

## 20. Inspection routes (console data browser; released in v0.8.0)

The read-only inspection routes (`docs/http-api.md`, "Inspection (opt-in)")
add no new metric family. They extend two existing label sets:

- `route` gains `/v1/nodes`, `/v1/nodes/{node}`, `/v1/nodes/{node}/edges` and
  `/v1/edges` (18 values instead of 14). The routes are always registered,
  answering 409 `inspection_disabled` when `KNOTREL_INSPECTION` is off, so the
  bound grows whether or not inspection is enabled: `http_requests_total` by
  4 × 10 × 7 = 280 and `http_request_duration_seconds` by 4 × 18 = 72.
- `kind="inspect"` is added to the three job-phase histograms (section 11)
  only when inspection is enabled, because the routes are refused before
  admission otherwise: 3 × 17 = 51 series.

The calculated maximum becomes:

| Lifecycle support | Inspection | Standalone | Source-managed |
| --- | --- | --- | --- |
| off | off | 1766 (1414 + 352) | 2187 (1835 + 352) |
| off | on | 1817 | 2238 |
| on | off | 1784 (1432 + 352) | 2196 (1844 + 352) |
| on | on | 1835 | 2247 |

Actual counts stay far below the bound, because most route and method
combinations can only produce `405`. The series-count qualification
(section 18) predates this change.

**Tests.** `tests/inspection.rs` covers the routes. The `kind` label values are
pinned by the unit tests of `metrics::JobKind`, and the route templates by
`api::Route`.

**Overhead.** Not qualified. The page cost and its interference with
concurrent work are measured in
[console inspection costs](console-inspection-costs.md).

## 21. Per-lane admission metrics (v1.0.0, unreleased)

**Problem (contract freeze review, finding M1).** Until v0.9.x,
`knotrel_graph_job_capacity` reported only the shared lane
(`max_pending_jobs`) while `knotrel_graph_jobs_outstanding` counted jobs of
every lane plus the import. In source-managed mode the ratio could exceed 1 by
the reserved feeder permits, the 2 control permits and the import in normal
operation, so `KnotrelQueueSaturated` could fire although no lane was full,
and `requests_rejected_total{reason="busy"}` did not say which lane refused.
Maintainer decision 7 (2026-11-22) adds a `lane` label so that numerator and
capacity always refer to the same lane.

**Lanes.** The `lane` label values are the server's admission semaphores
(`service::Lane`), a closed set frozen at 1.0:

| `lane` | Admits | Capacity (`graph_job_capacity`) | Modes |
| --- | --- | --- | --- |
| `shared` | queries, stats, inspection; every graph job in standalone mode | `KNOTREL_MAX_PENDING_JOBS` (`max_pending_jobs`) | both |
| `feeder_data` | `/v1/feeder/events`, `/v1/feeder/barrier`, `/v1/feeder/gap` | `KNOTREL_FEEDER_RESERVED_JOBS` | source-managed |
| `feeder_control` | `/v1/feeder/session`, `/v1/feeder/heartbeat` | 2 | source-managed |
| `import` | `/v1/feeder/import` (the single import permit) | 1 | source-managed |

**Semantics.**

- `knotrel_graph_jobs_outstanding{lane}` is `capacity − available permits` of
  the lane's semaphore, read with one atomic load per lane at scrape
  (`Service::lane_occupancy`). A permit is held from admission until the
  worker that owns it ends (success, error, panic, or after its caller timed
  out or was cancelled); the import permit from acquisition, before the body
  is read, until the import ends, so `{lane="import"}` matches
  `knotrel_import_in_progress`, except briefly at the end of an import:
  `ImportPermit` clears `in_progress` just before it releases the permit, so
  a scrape in between reads `in_progress` 0 with the import lane still 1.
- No permit is ever added to or removed from a lane semaphore, so for every
  lane `0 ≤ outstanding ≤ capacity` in every scrape, and
  `outstanding == capacity` exactly when the lane's next admission would be
  refused with `busy` (`import_capacity` for the import lane). Numerator and
  capacity of a lane are read in the same scrape and carry identical label
  sets, so PromQL's default one-to-one matching pairs them by `lane`.
- The sum over lanes is not the drain count: `Lifecycle::outstanding_jobs`
  counts tracked graph jobs (an import only once its body has been received),
  and a finishing worker releases its permit just before its job guard. It stays internal (shutdown logs).
- `knotrel_requests_rejected_total{reason,lane}`: `busy` carries the lane
  whose permits were exhausted, `import_capacity` carries `lane="import"`,
  and `shutting_down` and `unavailable`, refused before or regardless of a
  lane, carry `lane="none"`. Only these combinations exist:

| `reason` | `lane` | Modes |
| --- | --- | --- |
| `busy` | `shared` | both |
| `busy` | `feeder_data` | source-managed |
| `busy` | `feeder_control` | source-managed |
| `import_capacity` | `import` | both (always 0 in standalone mode, kept from v0.4.0) |
| `shutting_down` | `none` | both |
| `unavailable` | `none` | both |

**Series count.** Fixed at compile time and independent of load:

| Family | Standalone | Source-managed |
| --- | ---: | ---: |
| `graph_jobs_outstanding{lane}` | 1 | 4 |
| `graph_job_capacity{lane}` | 1 | 4 |
| `requests_rejected_total{reason,lane}` | 4 | 6 |

Standalone mode is unchanged (one series per gauge, four rejection series);
source-managed mode gains 8 series. The calculated maximum of section 20
becomes:

| Lifecycle support | Inspection | Standalone | Source-managed |
| --- | --- | --- | --- |
| off | off | 1766 | 2195 (2187 + 8) |
| off | on | 1817 | 2246 |
| on | off | 1784 | 2204 |
| on | on | 1835 | 2255 |

`series_count_is_constant_under_sustained_churn` checks the churned exposition
against the source-managed bound without lifecycle support or inspection
(2195).

**Compatibility.** Adding a label to existing families changes their label
names, which [compatibility.md](compatibility.md) treats as breaking for
stable families. It is an approved exception of the 1.0 contract freeze
(decision 7), to be listed in the v1.0.0 release notes. Effect on existing
queries: in standalone mode each gauge keeps exactly one series; in
source-managed mode `outstanding / capacity` now yields one ratio per lane,
and a query that summed or selected the families without labels must
aggregate `by (lane)` or select a lane. Prometheus treats a missing label as
empty, so `{lane=""}` matches nothing from v1.0.0.

**Alerts.** `KnotrelQueueSaturated` divides the two gauges per lane, excluding
the import lane (full for the whole duration of every import), and
`KnotrelRequestsRejectedBusy` sums `busy` rejections `without (reason)`, keeping `lane` and the scrape target's labels
([alerting.md](alerting.md)).

**Tests** ([`tests/metrics.rs`](../crates/knotrel-server/tests/metrics.rs),
module `lanes`):

- `each_mode_exposes_its_lanes_with_their_capacities`: the lane set and
  capacities per mode;
- `saturation_ratio_per_lane_stays_within_one_and_reaches_one_only_when_full`:
  source-managed operation with reserved feeder permits in use and the shared
  lane full (the old ratio read 2/1): every lane's ratio stays at most 1 in
  every scrape and is 1 exactly on the lanes whose next admission is refused,
  each refusal counted as `busy` on its lane;
- `import_lane_is_full_only_while_an_import_holds_its_permit`;
- `alert_expressions_divide_and_count_by_lane`: parses the shipped
  `KnotrelQueueSaturated` expression, evaluates it against a real exposition
  with one-to-one matching on identical label sets, and checks that
  `KnotrelRequestsRejectedBusy` keeps `lane` and the target labels;
- the label-domain check of `exposition_is_valid_and_uses_only_bounded_labels`
  and the per-series checks of the `outcomes` module.

**Overhead.** Recording a rejection is still one atomic increment of a
pre-created counter. A scrape reads up to four semaphore permit counts
(atomic loads) instead of one atomic counter and encodes up to 8 gauge
samples instead of 2. No request path changes. The metrics overhead budget
of section 6 applies; the change is part of the v1.0.0 candidate measured by
the v1.0.0 performance campaign, and no overhead claim is made before that
measurement.
