# Observability contract

**Status:** Vocabulary for roadmap M6.1, plus the first M6.2 slice
(section 7), which is implemented but unreleased. M6 is deferred and is not
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
| Admission capacity | `Service` permit semaphore (`max_pending_jobs`), import semaphore (1) | Internal | Rejections are logged but not counted. |
| Lifecycle phase, worker failure | `Phase`, `unavailable` flag | Exposed via `/ready` and opt-in `/metrics` | `readiness_state` and `service_unavailable` gauges. |
| Synchronization state | `GraphState` (`generation`, `version`, `sync_status`, `applied_delivery_sequence`, `source_position`, session, epoch, `lease_deadline`) | Internal | Partly visible through `/v1/stats`, headers and `/ready`. |
| Event outcome counts | `FeederEventsResponse.events_applied` | Exposed per response | No cumulative count anywhere. |
| Queue, lock-wait, execution, import and catch-up timing | none | **Needs instrumentation** | Only whole-request server-side latency exists, in logs and (opt-in) `/metrics`. |
| HTTP counts, duration, backlog, capacity, unavailable flag, readiness, build | `GET /metrics` (`metrics.rs`) | Exposed when `KNOTREL_METRICS_ENABLED=true` | First M6.2 slice; section 7. |

### Concrete operator gaps

1. No cumulative counts for rejection reasons, applied and duplicate events, gaps, conflicts and imports: they are visible only as individual log lines or response bodies. (Request counts by route and status class are now available from the opt-in `/metrics`.)
2. No backlog signal outside drain. (Now exported as `graph_jobs_outstanding` via the opt-in `/metrics`.)
3. Graph counts are readable only through `/v1/stats`, which competes for the same admission permits and graph lock as the work being diagnosed.
4. No breakdown of where time goes: caller latency cannot be split into queueing, lock wait and execution.
5. There is no record of how long bootstrap or replacement catch-up took.
6. A dropped request future leaves no request log line. (The opt-in `/metrics` endpoint now counts it as `cancelled`.)
7. The status code alone cannot distinguish the `503` causes: `busy`, `request_timeout`, `shutting_down`, `unavailable`, and `import_in_progress` from saturated feeder calls.

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

Import preparation runs before the lock and is timed separately.

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

- `503 request_timeout`: the worker keeps running and may still apply the mutation;
- `cancelled` (dropped request future): same as above;
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

**`import_in_progress` from feeder saturation.** Feeder events, gap and
barrier calls that fail admission return `503 import_in_progress`
(`FeederError::from(ServiceError::Busy)`), although the log says
`busy`. This is accepted compatibility debt (closure plan D7). Telemetry
classifies by the internal cause (`busy`) and leaves the HTTP code unchanged.

## 3. Counting units

