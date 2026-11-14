# Observability contract

**Status:** Vocabulary for roadmap M6.1, plus six M6.2 increments
(sections 7, 8, 10, 11, 12 and 13), which are implemented but unreleased. M6 is deferred and is not
part of v0.3.0. Neither M6.2 nor M6 is complete. Rows marked *pending* in
section 4 are vocabulary only: no endpoint, metric or log field exists for
them yet. The opt-in `/metrics` endpoint does not change any existing route,
status code or body of the [HTTP API](http-api.md), the
[integration contract](integration-contract.md) or the
[deployment runbook](deployment.md).

Scope: service health, workload pressure and feeder synchronization of one
`knotrel-server` process. Out of scope: alert rules (M6.4), budgets (M6.5),
log-format changes (M6.3), persistence metrics, and anything requiring a
contention redesign (M5).

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
| Request log | `api::logging_middleware` | Exposed (tracing) | `method`, `uri` (path), `status`, `latency_us`. `info` level; probes (and `/metrics` when enabled) at `trace`. Outermost layer, so it sees `request_timeout`, `shutting_down` and handler responses. It has no drop hook: a request future that is dropped before a response exists emits **no** line. |
| Rejection and failure logs | `Service::run`, `Service::execute_feeder_worker`, `Service::acquire_import_permit`, `lifecycle::request_timeout`, `declare_gap`, worker `JoinError` handlers | Exposed (tracing) | Messages for capacity exhaustion, import capacity, request time limit, declared gap (`failed_delivery_sequence`, caller-supplied `reason`), worker panic and unavailable service. |
| Process lifecycle logs | [`main.rs`](../crates/knotrel-server/src/main.rs), [`serve.rs`](../crates/knotrel-server/src/serve.rs) | Exposed (tracing) | Startup configuration, shutdown requested, draining (`open_connections`, `outstanding_jobs`), shutdown deadline (`abandoned`), accept backoff, connection limit. |
| Outstanding worker jobs | `Lifecycle::outstanding_jobs`, `JobGuard` | Exposed via opt-in `/metrics` | Queued plus running graph jobs, including import workers. Otherwise logged only when draining. |
| Admission capacity | `Service` permit semaphore (`max_pending_jobs`), import semaphore (1) | Exposed via opt-in `/metrics` | Rejections are logged and counted by `requests_rejected_total` (section 8). |
| Lifecycle phase, worker failure | `Phase`, `unavailable` flag | Exposed via `/ready` and opt-in `/metrics` | `readiness_state` and `service_unavailable` gauges. |
| Synchronization state | `GraphState` (`generation`, `version`, `sync_status`, `applied_delivery_sequence`, `source_position`, session, epoch, `lease_deadline`) | Internal | Partly visible through `/v1/stats`, headers and `/ready`. |
| Event outcome counts | `FeederEventsResponse.events_applied` | Exposed per response; cumulative via opt-in `/metrics` | Fifth M6.2 increment; section 12. |
| Import preparation and catch-up timing | none | **Needs instrumentation** | Only whole-request server-side latency exists, in logs and (opt-in) `/metrics`. |
| HTTP counts, duration, backlog, capacity, unavailable flag, readiness, build, graph node and edge counts | `GET /metrics` (`metrics.rs`) | Exposed when `KNOTREL_METRICS_ENABLED=true` | M6.2 increments; sections 7, 8 and 10. |
| Graph-job queue, lock-wait and execution time | `JobClock` and `lock_timed` (`metrics.rs`), recorded by workers | Exposed when `KNOTREL_METRICS_ENABLED=true` | Fourth M6.2 increment; section 11. |
| Rejections before execution, uncertain-outcome observations | `ServiceTelemetry` (`metrics.rs`), owned by `Service` | Exposed when `KNOTREL_METRICS_ENABLED=true` | Second M6.2 increment; section 8. |
| Graph-operation and feeder-event outcomes | `ServiceTelemetry::count_operation`, `count_feeder_event` (`metrics.rs`), called by workers | Exposed when `KNOTREL_METRICS_ENABLED=true` | Fifth M6.2 increment; section 12. |
| Feeder rejections and desynchronizations | `ServiceTelemetry::count_feeder_rejection`, `count_desynchronization` (`metrics.rs`), called where the error or transition is produced | Exposed when `KNOTREL_METRICS_ENABLED=true` (source-managed mode) | Sixth M6.2 increment; section 13. |

