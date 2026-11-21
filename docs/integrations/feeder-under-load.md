# Feeder progress under query load (roadmap 0.9.3, release v0.9.0)

**Status: protocol APPROVED by the maintainer on 2026-11-21 ("Approvati,
procedi"), unchanged from the proposal. Measured: baseline v0.8.0 and the
server code destined for v0.9.0 (change A). Q3 FAILS on both and is accepted
as a declared limitation of v0.9.0, limited to the measured configuration
(section "Maintainer decision on Q3").**

"0.9.3" is the roadmap item, not a version: the work belongs to the
unreleased v0.9.0.
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

## Candidate A results (reserved feeder admission, 2026-11-21)

**Verdict: Q1 PASS, Q2 PASS, Q3 FAIL, Q4 PASS, Q5 PASS.** Change A
([design](feeder-admission-design.md)) removes the feeder refusals and the
64-client lag spikes, but catch-up under query load stays below Q3's
thresholds. By the approved design (section 4), Q3 is reported as failing,
with the diagnostic below, for a maintainer decision; no further server change
is made in 0.9.3.

**Environment.** Same machine and conditions as the baseline (AC power, no
thermal warning, nothing else running). Candidate: commit `96ded7a`, release,
SHA-256 `71d60afe27be75c034416d27c98704050d60eb02cd3991d79ce01fa2ea245509`.
The change was later rebased onto `main` for merge; `git diff` against
`96ded7a` shows no change in `crates/` outside the service's test module (one
added test for the lane labels), so the merged server code is the code
measured here.
Run 2026-11-21 17:13:22–17:35:18 UTC: diagnostic B (6 observations), then the
21 observations of the protocol in 1,168 s; none repeated, none failed.

Raw output (SHA-256):
- [`candidate-A.jsonl`](feeder-under-load/candidate-A.jsonl)
  `dbf82455ad0cedef1b1526768b120d6356683402fbe2cb76cad2d2b9ab786144`
- [`candidate-A-summary.json`](feeder-under-load/candidate-A-summary.json)
  `9b3e63d4aba8a43f2c2ecfa36a49b1e968d3be8eab594f79a65fe96daf895751`
- [`diagnostic-B-v0.8.0.jsonl`](feeder-under-load/diagnostic-B-v0.8.0.jsonl)
  `8cdeebd1e817f768ca843c1a3f3f144eaa1d4e1bbd8d3a45d6056539ea3bef9c`
- [`diagnostic-B-candidate-A.jsonl`](feeder-under-load/diagnostic-B-candidate-A.jsonl)
  `7e018d3f576841cb9a2378f7713351dabcd556b0b75066cdef493c95cf5b8306`

**Disclosed:**
- The diagnostic's v0.8.0 server was rebuilt from the same tag; its SHA-256
  (`b7125518…5c96e`) differs from the baseline build's because release builds
  are not byte-reproducible.
- In these runs the tool counted L2 query requests for about 1.1 s after
  `ready` while dividing by the time to `ready`, so the **L2 query throughput
  of the candidate and of the diagnostic is overstated** and is not compared
  with the baseline (fixed in the tool after the run). No criterion uses it:
  Q5 uses L1, and latency percentiles are unaffected by the divisor.

**L1, steady freshness** (median over 3 observations, range in brackets):

| Cell | Marker p99 (ms) | Marker max (ms) | Query req/s | Query p99 (ms) | Feeder 503s |
| --- | --- | --- | --- | --- | --- |
| G100k, 0 clients | 52.3 [45.6–55.6] | 56.8 [53.8–57.4] | — | — | 0 |
| G100k, 16 clients | 156 [104–185] | 179 [148–223] | 1,270 [1,267–1,297] | 74.7 [71.9–75.2] | 0 |
| G100k, 64 clients | 258 [254–299] | 309 [295–384] | 1,132 [1,129–1,152] | 155 [150–169] | 0 |
| G10k, 16 clients | 57.8 | 58.7 | 31,399 | 1.5 | 0 |

**L2, catch-up** (94,604 events in every observation):

| Cell | Time to `ready` (s) | Events/s | Feeder 503s |
| --- | --- | --- | --- |
| G100k, 0 clients | 0.50 | 190,906 [188,623–191,987] | 0 |
| G100k, 16 clients | 3.4 [3.1–3.5] | 27,997 [26,995–30,479] | 0 |
| G100k, 64 clients | 5.5 [5.4–6.1] | 17,075 [15,549–17,365] | 0 |

**Criteria, candidate against baseline:**

| ID | Baseline v0.8.0 | Candidate A | Result |
| --- | --- | --- | --- |
| Q1 | p99 142 ms, max 281 ms | p99 156 ms, max 179 ms | **PASS** |
| Q2 | FAIL (one observation) | all 12 L1 observations pass | **PASS** |
| Q3 | 16 clients 16.3 %, 64 clients 2.1 % | 16 clients 14.7 %, 64 clients 8.9 % (≥ 50 % and ≥ 25 % required) | **FAIL** |
| Q4 | 25.0 s | 5.5 s (≤ 60 s) | **PASS** |
| Q5 | — | 16 clients: query throughput 103 % (1,233 → 1,270 req/s), p99 97 % (77.2 → 74.7 ms); 64 clients: throughput 96 % (1,179 → 1,132 req/s), p99 82 % (190 → 155 ms) (≥ 90 % and ≤ 125 % required) | **PASS** |