| Unit | Counted once per | Batch / multi-event behavior |
| --- | --- | --- |
| HTTP request | request that reached the router and either produced a response or was cancelled | A batch counts as one request regardless of size or per-operation errors. Each request is counted exactly once. A scrape of `/metrics` counts itself after it completes, so it appears in the next scrape. Requests refused by the connection limit or malformed before routing never reach the router and are not counted. |
| Graph job | accepted worker job (operation, batch, stats, feeder call, import publication) | A batch is one job. |
| Graph operation | operation in `/v1/operations` or `/v1/batch` that reached the graph | Every executed batch entry is counted once with its own outcome; a batch is never counted as a single failure. Operations rejected before execution (size limit, malformed request) are not graph operations. |
| Feeder event | event evaluated in `/v1/feeder/events` | Applied prefix: each event counted by outcome. A retained exact duplicate counts as `duplicate`. The first failing event counts as `rejected`. Events after it were never evaluated and are not counted. Whole-request rejections count zero events. |
| Import | import request that obtained the import permit | Counted once with a terminal result. |

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
| `requests_rejected_total` | counter | Is work refused before execution, and why? | One per refusal at the boundary that refuses it: admission in `Service::run` / `execute_feeder_worker` (`busy`), `acquire_import_permit` (`import_capacity`), `shutdown_gate` (`shutting_down`), the pre-execution unavailable checks (`unavailable`). | `reason` ∈ {`busy`, `import_capacity`, `shutting_down`, `unavailable`} | continues | pending | E |
| `requests_outcome_uncertain_total` | counter | How many mutations may or may not have run? | One per request whose outcome is uncertain (section 2), at its producing boundary: `lifecycle::request_timeout` (`request_timeout`) and the `JoinError` mappings (`worker_failed`). Cancellation is already counted by `http_requests_total{status_class="cancelled"}`. | `cause` ∈ {`request_timeout`, `worker_failed`} | continues | pending | E |
| `graph_jobs_outstanding` | gauge, jobs | How deep is the backlog? | `Lifecycle::outstanding_jobs` read at scrape: queued plus running, including import, and including jobs whose caller timed out or was cancelled. Not bounded by `max_pending_jobs`: an import can add one, and a finishing worker releases its admission permit just before its `JobGuard` (drop order in `Service::run` and `execute_feeder_worker`), so a newly admitted job can briefly be counted alongside it. Do not alert on `outstanding / capacity > 1`. | none | unaffected | **implemented** | E |
| `graph_job_capacity` | gauge, jobs | What is the admission limit? | Graph-job admission permit limit (`max_pending_jobs`), constant after startup. Not a bound on `graph_jobs_outstanding`: imports and worker teardown can make outstanding jobs exceed it (see that row). | none | constant | **implemented** | E |
| `graph_job_queue_seconds`, `graph_lock_wait_seconds`, `graph_job_execution_seconds` | histograms, s | Is latency queueing, contention or work? | Section 2 split, recorded inside the worker closure, so it is observed even when the caller has gone. | `kind` ∈ {`operation`, `batch`, `stats`, `feeder_session`, `feeder_heartbeat`, `feeder_events`, `feeder_gap`, `feeder_barrier`, `import_publish`} | continues | pending | E |
| `graph_operations_total` | counter | What mix of operations runs, and how many change the graph? | One per executed operation (section 3), counted at `Operation::apply`. | `op` ∈ {`add_node`, `link`, `cut`, `connected`}; `outcome` ∈ {`changed`, `unchanged`, `true`, `false`, `error`} | continues | pending | E |
| `graph_nodes`, `graph_edges` | gauges | How big is the graph? | Published to atomics by the worker after each changed mutation and after import publication. Read at scrape with no lock. | none | jump to imported counts | pending (`/v1/stats`, locked) | E |
| `service_unavailable` | gauge 0/1 | Has a worker failure disabled the service? | The `unavailable` flag. Becomes 1 after a worker panic and stays 1 until process restart. | none | unaffected | **implemented** | E |
| `readiness_state` | gauge, one-hot | What would `/ready` say? | `lifecycle::readiness_state`: same precedence as `lifecycle::readiness`, computed read-only at scrape. Stale is derived from the published deadline, without the compare-and-swap. | `state` ∈ {`unavailable`, `draining`, `starting`, `ready`, and in source-managed mode `uninitialized`, `loading`, `catching_up`, `stale`, `desynchronized`} | follows state | **implemented** | E |
| `feeder_lease_remaining_seconds` | gauge, s | Is the feeder alive? | Published lease deadline minus now at scrape. Negative once expired. Absent with no session or in standalone mode. | none | session kept | pending | E |
| `feeder_events_total` | counter | Is the source making progress, and how much is redundant? | Per evaluated event (section 3) in `apply_events`. `unchanged` covers no-op mutations and `progress`. | `result` ∈ {`changed`, `unchanged`, `duplicate`, `rejected`} | continues | pending | E |
| `feeder_rejections_total` | counter | Why did a feeder call fail? | One per rejected feeder request, at the `FeederError` returned. Labelled with the underlying error code, not `partial_failure`. An event domain error (any core code) collapses to `event_failed`. Admission `busy` and `unavailable` are excluded (counted in `requests_rejected_total` or `requests_outcome_uncertain_total`). | `code` ∈ the static wire codes mapped in `api::feeder_error` (for example `stale_generation`, `fenced_feeder_session`, `lease_expired`, `source_epoch_mismatch`, `out_of_order_sequence`, `source_predecessor_mismatch`, `conflicting_duplicate`, `unverifiable_duplicate`, `graph_desynchronized`, `barrier_not_reached`, `superseded_barrier`) plus `event_failed` | continues | pending | E |
| `desynchronizations_total` | counter | Why did the graph require a rebuild? | One per transition into `desynchronized`, at the sites in `GraphState::apply_events` and `declare_gap` that set it. A predecessor mismatch covers both `prev_delivery_sequence` and `prev_source_position`, which share one wire code. Out-of-order sequences and evicted duplicates do not desynchronize. | `cause` ∈ {`gap_declared`, `source_epoch_mismatch`, `source_predecessor_mismatch`, `conflicting_duplicate`} | continues | pending | E |
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