### Concrete operator gaps

1. No cumulative counts for applied and duplicate events, gaps, conflicts and imports: they are visible only as individual log lines or response bodies. (Request counts by route and status class, pre-execution rejections by reason, and graph-operation and feeder-event outcomes are now available from the opt-in `/metrics`; section 12. Feeder rejections by code and desynchronizations by cause, including gaps and conflicts, are counted too; section 13. Imports remain uncounted.)
2. No backlog signal outside drain. (Now exported as `graph_jobs_outstanding` via the opt-in `/metrics`.)
3. Graph counts are readable only through `/v1/stats`, which competes for the same admission permits and graph lock as the work being diagnosed. (The opt-in `/metrics` now exposes worker-published counts without either; section 10.)
4. No breakdown of where time goes: caller latency cannot be split into queueing, lock wait and execution. (The opt-in `/metrics` now records worker queue, lock-wait and execution time per job kind; these intervals do not add up to HTTP latency; section 11.)
5. There is no record of how long bootstrap or replacement catch-up took.
6. A dropped request future leaves no request log line. (The opt-in `/metrics` endpoint now counts it as `cancelled`.)
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
- **lock wait:** from the start of `graph.lock()` until it is acquired;
- **execution:** time holding the graph lock.

Import preparation (parse and candidate construction) runs on the worker
before its lock request and belongs to none of the three; its own timing is
pending. Exact boundaries are in section 11.

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
| Import | import request that obtained the import permit | Counted once with a terminal result. |
| Rejected feeder request | session, heartbeat, events, gap or barrier request for which the service produced a feeder-protocol error | Once per request, under the underlying error code; a partial event failure is one rejection, not one per event and not `partial_failure` (section 13). Admission refusals, HTTP-adapter checks and imports are not feeder rejections. |
| Desynchronization | transition of the graph into `desynchronized` | Once per transition; a request on a graph already `desynchronized` adds none (section 13). |
| Rejected request | request refused before any graph job or import ran for it (`shutting_down` also covers requests that would not create one) | At most once, at the first refusing boundary (section 8). Partial batch or event failures are not rejections. |
| Uncertain-outcome observation | caller abandonment of, or worker panic in, an admitted state-changing job | Not deduplicated per request: a timeout followed by a worker panic is two observations (section 8). |

## 4. Minimum vocabulary

The "M6.2" column gives the proposed Prometheus name (prefix `knotrel_`).
`E` marks essential first delivery and `D` marks deferred. **Implemented**
means served by the first slice (section 7); **pending** means vocabulary
only. Every series is
per process and resets to zero on process restart, because Knotrel holds no
durable state. "Replacement" means publication of a new generation by import.