**Effect of change A.** At 64 clients the feeder is no longer refused (240
refusals per window → 0), the marker lag p99 falls from 1,315 ms to 258 ms
and its maximum from 1,515 ms to 309 ms, catch-up rises from 3,789 to 17,075
events/s and time to `ready` from 25.0 s to 5.5 s. Query throughput and
latency stay within Q5; the 64-client query p99 improves, the 64-client
throughput is 4 % lower. At 16 clients, where there were no refusals, change A
has no measurable effect, as expected.

## Diagnostic B (16 clients, L2 G100k, 3 observations per build)

Mean per feeder events job (about 94 jobs of about 1,000 events per
catch-up), from Knotrel's phase histograms between feeder start and `ready`:

| Build | Time to `ready` (s) | Admission queue (ms) | Lock wait (ms) | Execution (ms) | Feeder 503s | Host CPU busy, catch-up (median) |
| --- | --- | --- | --- | --- | --- | --- |
| v0.8.0 | 2.72 / 2.76 / 3.82 | 1.17–1.43 | 6.12–6.56 | 0.82–0.83 | 0 | 93–97 % |
| candidate A | 3.35 / 2.97 / 3.04 | 1.15–1.27 | 6.10–6.39 | 0.81–0.84 | 0 | 93–94 % |

Query batch jobs in the same windows: admission queue 0.7–0.8 ms, lock wait
2.9–4.0 ms, execution 6.3–6.9 ms. During the warm-up with only queries running,
host CPU was 100 % busy.

What this separates:
- **Admission wait:** none. No feeder request was refused in any observation,
  with or without change A; the admission queue (blocking-pool scheduling) is
  about 1.2 ms per job.
- **Lock wait:** the largest phase inside Knotrel, about 6.2 ms per job, about
  7.5 times the execution. The lock is task-fair, so this is the time for the
  query batches already holding the lock (about 6.5 ms each) to finish.
- **Execution:** about 0.8 ms per 1,000-event job.
- **Outside Knotrel's graph jobs:** the three phases add up to about 0.8 s per
  catch-up (0.11 + 0.60 + 0.08 s), about a quarter of the 3 s. The rest is
  spent outside the server's graph work: HTTP, the feeder, PostgreSQL's
  decoding, all on a host whose CPUs are 93–97 % busy.

Reading, limited to what was measured:
- The lock-wait finding matches the decision table's first row (B1), which is
  out of scope for 0.9.3. Even removing the lock wait entirely would shorten a
  catch-up by about 0.6 s of 3 s, to roughly 20 % of the no-query rate; by this
  estimate a lock change alone would not reach Q3's 50 %.
- **CPU contention remains a hypothesis.** The host was saturated throughout
  and most of the time is outside the server's measured phases, which is
  consistent with it, but no run varied CPU availability (for example
  PostgreSQL and the clients on another machine), so it is not shown to be the
  cause.

## Status of roadmap item 0.9.3

- Change A is implemented, tested, measured and merged (`f283d30` on `main`,
  for v0.9.0): it fixes the admission starvation (Q2 and Q4 pass, Q3's
  64-client case improves fourfold) without costing query throughput or
  latency beyond Q5.
- **Q3 fails** at 16 and 64 clients, on v0.8.0 and on the v0.9.0 code. B1 and
  B2 were out of the approved scope and are not implemented.

## Maintainer decision on Q3 (2026-11-21)

- **Q3 is accepted as a declared limitation of v0.9.0, limited to the measured
  configuration**: one Apple M3 with 8 CPUs, macOS 26.5.2, loopback, PostgreSQL
  18.7, graph G100k, the protocol's writer rate and backlog, query clients with
  backoff.
- **The result stays FAIL.** The thresholds (≥ 50 % at 16 clients, ≥ 25 % at
  64) are unchanged; nothing in this document or in the release notes may
  present Q3 as passed.
- **No cause is claimed.** Host CPU contention is consistent with the
  diagnostic but was not demonstrated: no run varied CPU availability. No
  two-machine or pinned-CPU run was set up.
- The numbers to state with the limitation:

| G100k catch-up of a 100,000-change backlog | Required | v0.8.0 | v0.9.0 (change A) |
| --- | --- | --- | --- |
| 16 query clients, share of the no-query rate | ≥ 50 % | 16.3 % (29,416 / 180,402 events/s) | 14.7 % (27,997 / 190,906 events/s) |
| 64 query clients, share of the no-query rate | ≥ 25 % | 2.1 % (3,789 events/s) | 8.9 % (17,075 events/s) |
| 64 query clients, time to `ready` | ≤ 60 s (Q4) | 25.0 s | 5.5 s |

- **Follow-up:** [knotrel/knotrel#87](https://github.com/knotrel/knotrel/issues/87),
  feeder catch-up under query load: test the CPU hypothesis with a protocol
  variant approved beforehand, then, depending on the result, B1 or B2 against
  the same criteria. Not assigned to a release.
