# B2: bounded concurrent query execution (#87)

**Status: DESIGN PROPOSAL FOR MAINTAINER APPROVAL (2026-11-27).** Nothing is implemented or
measured.

- Approval fixes the mechanism, the bound, the comparison and the adoption criteria before
  any code is written or measured.
- Under the per-release measurement policy (#186) this is an **impact estimate**: it
  decides adoption only.
- The Q3 release verdict belongs to the v1.2.0 qualification (#182), on a defined
  candidate.

**Context.**
- The BFS experiment line is closed. `main` (25fadec) restored the v1.1.0 `Graph::connected`
  (#205), so the query path is v1.1.0's.
- v1.2.0 has no product change today. It stays open until this decision.

## 1. Starting point: the restored query path

| Quantity (one Apple M3, 8 CPUs, loopback, PostgreSQL 18) | Value | Source |
|---|---|---|
| L2 G100k catch-up share at C=16 | **15.0 %** (3 obs.) | #181 estimate, base arm (= v1.1.0 code) |
| same, C=64 | 9.6 % | same |
| same, CF5 S3 (v1.1.0 qualification) | 16.6 % and 9.1 % | #87 |
| L2 C=0 catch-up rate | 178,159 events/s, host CPU about 59 % busy | #181 base |
| Query batch execution (8 `connected` per batch), L2 C=16 | 6.7–7.2 ms | #181 base |
| Feeder lock wait per job of about 1,000 events, L2 C=16 | 7.0–7.5 ms (job execution about 0.8–1.1 ms) | #181 base; v0.9.0 diagnostic B |
| `Graph::connected` share of on-CPU server samples under query load | 84–96 % | H1 profiling |
| Host CPU busy during L2 C=16 | 93–97 % | v0.9.0 diagnostic B |
| L1 G100k C=16 query throughput and client p99 (v1.1.0 server) | 866 req/s; 71 ms | #182 attempt 1, Q5 reference arm |
| L1 G100k C=64 | 864 req/s; 173 ms | same |
| L2 G100k C=16 query throughput and p99 | 1,250–1,332 req/s; 64.5–80.5 ms | #181 base |

**How a query batch runs today** (`knotrel-server`, `service.rs`):
1. **Admission:** a permit of the shared lane (`KNOTREL_MAX_PENDING_JOBS`, default 32).
   Feeder events, barriers and gaps use their own reserved lane (default 4), so queries
   never take feeder permits.
2. **Execution:** `spawn_blocking`, then the graph's `parking_lot` `RwLock`.
   - A request whose operations are all queries holds the **read** guard for the whole
     batch, so the batch is one consistent snapshot.
   - Feeder events take the **write** guard.
   - The lock is task-fair: a waiting writer blocks newly arriving readers, but it must
     wait for every reader already inside.
3. **The failing mechanism:**
   - With C ≥ 8 closed-loop query clients, up to min(C, 32) CPU-bound batches run at once on
     8 cores, each stretched by time-slicing.
   - A feeder job waits for all of them to leave, then competes for CPU with them, and so do
     PostgreSQL and the feeder process.
   - The lock wait (about 7 ms) explains only part of the slowdown: at C=16 a job's cycle is
     about 6× longer than at C=0. The rest is CPU taken by queries. B1 is already excluded
     because the lock is task-fair (decision of 2026-11-26).

## 2. Mechanism: a query execution limiter

**Setting.** A new opt-in setting **`KNOTREL_MAX_CONCURRENT_QUERIES`**, an integer
1…`KNOTREL_MAX_PENDING_JOBS`.
- Unset (default): no limiter exists, and the code path is today's.
- Set to K: at most K all-query requests may hold or wait for the graph's read guard at
  once.

**Placement.**
- After admission and **before** `spawn_blocking`, a query job awaits a permit of an
  async semaphore with K permits.
- Waiting therefore uses no blocking thread and does not touch the graph lock.
- The permit moves into the worker with the admission permit and is released when the
  worker ends, whatever the outcome (success, error, panic).

**Scope.**
- Applies to requests whose operations are all queries (`/v1/batch`, `/v1/operations`,
  read path).
- Not applied to stats, inspection, writers or feeder jobs. Writers never wait on the
  limiter.

**Invariants.**

| # | Invariant |
|---|---|
| I1 | At most K query jobs are past the limiter, holding the read guard or waiting for it |
| I2 | Answers and snapshot semantics are unchanged: a batch is still evaluated under one read guard. *Lock slicing* (releasing the guard between the queries of a batch) is **rejected**, because it would break the one-snapshot guarantee |
| I3 | Admission and `503 busy` semantics are unchanged. A job waiting at the limiter keeps its admission permit, so queued plus running query jobs stay ≤ the shared-lane capacity, as today |
| I4 | A request that times out or disconnects while waiting at the limiter has run nothing; its permits are released (the existing request timeout covers the wait) |
| I5 | With the setting unset, behavior and code path are exactly today's (no semaphore object, no extra await) |
| I6 | Observability: a new phase histogram `limiter_wait` (per job kind) and a gauge of jobs waiting at the limiter, next to the existing `admission_queue`, `lock_wait` and `execution` phases |

**Bound for the estimate:** **K = 4** on the 8-CPU host, half the CPUs. Rationale:
- At C=0 the whole catch-up path (feeder, PostgreSQL, server) uses about 59 % of the host,
  about 4.7 CPUs.
- K = 4 leaves about 4 CPUs to that path while queries keep the other half.
- K is fixed by this design and not tuned after measuring. A different K would be a new
  estimate.

## 3. Expected effect (model, to be verified, not a prediction to rely on)

**Feeder opportunity.**
- **Lock wait:** a waiting feeder job now waits only for at most 4 batches, each running
  without time-slicing. That is at most one batch execution (about 7 ms), about 3.5 ms on
  average, against 7.0–9.3 ms today.
- **CPU:** queries can use at most about 4 CPUs, so about 4 stay available to the feeder,
  PostgreSQL and HTTP.
- **Rough expectation:** the C=16 share rises from about 15 % into the 50–85 % range, and
  the C=64 share lands in the same range, because the bound does not grow with C.
- **Main uncertainties:** HTTP and JSON work for queued requests still uses CPU, and
  PostgreSQL's own needs are not measured separately.

**Cost to queries.**

| Load | Today (v1.1.0 path) | With K = 4 (model) | Q5 threshold |
|---|---|---|---|
| L1 C=16 throughput | 866 req/s | ceiling about 4 / 7 ms ≈ 560–600 req/s (−30 to −35 %) | ≥ 90 % of the reference → **expected to fail** |
| L1 C=16 client p99 | 71 ms | mean latency ≈ 16 / 580 ≈ 28 ms; p99 uncertain, about 60–110 ms | ≤ 125 % (≈ 89 ms) → uncertain |
| L1 C=64 throughput | 864 req/s | same ceiling, about −30 % | expected to fail |
| L1 C=64 client p99 | 173 ms | admitted jobs ≤ 32: mean ≈ 32 / 580 ≈ 55 ms; p99 likely ≤ today | likely within 125 % |
| L2 C=16 throughput | 1,250–1,332 req/s | about 560–600 req/s (about −55 %) | descriptive |

**In short:** a fixed bound trades query throughput for feeder progress. Under the approved
thresholds the expected outcome is Q3 PASS and Q5 FAIL in the opt-in mode, with the default
mode unchanged.

## 4. Alternatives considered

| Alternative | Why not in this design |
|---|---|
| **Gated bound:** apply K only while a feeder job is queued or running | smaller Q5 cost in L1 (no backlog), but it adds coupling between lanes and a trigger to tune. It is a possible second candidate only if this one is rejected for Q5 alone and the maintainer asks for it |
| Lock slicing: release the read guard between queries | breaks the one-snapshot batch semantics (I2) |
| B1: feeder passes waiting readers | already excluded: the lock is task-fair (2026-11-26) |
| `cluster-forest` for feeder-heavy workloads | a reference, not a server change: it passed Q3 in CF5 S3 (93.5 %, 77.8 %). It stays documented guidance |

## 5. Before/after comparison (impact estimate)

**Arms.**

| Arm | Build | Setting |
|---|---|---|
| `before` | `main` 25fadec server (= v1.1.0 query path) | — |
| `off` | B2 build | `KNOTREL_MAX_CONCURRENT_QUERIES` unset |
| `on` | B2 build | `KNOTREL_MAX_CONCURRENT_QUERIES=4` |

`before` is the Q runner's in-session reference server (v1.1.0 = 25fadec for the server and
the query path).

**Runner.** The frozen block-Q procedure of the v1.0.0 and v1.2.0 qualifications, run once
for `off` and once for `on`, each with the `before` reference arm in the same session:
- `run_v100q_q.py` (3 rounds, the seven cells, cap 2,700 s);
- `evaluate_v100q_q.py`, as fixed in knotrel-benchmarks#60;
- PostgreSQL 18 container only during Q;
- same host conditions as #182: AC power, `caffeinate`, swap counters.

**Correctness first (no timing).**
- Unit tests of the limiter: the bound is respected under concurrency; cancellation while
  waiting releases both permits; writers and feeder jobs never wait on it; the unset path
  is unchanged.
- Integration tests of `/v1/batch` and `/v1/operations` with the setting on.
- The existing differential and server test suites, and the full CI matrix.

**Reported (descriptive), every arm:**
- query throughput and client p50/p99;
- the C=16 and C=64 shares;
- feeder lock wait, `limiter_wait` and query batch execution from the phase histograms;
- host CPU.

**Two summary quantities, reported separately:**
- **feeder recovery against `before`:** (share_on − share_before) / share_before at C=16;
- **distance from Q3:** share_on − 50 % at C=16, in points.

## 6. Adoption criteria (approved thresholds, unchanged)

| Mode | Q1 | Q2 | Q3 | Q4 | Q5 (against the `before`/v1.1.0 reference) |
|---|---|---|---|---|---|
| `off` (default) | PASS | PASS | reported (expected FAIL, as today) | PASS | **PASS**: the default mode must not regress |
| `on` (K = 4) | PASS | PASS | **PASS**: C=16 ≥ 50 % and C=64 ≥ 25 % of C=0 | PASS | **maintainer decision before the freeze**, see below |

Thresholds: Q1 marker lag p99 ≤ 1 s and max ≤ 3 s; Q2 no growing lag and retained WAL ≤
start + 16 MB; Q3 as above; Q4 time to `ready` ≤ 60 s; Q5 throughput ≥ 90 % and p99 ≤ 125 %.

**Q5 in the `on` mode: a decision needed now.**
- **(A) Q5 deciding in both modes.** By the model, the `on` mode fails Q5 and B2 is
  REJECTED. Q3 then stays FAIL as a declared limitation, and v1.2.0 has no product change.
- **(B) Q5 deciding in the default mode only; in the `on` mode it is reported as the
  declared cost of an explicit opt-in.** The release notes would state the measured
  throughput and p99 change next to the feeder gain.
  - *Recommended.* Defaults stay Q5-clean, and the trade-off is the operator's informed
    choice.

**Adopt only if** every deciding cell passes, in both modes, under the chosen Q5 option, and
the correctness checks hold. Otherwise the result is "no change": Q3 keeps its declared
limitation and B2 stays closed.

**Stop rule.**
- If the `off` run already fails a deciding criterion, the `on` run is skipped and recorded
  as skipped.
- If the `on` run fails Q3, the result is REJECT whatever Q5 shows.

## 7. Budget and order

1. Implementation and correctness tests, then CI.
2. Freeze: B2 commit, tooling commit, SHA-256 of this design.
3. `off` Q run.
4. Stop check.
5. `on` Q run.

**Cap: 2 hours of host time** for the two Q runs (each capped at 2,700 s of collection, plus
Docker start and stop). Builds are excluded. If the cap is reached first, the outcome is
INCOMPLETE.

## 8. What follows

- **ADOPT.** B2 becomes the v1.2.0 product change. The next #182 attempt is scoped from the
  diff (server query admission path, and Q in both modes) and scheduled on that defined
  candidate.
- **REJECT.** Q3 keeps its declared limitation, and there is no product change for v1.2.0.
  The maintainer then decides whether to close 1.2.0 without a release, or to fold the
  documentation into the next release.
