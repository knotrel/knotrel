# Feeder admission and scheduling under query load: design (roadmap 0.9.3)

**Status: PROPOSED, awaiting maintainer approval. No code has been written.**

The v0.8.0 baseline of the [feeder-under-load check](feeder-under-load.md)
failed Q2 and Q3. By its decision rule, 0.9.3 continues with the smallest
server change that addresses the failing mechanisms. This document states
the mechanisms, the proposed change, its invariants and how it is verified.
It does not change the approved criteria.

## 1. What the baseline showed

| Clients | Observation | Criterion |
| --- | --- | --- |
| 64 | The feeder's event requests were refused with `503 import_in_progress` 188–243 times per 60 s window; in one observation continuously for several seconds, with marker lags of 7–11 s | Q2 (that observation), Q3 at 64 clients (2.1 % of the no-query rate) |
| 16 | No refusal, but catch-up of a backlog at 16.3 % of the no-query rate | Q3 at 16 clients |

## 2. How the server behaves today (v0.8.0, read from the code)

- **One admission semaphore** of `KNOTREL_MAX_PENDING_JOBS` permits (default
  32) is shared by every graph job (`Service::admit`, and
  `Service::execute_feeder_worker` for feeder jobs). A request that finds no
  free permit is refused immediately. This covers query requests and also
  **every feeder request: events, barrier, gap, session registration and
  heartbeat**. Imports have their own semaphore of one permit.
- Consequence: with enough concurrent query requests, the feeder can be
  refused for as long as the queries keep every permit busy. Even the
  **heartbeat** can be refused, which can let the lease expire and turn
  Knotrel `stale` although the feeder is alive.
- **The graph lock** is a `parking_lot::RwLock`. Query-only jobs take it
  shared; feeder jobs take it exclusively. `parking_lot`'s lock is
  task-fair: once a writer waits, newly arriving readers wait behind it. So a
  waiting feeder job already does not starve behind a stream of new queries;
  it waits for the readers that hold the lock when it arrives.
- Therefore the 16-client slowdown is **not explained** by the lock policy
  alone. At 100k nodes a query batch of 8 `connected` holds the lock for a few
  milliseconds; waiting for the in-flight readers costs about that much per
  feeder batch, while the measured cost was about 34 ms per 1,024-event batch
  against 5.5 ms without queries. The remaining time is probably CPU
  contention: 16 clients' breadth-first searches keep most of the 8 cores
  busy, and PostgreSQL, the feeder and the load tool share the same machine.
  This is a hypothesis, not a measurement.

## 3. Change A: reserved admission for feeder requests

**What:** a second semaphore, used only by feeder requests in source-managed
mode, so that queries can never take the capacity the feeder needs.

- New setting `KNOTREL_FEEDER_RESERVED_JOBS`, integer from 1 to 64, default
  4. Ignored in standalone mode (no feeder routes).
- Feeder events, barrier, gap, session registration and heartbeat acquire a
  permit from the reserved semaphore **instead of** the shared one. Query
  requests, stats and inspection keep using the shared semaphore, unchanged.
  Imports keep their own single permit, unchanged.
- When the reserved semaphore is exhausted (only if the feeder sends more than
  4 concurrent requests, which the reference feeder never does), the feeder
  request is refused exactly as today: `503 import_in_progress`, counted as
  `busy`.

**Why 4:** the reference feeder has at most one events request and one
heartbeat in flight; a session registration or a barrier may overlap them.
Four covers that with one spare and keeps the extra bound small.

**Invariants kept** (each with a test):
1. **Bounded work.** Outstanding graph jobs stay bounded:
   `KNOTREL_MAX_PENDING_JOBS + KNOTREL_FEEDER_RESERVED_JOBS` plus one import,
   instead of `KNOTREL_MAX_PENDING_JOBS` plus one import. The documented bound
   in `docs/http-api.md` (resource bounds) changes accordingly.
2. **Same job path.** Reserved permits only change admission. The job still
   runs through the same worker, lock, failure marker, panic handling,
   cancellation (the permit stays with the worker), drain accounting and
   telemetry.
3. **No new outcome.** No HTTP route, field, status or error code changes. A
   refused feeder request still gets `503 import_in_progress`.
4. **Queries unaffected when no feeder request runs.** The shared semaphore
   and the query path are unchanged.