| Name (M6.2) | Type, unit | Operator question | Meaning and producing boundary | Labels (bounded) | On replacement | Today | M6.2 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `http_requests_total` | counter | What are callers sending and receiving? | One per request (section 3), recorded by `metrics::track_http` when the inner service returns a response, or by its drop guard as `cancelled` if the future is dropped first. Never both. | section 7 | continues | **implemented** | E |
| `http_request_duration_seconds` | histogram, s | How long does the server take to produce a response? | Server-side, from middleware entry to response construction (section 2). Includes `request_timeout` and `shutting_down` responses. Excludes body transmission, cancelled requests and worker time after the caller has gone. | `route` | continues | **implemented** | E |
| `requests_rejected_total` | counter | Is work refused before execution, and why? | One per request refused before its graph job or import runs, at the boundary that refuses it (section 8). At most one per request. | `reason` ∈ {`busy`, `import_capacity`, `shutting_down`, `unavailable`} | continues | **implemented** | E |
| `requests_outcome_uncertain_total` | counter | How often may a state change have happened without its caller learning the result? | One per **observation**, not per distinct request (section 8): a caller stopped waiting for an admitted state-changing job (`request_timeout`, `cancelled`), or such a job's worker panicked (`worker_panic`). One request can contribute a caller observation and later a `worker_panic`. `http_requests_total{status_class="cancelled"}` is unchanged and counts every dropped request, including probes and reads. | `cause` ∈ {`request_timeout`, `cancelled`, `worker_panic`} | continues | **implemented** | E |
| `graph_jobs_outstanding` | gauge, jobs | How deep is the backlog? | `Lifecycle::outstanding_jobs` read at scrape: queued plus running, including import, and including jobs whose caller timed out or was cancelled. Not bounded by `max_pending_jobs`: an import can add one, and a finishing worker releases its admission permit just before its `JobGuard` (drop order in `Service::run` and `execute_feeder_worker`), so a newly admitted job can briefly be counted alongside it. Do not alert on `outstanding / capacity > 1`. | none | unaffected | **implemented** | E |
| `graph_job_capacity` | gauge, jobs | What is the admission limit? | Graph-job admission permit limit (`max_pending_jobs`), constant after startup. Not a bound on `graph_jobs_outstanding`: imports and worker teardown can make outstanding jobs exceed it (see that row). | none | constant | **implemented** | E |
| `graph_job_queue_seconds`, `graph_lock_wait_seconds`, `graph_job_execution_seconds` | histograms, s | Is latency queueing, contention or work? | Section 2 split, recorded inside the worker closure, so it is observed even when the caller has gone. Exact boundaries in section 11. | `kind` ∈ {`operation`, `batch`, `stats`, `feeder_session`, `feeder_heartbeat`, `feeder_events`, `feeder_gap`, `feeder_barrier`, `import_publish`} | continues | **implemented** | E |
| `graph_operations_total` | counter | What mix of operations runs, and how many change the graph? | One per executed operation (section 3), counted by the worker when `GraphState::apply` returns, under the graph lock (section 12). Feeder events and imports are never counted here. | `op` ∈ {`add_node`, `link`, `cut`, `connected`}; `outcome` ∈ {`changed`, `unchanged`, `error`} for mutations, {`true`, `false`, `error`} for `connected` | continues | **implemented** | E |
| `graph_nodes`, `graph_edges` | gauges | How big is the graph? | Published by the worker, under the graph lock, when a state-changing job's graph work returns and at import publication; initialized at construction. Retains the last published values; not rolled back by a later worker panic. Read at scrape without the graph lock. Not a coherent pair (section 10). | none | jump to published import counts | **implemented** | E |
| `service_unavailable` | gauge 0/1 | Has a worker failure disabled the service? | The `unavailable` flag. Becomes 1 after a worker panic and stays 1 until process restart. | none | unaffected | **implemented** | E |
| `readiness_state` | gauge, one-hot | What would `/ready` say? | `lifecycle::readiness_state`: same precedence as `lifecycle::readiness`, computed read-only at scrape. Stale is derived from the published deadline, without the compare-and-swap. | `state` ∈ {`unavailable`, `draining`, `starting`, `ready`, and in source-managed mode `uninitialized`, `loading`, `catching_up`, `stale`, `desynchronized`} | follows state | **implemented** | E |
| `feeder_lease_remaining_seconds` | gauge, s | Is the feeder alive? | Published lease deadline minus now at scrape. Negative once expired. Absent with no session or in standalone mode. | none | session kept | pending | E |
| `feeder_events_total` | counter | Is the source making progress, and how much is redundant? | Per evaluated event (section 3) in `GraphState::apply_events`, under the graph lock (section 12). `unchanged` covers no-op mutations and `progress`. | `result` ∈ {`changed`, `unchanged`, `duplicate`, `rejected`} | continues | **implemented** | E |
| `feeder_rejections_total` | counter | Why did a feeder call fail? | One per session, heartbeat, events, gap or barrier request the service rejected, where the `FeederError` is produced (section 13). Labelled with the underlying error code, not `partial_failure`. An event domain error (any core code) collapses to `event_failed`. Excluded: admission `busy` (HTTP `import_in_progress`) and `unavailable` (counted in `requests_rejected_total`), timeouts, cancellations and panics (`requests_outcome_uncertain_total`), HTTP-adapter checks and malformed bodies, and imports (`imports_total`). Source-managed mode only. | `code` ∈ {`session_required`, `fenced_feeder_session`, `lease_expired`, `invalid_lease_duration`, `graph_uninitialized`, `graph_desynchronized`, `stale_generation`, `source_mismatch`, `source_epoch_mismatch`, `out_of_order_sequence`, `source_predecessor_mismatch`, `conflicting_duplicate`, `unverifiable_duplicate`, `sequence_exhausted`, `invalid_request`, `event_failed`, `barrier_position_mismatch`, `barrier_not_reached`, `superseded_barrier`} | continues | **implemented** | E |
| `desynchronizations_total` | counter | Why did the graph require a rebuild? | One per transition into `desynchronized`, at the sites in `GraphState::apply_events` and `declare_gap` that set it. A predecessor mismatch covers both `prev_delivery_sequence` and `prev_source_position`, which share one wire code. Out-of-order sequences and evicted duplicates do not desynchronize. | `cause` ∈ {`gap_declared`, `source_epoch_mismatch`, `source_predecessor_mismatch`, `conflicting_duplicate`}. Source-managed mode only (section 13). | continues | **implemented** | E |
| `feeder_last_applied_age_seconds` | gauge, s | When did the graph last move forward? | Now minus the monotonic instant of the last applied (non-duplicate) event or import publication. **Not lag** (section 2). | none | reset by publication | pending | E |
| `import_in_progress` | gauge 0/1 | Is an import running? | Import permit held. | none | — | pending | E |
| `imports_total` | counter | Do imports succeed? | One per permitted import, at its terminal result. | `result` ∈ {`published`, `payload_rejected` (size, parse, limits), `publication_rejected` (fencing, lease, precondition, mode, exhausted), `worker_failed`} | continues | pending | E |
| `import_duration_seconds` | histogram, s | Where does import time go? | `receive` (body read after permit), `prepare` (parse and build, off-lock), `publish` (lock wait, validation, swap). Observed even when publication is rejected. | `phase` ∈ {`receive`, `prepare`, `publish`} | continues | pending | E |
| `catchup_duration_seconds` | histogram, s | How long did bootstrap or replacement take to become ready? | From import publication (entering `catching_up`) to the first `synchronized` barrier (B2) in that generation. Abandoned if the generation is replaced or desynchronized first. | `kind` ∈ {`initial`, `replacement`} | new interval starts | pending | E |
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

