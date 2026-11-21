# Feeder progress under query load (roadmap 0.9.3)

**Status: APPROVED by the maintainer on 2026-11-21 ("Approvati, procedi"),
unchanged from the proposal. Baseline measurement below.**
The protocol, the criteria and the decision rule below are fixed **before**
any measurement. After approval they do not change; a failed criterion is
reported as a failure, with its numbers, and is not reinterpreted.

This is a targeted check, **not** a release qualification. It answers one
question: does Knotrel stay current with its source while it serves queries,
or can query traffic make it progressively stale? The answer decides whether
v0.9.0 changes the server's admission or scheduling.

## Why

- **v0.6.0 measured the problem** (knotrel-benchmarks
  `results/v0.6.0-qualification/supplement-M/REPORT-M-SUPPLEMENT.md`, §4.1,
  100k nodes): feeder catch-up fell from about 7,000–9,800 events/s with 4
  query clients to about 900 events/s with 16, because each feeder batch
  waited behind about one round of queued query batches for the graph lock.
  With 64 clients the 32 admission permits were all held by queries and the
  feeder was refused with `503 import_in_progress`. That 64-client number
  mostly reflected the probe's clients retrying with no backoff.
- **v0.7.0 changed the lock** (query-only requests now share a read-write
  lock) and improved queries during feeder events, but did not re-measure
  catch-up under query load. Feeder events still take the lock exclusively,
  and admission still reserves no permit for feeder requests.
- **0.9.1 provides a real feeder.** This check uses the reference PostgreSQL
  feeder instead of a synthetic one.

## What is measured

Two quantities, both seen from outside Knotrel:

- **Freshness lag:** the time from a PostgreSQL commit to the moment Knotrel
  answers with that change.
- **Catch-up rate:** how fast a backlog of source changes is applied, and how
  long until Knotrel is `ready` again.

## Protocol

**Builds:**
- Knotrel server: `cargo build -p knotrel-server --release --locked` at tag
  `v0.8.0` (the baseline). A candidate, if one is needed, is built the same way
  from its commit.
- Feeder and load tool: `cargo build --release --locked` in
  `integrations/postgres-feeder` at the commit that adds the load tool.

**Machine:** one developer machine, recorded with the results; loopback
networking; PostgreSQL 18.7 in the local container of
`scripts/start-test-postgres.sh`. Before starting, `ps` shows no other
benchmark and no build running; the machine is on AC power.

**Server settings:** `KNOTREL_MODE=source-managed`,
`KNOTREL_TOPOLOGY_LIFECYCLE=true`, `KNOTREL_MAX_NODES=unlimited`,
`KNOTREL_MAX_EDGES=unlimited`, `KNOTREL_METRICS_ENABLED=true`, everything else
default (32 admission permits, `compact-bfs`). The feeder runs with its
defaults (batches of up to 1,024 events).

**Graphs** (the reference schema, loaded in PostgreSQL from a fixed seed):

| Graph | Nodes | Edges | Inactive nodes | Disabled edges |
| --- | --- | --- | --- | --- |
| G10k | 10,000 | 30,000 random | 1 % | 1 % |
| G100k | 100,000 | 300,000 random | 1 % | 1 % |

**Load tool** (`integrations/postgres-feeder/examples/load.rs`, added with the
measurement; it uses the feeder's own dependencies):

- **Query clients:** `C` clients, each in a closed loop sending
  `POST /v1/batch` with 8 `connected` queries between random registered nodes,
  on a persistent connection. On `503` a client waits with exponential backoff
  (10 ms doubling to 200 ms, ±50 % jitter) and retries. This retry policy is
  part of the protocol: unlike the v0.6.0 probe, refused clients do not spin.
- **Writer:** one PostgreSQL connection committing 200 transactions per
  second, 5 changes each (1,000 changes/s): 60 % flag toggles (`active`,
  `enabled`), 30 % edge inserts and deletes, 10 % node inserts and deletes.
- **Freshness marker:** every 200 ms the tool flips the `active` flag of one
  of 100 reserved marker nodes in its own transaction, records the time when
  `COMMIT` returns, then asks Knotrel `node_state` for that node every 2 ms
  until the flag matches. The lag of that marker is the difference. Marker
  requests use the same backoff on `503`, starting at 1 ms.
- **Samplers:** every second, Knotrel's applied source position (from
  `GET /v1/stats`; the delivery sequence is visible only to a registered
  feeder, so the tool does not read it) and the slot's retained WAL
  (`pg_current_wal_lsn() - confirmed_flush_lsn`).

**Scenarios:**