**Remaining essential (`E`, pending):** rejection and outcome-uncertain
counters, job queue/lock/execution histograms, operation and feeder-event
counters, feeder rejections, desynchronizations, graph counts, lease
remaining, last-applied age, import gauge/counter/durations and catch-up
duration.

**Instrumentation locations for the remaining rows:**

| Signal | Location |
| --- | --- |
| Admission rejections and the job histograms | `Service::run` and `Service::execute_feeder_worker`. Record the instant before `spawn_blocking`, at closure start, around `graph.lock()`, and at release. |
| Outcome uncertain | `lifecycle::request_timeout` and the `JoinError` mappings in `Service::run`, `execute_feeder_worker` and `Service::import`. |
| Operations | `GraphState::apply`, called per operation for single and batch requests. |
| Events, rejections, desynchronizations | `GraphState::apply_events`, `declare_gap`, and feeder-error construction. |
| Imports | `api::feeder_import` and `Service::import`. |
| Catch-up | Start instant stored in `GraphState` at publication; observed in `declare_barrier` B2. |
| Graph counts and last-applied instant | Atomics updated by the worker under the lock. |
| `/metrics` reads | Atomics, the published readiness word, `outstanding_jobs` (a `watch` channel) and the registry. No graph lock, admission permit or import permit; registry and `watch` synchronization still applies. |

These need a handle to the per-instance registry inside `Service`, which
the first slice deliberately avoided (it reads `Service` from outside).

**Invariants (all slices):**

1. `/metrics` takes no admission permit, no graph lock and no import permit. It is never rejected by `busy`, and it reports during saturation, draining (treated like the probes in `lifecycle::shutdown_gate`) and worker failure.
2. Instrumentation never changes status codes, response bodies, headers, admission order or the compare-and-swap semantics of `/ready`.
3. Label sets are exactly those listed; series count is independent of graph size, request count, generations and sessions.
4. Counters are monotonic within a process; graph replacement never resets a counter.
5. Worker-side observations are recorded even when the caller timed out or was cancelled; HTTP observations never imply that a worker finished.
6. Registries are owned per router instance; no global registry or recorder.
7. With metrics disabled, no accounting or timing work is added to the request path.

**Focused tests for the remaining rows:**

- Request timeout counts `request_timeout` as uncertain while the job histogram is still observed after release.
- A worker panic counts `worker_failed` as uncertain, not as `unavailable` rejection.
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

Counters and histograms are created on first use and reset only on process
restart. Graph replacement does not affect any series.

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
counts are far below the bound.

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

## 8. Decisions

1. **Exposure of `/metrics`.** Resolved in section 7: existing listener and
   trust boundary, disabled by default, enabled by a startup option,
   documented as unauthenticated.
2. **Graph counts without the lock.** Recommendation (pending): use
   worker-published atomics (section 6) rather than a lock-taking scrape. The
   values may trail an in-flight job by that job's duration; this is stated
   in the metric help.
3. **Cancellation accounting.** Resolved in section 7: counted as the
   neutral `cancelled`, not `client_closed`. Whether `logging_middleware`
   should also log it is left to M6.3.
4. **Lag.** Recommendation: no lag metric until the protocol carries a
   comparable source frontier. That would be an integration-contract change,
   not an observability change.
5. **Overhead budget.** Open. It must be approved before an overhead
   campaign; see section 6.