**Remaining essential (`E`, pending):** lease remaining, last-applied age, import gauge/counter/durations
and catch-up duration.

**Instrumentation locations for the remaining rows:**

| Signal | Location |
| --- | --- |
| Operations | `GraphState::apply`, called per operation for single and batch requests. |
| Imports | `api::feeder_import` and `Service::import`. |
| Catch-up | Start instant stored in `GraphState` at publication; observed in `declare_barrier` B2. |
| Last-applied instant | Atomics updated by the worker under the lock (as graph counts are; section 10). |
| `/metrics` reads | Atomics, the published readiness word, `outstanding_jobs` (a `watch` channel) and the registry. No graph lock, admission permit or import permit; registry and `watch` synchronization still applies. |

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

**Overhead qualification: pending.** No overhead budget has been approved,
and the v0.3.0 regression-qualification tolerances are **not** an
automatically approved instrumentation budget. A proposed approach, to be
approved before running:

- paired runs of one release build with metrics disabled versus enabled, on
  the existing qualification cells with the balanced collector;
- a budget and stopping rules agreed in advance;
- a series-count check after repeated imports and session takeovers.

No campaign has been run.

## 7. First M6.2 slice (implemented, unreleased)

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
`Cache-Control: no-store`. A scrape reads atomics, the published readiness
word, the outstanding-jobs `watch` channel and the in-memory registry. It
takes no graph lock and no admission or import permit. It is not lock-free:
it uses synchronization internal to the `prometheus-client` registry (the
metric-family maps and histogram state, which request accounting also
updates) and to the outstanding-job `watch` channel. The readiness gauge
itself is computed from atomics and the clock, like `/ready`. While draining it is
served like the probes, with `Connection: close`.

**Middleware order** (outermost first): request logging, `metrics::track_http`,
`lifecycle::shutdown_gate`, `lifecycle::request_timeout`, handler. HTTP
metrics therefore see `shutting_down` and `request_timeout` responses.

**Labels.**

- `route`: the matched router template — `/health`, `/ready`, `/metrics`,
  `/v1/info`, `/v1/stats`, `/v1/operations`, `/v1/batch`,
  `/v1/feeder/session`, `/v1/feeder/heartbeat`, `/v1/feeder/import`,
  `/v1/feeder/events`, `/v1/feeder/gap`, `/v1/feeder/barrier` — or
  `unmatched` (14 values). A known path with an unsupported method (`405`)
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
| `knotrel_graph_jobs_outstanding` | gauge | Section 4. |
| `knotrel_graph_job_capacity` | gauge | Section 4. |
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
12). The sixth adds 23 source-managed only, for a current maximum of
**1414 standalone, 1743 source-managed** (section 13).

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