**Observability:** `requests_rejected_total{reason="busy"}` keeps counting
feeder refusals. The capacity gauge gains no new series. `GET /v1/info`
reports the new setting as `feeder_reserved_jobs` in source-managed mode
(additive field; documented in `docs/openapi.yaml` and `docs/http-api.md`).

**Expected effect:** the 64-client refusals and the heartbeat risk disappear.
This targets Q2's failing observation and Q3 at 64 clients. It does not by
itself address Q3 at 16 clients, where there were no refusals.

## 4. Step B: diagnose the 16-client slowdown before changing anything else

Because the lock already gives waiting writers precedence (section 2), no lock
change is proposed blindly. First a **diagnostic run**, declared here before
it runs, descriptive only (no criterion):

- Cell L2, G100k, 16 clients, 3 observations, the baseline build (`v0.8.0`)
  and the candidate with change A, same protocol.
- Recorded in addition: Knotrel's `/metrics` before and after each catch-up
  (`graph_job_queue_seconds`, `graph_job_lock_wait_seconds` and
  `graph_job_execution_seconds` for `kind="feeder_events"` and for the query
  kinds), the feeder's time per request (from its log), and host CPU usage
  sampled every second.

Decision, from the diagnostic:

| Finding | Action |
| --- | --- |
| Feeder events wait mostly for the lock (lock wait ≫ execution) | **B1:** let a queued feeder job pass ahead of the readers it would otherwise wait for: readers that have not yet started their traversal re-check a "feeder waiting" flag and yield before taking the shared lock. Designed and reviewed as an addendum to this document before coding. |
| Feeder events run slowly while the lock is held (execution ≫ lock wait) and the CPUs are saturated | **B2:** bound the number of query jobs that run concurrently (new setting, default the number of CPUs minus one), so a feeder job finds a core. Same addendum and review. |
| Neither dominates (the time is in HTTP, PostgreSQL or the feeder) | **B0:** no further server change. Q3 at 16 clients is reported as failing with its cause, for a maintainer decision. |

B1 and B2 trade query throughput for feeder progress; Q5 (query throughput ≥
90 %, p99 ≤ 125 % of the baseline) bounds that trade.

## 5. Verification

1. **Tests** for change A: queries holding every shared permit do not block
   feeder events, barrier, heartbeat or session (unit tests in `service.rs`
   with the existing permit-holding fixtures); a flood of feeder requests
   beyond the reserved permits gets `503 import_in_progress`; standalone mode
   is unchanged; settings parsing and `GET /v1/info`; the existing admission,
   cancellation, panic and drain tests keep passing.
2. **The postgres-feeder integration tests** on PostgreSQL 15 to 19.
3. **The approved measurement** on the candidate: all cells of the protocol,
   3 observations, against Q1 to Q4 and Q5. Same machine and conditions as the
   baseline, recorded in `feeder-under-load.md` with raw output.
4. **Review** of the change and its measurement before merge. The final
   v0.9.0 campaign (0.9.6) qualifies it again.

## 6. Compatibility and documentation

- No HTTP contract change; one additive `/v1/info` field; one new setting
  with a default. Deployments that do not set it get 4 reserved feeder
  permits.
- Update: `docs/http-api.md` (settings table, resource bounds, admission),
  `docs/openapi.yaml` (`/v1/info`), `docs/observability-contract.md` (the
  admission capacity row), the server README, `docs/deployment.md` if it
  sizes `KNOTREL_MAX_PENDING_JOBS`, the console's runtime decoder only if it
  reads the new field (it does not need to), and the release notes.

## 7. Risks

- **A misbehaving feeder** can now use up to 4 more concurrent jobs than
  before. The feeder is already a trusted, fenced writer (one active session);
  the bound stays fixed and small.
- **Change A may not be enough for Q3 at 64 clients** if the remaining time is
  CPU contention too; the measurement will show it, and step B applies.
- **A single-machine artifact:** if the diagnostic finds that the 16-client
  slowdown comes from the load generator, PostgreSQL and Knotrel sharing 8
  cores, no server change can remove it. That is reported as such (B0), not
  hidden.

## 8. To approve

1. Change A as specified (reserved permits for feeder requests, default 4,
   setting `KNOTREL_FEEDER_RESERVED_JOBS`, additive `/v1/info` field).
2. Step B: the diagnostic run and its decision table, with B1 or B2 designed
   as an addendum only if the diagnostic points to them.
3. The verification plan, including the full re-measurement against Q1–Q5.