| ID | Scenario | Window |
| --- | --- | --- |
| L1 | **Steady freshness.** Fresh server and feeder bootstrap, wait until `ready`, start the writer and `C` query clients, 15 s warm-up, then a 60 s measured window. | 60 s |
| L2 | **Catch-up.** Knotrel serves queries while `stale` and `catching_up` (it refuses them only while `uninitialized`, `loading` or `desynchronized`), so the clients load the server during the whole catch-up. Fresh server, feeder bootstrap, wait until `ready`, stop the feeder, let the writer produce a backlog of 100,000 changes unthrottled, stop the writer, start `C` query clients, 5 s warm-up, start the feeder. Measured: time from feeder start to `ready` (barrier accepted), and backlog events divided by that time. | until `ready`, at most 300 s |

**Cells and repetitions** (each observation uses a fresh Knotrel server and a
fresh feeder; order: all cells of one repetition, then the next repetition):

| Scenario | Cells | Observations |
| --- | --- | --- |
| L1 | G100k with `C` = 0, 16, 64; G10k with `C` = 16 | 3 per cell |
| L2 | G100k with `C` = 0, 16, 64 | 3 per cell |

Reported per cell: the median over observations of each statistic, and the
range. Marker lag statistics are computed per observation over all markers
of its window, then the median over observations is reported.

**Budget:** 45 minutes of measurement for the baseline. If it is exceeded,
the run stops and reports what it has; cells are not dropped silently. One
repeated observation is allowed per cell, only for a recorded host problem
(for example a thermal warning), with the reason written down.

## Criteria (approved 2026-11-21)

Evaluated on the baseline (`v0.8.0`):

| ID | Criterion | Cell |
| --- | --- | --- |
| Q1 | **Freshness under typical load:** marker lag p99 ≤ 1 s and maximum ≤ 3 s | L1, G100k, `C` = 16 |
| Q2 | **No growing lag:** in every L1 cell, the median marker lag of the last 10 s of the window ≤ 2 × the median of its first 10 s + 50 ms, and the retained WAL at the end of the window ≤ its value at the start + 16 MB | all L1 cells |
| Q3 | **Catch-up under load:** catch-up rate with `C` = 16 ≥ 50 % of the rate with `C` = 0; with `C` = 64 ≥ 25 % | L2, G100k |
| Q4 | **Bounded recovery at saturation:** time to `ready` ≤ 60 s | L2, G100k, `C` = 64 |

Descriptive only (no criterion): query throughput and client p99 of every
cell, the G10k cell, feeder `503` refusals, feeder batch latencies.

## Decision rule

- **All criteria pass on the baseline:** no server change in v0.9.0. 0.9.3
  closes with "measured, no change needed", the numbers are recorded here, and
  the limits of the check (one machine, loopback, these rates) are stated.
- **A criterion fails:** design the smallest change that addresses the
  failing mechanism (for example a reserved admission permit for feeder
  requests, or giving queued feeder work precedence on the graph lock), with
  its invariants, implement it with tests, and measure the candidate on the
  same cells. The candidate must pass the failed criteria **and**:

| ID | Criterion for a candidate | Cell |
| --- | --- | --- |
| Q5 | Query throughput ≥ 90 % and client p99 ≤ 125 % of the baseline (the release qualification's tolerances) | L1, G100k, `C` = 16 and 64 |

  The change and its measurement are reviewed before it is merged. The final
  v0.9.0 campaign (0.9.6) qualifies it again with the rest of the release.

## Limits, stated in advance

- One machine, loopback, one PostgreSQL version: the numbers say nothing about
  networks with latency or other hardware.
- The writer rate and query mix are chosen, not derived from a real
  workload. A pass means "no problem at these rates", not "no problem at any
  rate".
- No performance claim for users follows from this check; the release notes
  can only report it as a targeted check, as for the inspection cost check.

## To approve

1. The criteria Q1–Q5 and their thresholds.
2. The retry policy of the query clients (exponential backoff on `503`).
3. The graphs, the writer rate (1,000 changes/s) and the backlog (100,000
   changes).
4. The budget (45 minutes) and the repetition rule.

## Baseline results (v0.8.0, 2026-11-21)

**Verdict: Q1 PASS, Q2 FAIL, Q3 FAIL, Q4 PASS.** By the decision rule, a
server change is needed (next section).

**Environment.**
- Machine: Apple M3, 8 CPUs, 16 GiB, macOS 26.5.2, on AC power, no thermal or
  performance warning; `ps` showed no other benchmark or build.
- Server: built at tag `v0.8.0` (`e3fc736`), release, SHA-256
  `2da94ec521f0169bbd2cfe7d2188e264d6582a935b27dced4e366c6d4ef6548e`.
- PostgreSQL 18.7 (`scripts/start-test-postgres.sh 18`), feeder and load tool
  release builds from the change that adds this section.
- Run: 2026-11-21 14:34:13–14:54:35 UTC, 1,222 s of the 45-minute budget, 21
  observations, none repeated, none failed.

Raw output:
[`baseline-v0.8.0.jsonl`](feeder-under-load/baseline-v0.8.0.jsonl)
(SHA-256 `c3acc77e1927bf565736a10e44933cfcc86b394f29d81ddb659bd4c7194e88eb`)
and the tool's summary
[`baseline-v0.8.0-summary.json`](feeder-under-load/baseline-v0.8.0-summary.json)
(SHA-256 `bc6e8c094c84a0942d24e4095259d0153f595cb416388afd69392faccc7ef0e6`).

**Implementation detail, disclosed.** In L2 the backlog was written in less
than the 10 s lease, so Knotrel was still `ready` when the feeder restarted and
no barrier would follow. To measure the protocol's event ("time from feeder
start to `ready`, barrier accepted"), the tool waits until Knotrel reports
`stale` before starting the feeder, and stops the clock when the restarted
feeder's barrier is accepted. Cells, criteria and thresholds are unchanged.

**L1, steady freshness** (median over 3 observations, range in brackets;
marker lag in ms):

| Cell | Marker p50 | Marker p99 | Marker max | Query req/s | Query p99 (ms) | Query 503s | Feeder 503s | Writer changes/s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| G100k, 0 clients | 18.4 [16.7–18.9] | 53.9 [53.5–55.6] | 56.6 [53.9–59.2] | — | — | 0 | 0 | 995 |
| G100k, 16 clients | 36.4 [33.2–37.6] | 142 [134–152] | 281 [200–317] | 1,233 | 77.2 | 0 | 0 | 951 |
| G100k, 64 clients | 253 [227–320] | 1,315 [1,260–10,721] | 1,515 [1,483–11,123] | 1,179 | 190 | 57,176 | 240 | 782 [715–791] |
| G10k, 16 clients | 30.6 | 57.9 | 59.8 | 32,145 | 1.6 | 0 | 0 | 998 |

**L2, catch-up of a 100,000-change backlog** (94,604 events in every
observation; median and range):

| Cell | Time to `ready` (s) | Events/s | Query req/s | Query p99 (ms) | Feeder 503s |
| --- | --- | --- | --- | --- | --- |
| G100k, 0 clients | 0.52 [0.50–0.52] | 180,402 [180,171–183,878] | — | — | 0 |
| G100k, 16 clients | 3.22 [3.2–3.3] | 29,416 [28,450–29,630] | 1,411 | 61.1 | 0 |
| G100k, 64 clients | 25.0 [15.0–40.7] | 3,789 [2,322–6,310] | 1,476 | 129 | 87 |

**Criteria.**

| ID | Result | Numbers |
| --- | --- | --- |
| Q1 | **PASS** | G100k, 16 clients: marker p99 142 ms (≤ 1,000), max 281 ms (≤ 3,000) |
| Q2 | **FAIL** | 11 of 12 L1 observations pass. G100k, 64 clients, repetition 3: its lag condition passes (last 10 s median 231 ms against first 10 s 6,847 ms), but its retained-WAL condition cannot be shown: the sample at the start of the window is missing (the tool found no active slot at that instant). The rule requires both conditions for every observation, so Q2 fails. |
| Q3 | **FAIL** | Catch-up with 16 clients 16.3 % of the no-query rate (≥ 50 % required); with 64 clients 2.1 % (≥ 25 % required) |
| Q4 | **PASS** | G100k, 64 clients: 25.0 s to `ready` (≤ 60 s) |

**What the numbers show** (descriptive; the verdict above stands as computed):
- **With 64 clients, admission starves the feeder.** Queries hold all 32
  admission permits; the feeder's event requests are refused with
  `503 import_in_progress` 188–243 times per 60 s window. In repetition 3 the
  feeder was refused continuously for several seconds right after the
  bootstrap, which produced the 7–11 s marker lags of that observation. The
  query clients here back off on `503`, unlike the v0.6.0 probe, so this is
  not an artifact of spinning clients.
- **With 16 clients there is no refusal, yet catch-up falls to 16 %.** Each
  feeder batch takes the graph lock exclusively and waits for the query
  batches holding or queued for it; at 100k nodes a query batch holds the lock
  for milliseconds. Freshness stays well inside Q1 at the steady 1,000
  changes/s, but a backlog drains about six times slower.
- **The writer fell short at 64 clients** (715–791 instead of 1,000
  changes/s): PostgreSQL, the feeder, Knotrel and 64 clients share one machine.
  The failures stand regardless: the feeder had less to apply, not more.

## Next step (decision rule)

A criterion failed, so 0.9.3 continues with the smallest server change that
addresses the failing mechanisms, designed before it is coded:

1. **Admission:** feeder requests must not be refused because queries hold
   every permit (for example capacity reserved for the feeder routes). This
   targets the 64-client starvation (Q2's lag spike, Q3 at 64 clients).
2. **Lock precedence:** queued feeder work should not wait behind a stream of
   query batches (for example writer preference on the graph lock for feeder
   jobs). This targets Q3 at 16 clients.

The candidate is then measured on the same cells against Q1–Q4 and Q5
(query throughput ≥ 90 %, p99 ≤ 125 % of this baseline) and reviewed before
merge.