## 8. Second M6.2 increment: rejection and uncertain outcomes (implemented, unreleased)

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
| Admission capacity exhausted | `try_acquire_owned` failure in `Service::run` or `Service::execute_feeder_worker` | `requests_rejected_total{reason="busy"}`. HTTP stays `503 busy`, or `503 import_in_progress` for feeder session, heartbeat, events, gap and barrier calls (section 2); telemetry still records the internal cause `busy`. | request | caller |
| Import permit exhausted | `Service::acquire_import_permit` | `requests_rejected_total{reason="import_capacity"}` | request | caller |
| Shutdown | `lifecycle::shutdown_gate` refusing a non-probe request while draining. This includes requests that would never create a graph job, such as `/v1/info` and unmatched routes. | `requests_rejected_total{reason="shutting_down"}` | request | caller |
| Service already unavailable | The first check that refuses: `api::check_service_available`; the entry checks of `Service::run` and `execute_feeder_worker`; or the in-worker check under the graph lock (`graph.failed` or the unavailable flag) in `run`, `execute_feeder_worker` and `Service::import`. That check refuses before any change is applied to the live graph or a candidate is published; for import, JSON parsing and candidate construction may already have happened off-lock. | `requests_rejected_total{reason="unavailable"}` | request | caller, or worker for the in-worker check |
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
- a blocking task cancelled by runtime shutdown before it starts.

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
| `knotrel_requests_rejected_total{reason}` | counter | 4 |
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
   neutral `cancelled`, not `client_closed`. Whether `logging_middleware`
   should also log it is left to M6.3.
4. **Lag.** Recommendation: no lag metric until the protocol carries a
   comparable source frontier. That would be an integration-contract change,
   not an observability change.
5. **Overhead budget.** Open. It must be approved before an overhead
   campaign; see section 6.

## 10. Third M6.2 increment: graph node and edge gauges (implemented, unreleased)

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
| `Service::run` (single operations, legacy batches) | After the job's work returns, for state-changing jobs. Includes the successful operations of a batch that contained domain errors. | Read-only jobs (`stats`, query-only requests): counts cannot change. |
| `execute_feeder_worker` (events, gap) | After the operation returns, for mutating feeder calls, whether it succeeded, partially failed or was rejected. A partial failure publishes its applied prefix. | Session, heartbeat and barrier calls: no graph change. |
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
source-managed** (now 1414 and 1743; section 13).

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

## 11. Fourth M6.2 increment: graph-job queue, lock-wait and execution histograms (implemented, unreleased)

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
  initial load. Import `receive`/`prepare` timing remains pending.

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

The maximum becomes **1402 standalone, 1713 source-managed** (now 1414
and 1743; section 13). The test
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

## 12. Fifth M6.2 increment: graph-operation and feeder-event outcome counters (implemented, unreleased)

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

The maximum becomes **1414 standalone, 1720 source-managed** (now 1414 and
1743; section 13). The test
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

## 13. Sixth M6.2 increment: feeder rejection and desynchronization counters (implemented, unreleased)

**Series.** Two counters, registered only in source-managed mode, with
pre-created series and fixed labels:
`knotrel_feeder_rejections_total{code}` and
`knotrel_desynchronizations_total{cause}`. Labels come from
`metrics::FeederRejection` and `metrics::Desynchronization`, never from
session IDs, cursors, generations, event IDs, caller text or error messages.

**Endpoint scope.** Session, heartbeat, events, gap and barrier. Import is
excluded: its terminal results belong to the pending `imports_total`, whose
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
| `mode_conflict`, missing or invalid instance header (`invalid_instance_id`), header or body `stale_instance_id`, `invalid_graph_id`, malformed or unprocessable JSON | HTTP adapter, before the service is called; malformed bodies never become a typed `FeederError` | `http_requests_total` only |
| Import, including its fencing and precondition failures | import path | pending `imports_total` |

`FeederRejection::of` is total over `FeederError`. Variants produced only by
the HTTP adapter, by imports or by admission map to `None`. The unit test
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
| source-managed | 19 | 4 |

The maximum becomes **1414 standalone, 1743 source-managed**. The test
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
