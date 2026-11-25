# Atomic reference for the hub 10k lifecycle requirements (B4 `ops`, `events`)

Status: PROPOSED — awaiting maintainer approval; no measurement taken.

Tracking: [#180](https://github.com/knotrel/knotrel/issues/180) (H2, ledger
row "Hub 10k lifecycle ops and events (B4)"). Measurement policy:
[#186](https://github.com/knotrel/knotrel/issues/186). Release qualification:
[#182](https://github.com/knotrel/knotrel/issues/182).

This document proposes a reference design. It changes no code, no protocol
and no result. Every statement about code or evidence below cites its
source; statements marked **Assumption** were not verified in code or by
measurement.

## 1. The two requirements

Block B4 measures HTTP lifecycle timing as a paired comparison of two arms
inside one build: a lifecycle-enabled server (candidate arm) against a plain
server driven by an application-side planner (reference arm). The two
requirements are:

| Requirement | Cell (not equivalent) | Frozen counterpart (descriptive) | Candidate arm | Tolerance |
| --- | --- | --- | --- | --- |
| **B4 `ops`**: lifecycle batch operations, standalone mode | `B4-http-ops-equivalent-compact-bfs-hub-10000` | `B4-http-ops-compact-bfs-hub-10000` | 40 requests to `/v1/batch`, 256 lifecycle operations each, mutation-heavy trace without queries | 25% |
| **B4 `events`**: lifecycle feeder events, source-managed mode | `B4-http-events-equivalent-compact-bfs-hub-10000` | `B4-http-events-compact-bfs-hub-10000` | the same trace as feeder events, 128 per request to `/v1/feeder/events` | 25% |

Sources: cell definitions in
[`run_lifecycle_qualification.py` L175–L187](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/scripts/run_lifecycle_qualification.py#L175-L187)
(requests 40, tolerance 0.25); chunking in
[`http-lifecycle.rs` L1195–L1302](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L1195-L1302);
tolerances in
[`MATRIX.json` L63–L108](https://github.com/knotrel/knotrel-benchmarks/blob/v1.0.0/results/v1.0.0-qualification/MATRIX.json#L63-L108).
The `hub` topology is a ring backbone plus `max(1, n / 2000)` hubs of degree
`min(2000, n − 1)`, so five hubs at 10k
([`lib.rs` L49 and L110–L120](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/lib.rs#L110-L120)).

Their state today:

- v0.7.0 qualification 3 declared them not equivalent and unqualified
  ([PROTOCOL §4.3, L237–L307](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/PROTOCOL.md#L237-L307);
  [DECISIONS D3, L119–L160](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/DECISIONS.md#L119-L160)).
- v1.0.0 named them **E6**: run unchanged inside B4, no verdict, "cannot be
  qualified by comparison until an atomic reference design is approved"
  ([plan §4.2](../releases/v1.0.0-qualification-plan.md#42-excluded-the-reduction),
  [§5.6](../releases/v1.0.0-qualification-plan.md#56-unchanged-consequence-for-e6),
  [§8 rule 3](../releases/v1.0.0-qualification-plan.md#8-consequences-for-the-rc-approved-2026-11-23);
  [`MATRIX.json` `not_equivalent` and `unqualified_requirements`, L94–L107](https://github.com/knotrel/knotrel-benchmarks/blob/v1.0.0/results/v1.0.0-qualification/MATRIX.json#L94-L107)).
  The maintainer accepted them as unqualified for 1.0 with no performance
  claim ([DECISION §4 E6, L190–L196](https://github.com/knotrel/knotrel-benchmarks/blob/v1.0.0/results/v1.0.0-qualification/DECISION.md#L190-L196);
  [v1.0.0 release notes](../releases/v1.0.0-release-notes.md), "Not qualified (E6)";
  [readiness item 5](../releases/v1.0-readiness.md#state-during-v090)).
- v1.1.0 CF5 compared engines on the 11 deciding equivalent B4 cells only
  (L1); the two E6 cells are not part of it
  ([CF5 plan, cell L1](../releases/v1.1.0-cf5-qualification-plan.md#42-core-lifecycle-lifecyclegraph)).

## 2. Why no equivalent reference exists

### 2.1 What makes the other B4 planner cells equivalent

The plain server alone has no lifecycle semantics. The `-equivalent`
references (qualification 3) make the application provide them: inside the
timed reference window, for each lifecycle chunk, the application computes
the per-operation outcome, updates its catalog with the frozen planner, and
sends the plain `link`/`cut` requests that keep the plain graph equal to the
effective graph
([PROTOCOL §4.3](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/PROTOCOL.md#L237-L307);
[`run_window`, L691–L737](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L691-L737)).
Post-window checks compare outcomes, counts, registered content and plain
content
([PROTOCOL §4.4](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/PROTOCOL.md#L309-L348)).

Capability alone is not enough. The candidate guarantees
([topology lifecycle §4](../topology-lifecycle.md#4-atomicity-revisions-and-failure)):

- each lifecycle command is one indivisible step under the graph lock; no
  query observes a partly deactivated or partly removed node;
- each request (batch or feeder events) runs under one hold of the graph
  lock, so no query sees a state between its operations.

The reference matches this only if **each lifecycle chunk maps to one plain
request**. The probe records this per pair as `reference_split_chunks`,
`reference_max_op_plain_ops` and `atomicity_equivalent`, true only when no
chunk is split and no single command expands beyond 1,024 plain operations
([`Atomicity`, L660–L681](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L660-L681);
[record, L1549–L1553](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L1549-L1553)).
The report marks a deciding cell with `atomicity_equivalent: false` as
`NOT_EQUIVALENT`, fail-closed
([`report_v070q3_bc.py` L60–L71](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/scripts/report_v070q3_bc.py#L60-L71)).

Recorded in v1.0.0 attempt 1
([`BC-analysis.json`](https://github.com/knotrel/knotrel-benchmarks/blob/v1.0.0/results/v1.0.0-qualification/analysis/attempt1/BC-analysis.json)):

| Cell | `reference_max_op_plain_ops` | `reference_split_chunks` | `atomicity_equivalent` |
| --- | --- | --- | --- |
| ops, ops-query, events: sparse 10k and 100k (6 cells) | 10–15 | 0 | true |
| ops-query, hub 10k | 6 | 0 | true |
| **ops, hub 10k** | **1,565** | **2** | **false** |
| **events, hub 10k** | **1,565** | **2** | **false** |

### 2.2 The root cause

1. **One command is larger than any plain request.** The mutation-heavy hub
   trace deactivates two hub nodes (1754 and 5231). The planner expands a
   deactivation into one `cut` per enabled edge to an active neighbour
   ([`Planner::plan`, `SetNode`, L319–L334](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L319-L334)):
   1,425 and 1,565 plain cuts
   ([PROTOCOL L275–L280](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/PROTOCOL.md#L275-L280)).
   The query-heavy `ops-query` trace on the same graph does not hit a hub
   (largest expansion 6, table above).
2. **The plain request cap is fixed.** A `/v1/batch` or `/v1/feeder/events`
   request carries 1 to 1,024 operations: `MAX_BATCH_OPERATIONS`, a constant
   ([`lib.rs`](../../crates/knotrel-server/src/lib.rs) line 118, enforced in
   [`api.rs`](../../crates/knotrel-server/src/api.rs) line 1314 and
   [`service.rs`](../../crates/knotrel-server/src/service.rs) line 1432;
   documented in [`lib.rs`](../../crates/knotrel-server/src/lib.rs) "batches
   contain 1–1024 operations"). The probe mirrors it as `BATCH = 1_024`
   ([L93](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L93)).
   So the two chunks that contain these commands need two plain requests
   each (39 requests against 37 for ops, 76 against 74 for events).
3. **The plain API has no other atomic route.** It offers `add_node`,
   `link`, `cut` and `connected`; the standalone plain server has no
   import, and a plain request is the largest unit isolated by the graph
   lock. In source-managed mode the only atomic plain route is a full
   replacement import, which qualification 3 explicitly did not adopt
   without a maintainer decision (decision 8; `MATRIX`
   `unqualified_requirements`).

Changing the chunking cannot help: the indivisible unit is the single
command. Changing the trace to avoid hubs would remove the property the
requirement is about. In short: **the other B4 cells are equivalent because
the server's per-request isolation is enough to carry each lifecycle chunk;
on hub 10k it is not, and no server route the reference may use provides a
larger isolated unit.** Any atomic reference therefore has to provide
isolation outside a single plain request.

## 3. Recommended design: application isolation gate (`-atomic` reference)

### 3.1 What the reference arm executes

The reference arm is the existing `-equivalent` reference (same graph,
seed, trace, chunks, frozen planner, prebuilt plain requests, post-window
checks) with **one addition inside the timed window**: the application
holds an exclusive *isolation gate* for the whole of each lifecycle chunk.

Per lifecycle chunk (256 operations for `ops`, 128 for `events`), inside the
reference window:

1. acquire the application's gate exclusively (an async readers-writer lock
   owned by the application, created untimed when the arm is prepared);
2. compute the per-operation outcomes and update the catalog (unchanged);
3. send every plain request of the chunk, in order (unchanged: one request
   for most chunks, two for the two split chunks);
4. release the gate.

The gate is taken for **every** chunk, not only the split ones: an
application cannot offer isolation selectively without first planning the
chunk, and a uniform rule keeps the window comparable across chunks. Any
reader of the application (a `connected`, `node_state` or `edge_state`
answered by the application or forwarded to the plain server) takes the
gate shared.

The candidate arm does not change.

### 3.2 Why it is semantically equivalent

| Property | Candidate (lifecycle server) | `-atomic` reference | Equal? |
| --- | --- | --- | --- |
| Graph and trace | hub 10k, seed, mutation-heavy trace, 256 / 128 per request | identical inputs; same plain requests as the `-equivalent` reference | Yes |
| Lifecycle semantics | per-operation outcome, registered topology and flags | frozen planner computes the same outcomes and catalog, checked after the windows (PROTOCOL §4.4) | Yes, as today |
| Per-command indivisibility | one command is one step under the graph lock | the 1,425 / 1,565 cuts of a hub deactivation are inside one gate hold; no gated reader observes a partly deactivated hub | Yes, for readers through the application |
| Per-request isolation | one graph lock hold per request | one gate hold per lifecycle chunk, spanning one or two plain requests | Yes, for readers through the application |
| Visibility to direct server clients | every client of the server | a client reading the plain server directly, bypassing the application, can observe the state between the two plain requests of a split chunk | **No**: residual difference (decision M2) |
| Failure atomicity | batches continue after a domain error, no rollback ([§4](../topology-lifecycle.md#4-atomicity-revisions-and-failure)); a refused request applies nothing | domain outcomes computed as today; a transport or admission failure of the second request of a split chunk would leave the first applied | **Not provided**; in the probe any non-success response aborts the pair (`send` asserts success, [L683–L689](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L683-L689)), so no measured pair contains such a state (decision M3) |
| `state_version` and `generation` | +1 per changed command; generation unchanged | plain server versions count plain operations | Not compared, as for every existing equivalent reference (PROTOCOL §4.4 lists no version check) |

The design follows the premise the 11 deciding equivalent cells already
rest on: the plain server alone does not offer lifecycle semantics, so the
application on top of it is the service boundary
([PROTOCOL §4.3](https://github.com/knotrel/knotrel-benchmarks/blob/v0.7.0/results/v0.7.0-qualification-3/PROTOCOL.md#L237-L307)).
The gate extends that premise from capability to isolation.

The residual difference is larger for `events` than for `ops`. In
source-managed deployments readers query the server directly and the feeder
only writes; a gate therefore requires readers to go through the feeder's
application (or a proxy sharing its gate), which is a different deployment
shape. For `ops` the application already answers or forwards queries in the
reference design (`ops-query` planner,
[probe header L31–L33](https://github.com/knotrel/knotrel-benchmarks/blob/v1.1.0/tools/lifecycle-probes/src/bin/http-lifecycle.rs#L31-L33)).
The maintainer may adopt the design for `ops` only (decision M1).

**Assumption:** the B4 window is one sequential client, so the gate is
never contended in a measured pair; its added cost is one uncontended
acquire and release per chunk (40 per `ops` pair, 80 per `events` pair).
Its size is not measured and no claim is made about it.

### 3.3 Harness changes (knotrel-benchmarks; not done)

1. **Probe** (`tools/lifecycle-probes/src/bin/http-lifecycle.rs`):
   - new scenarios `ops-atomic` and `events-atomic`: the `-equivalent`
     reference plus the gate of §3.1; the candidate arm unchanged;
   - `Atomicity` gains `gated_chunks`; for `-atomic` scenarios
     `atomicity_equivalent` is true only when every lifecycle chunk,
     split or not, was sent under one gate hold; the record adds
     `reference_gated_chunks` and `atomicity_mode: "application_gate"`,
     and keeps `reference_split_chunks` and `reference_max_op_plain_ops`;
   - an untimed gate check after both windows (no request in flight): it
     replays each split chunk on a fresh reference arm while a concurrent
     reader takes the gate shared and queries `connected(hub, w)` over the
     hub's incident edges; every observation must be all-before or
     all-after. A negative control with the gate disabled must detect an
     intermediate state (as the injected-mutation smokes of qualification 3).
2. **Runner** (`scripts/run_lifecycle_qualification.py`): two new cells,
   `B4-http-ops-atomic-compact-bfs-hub-10000` and
   `B4-http-events-atomic-compact-bfs-hub-10000`, with the counterpart's
   requests (40) and tolerance (25%); the ordering rule that places a twin
   after its frozen counterpart (L75–L95) is extended to the `-atomic-`
   suffix. New IDs keep the historical `-equivalent-` cells' meaning
   unchanged.
3. **Matrix and report**: a new `atomic_references` block (deciding cells,
   atomicity rule, accepted residual differences) and the same fail-closed
   rule: `atomicity_equivalent: false` gives `NOT_EQUIVALENT`
   (`report_v070q3_bc.py` or its successor). Tests in `scripts/tests`.
4. **Smoke at real size before the freeze**, with the gate check and its
   negative control, as qualification 3 did for its equivalent cells.

No server change is required or proposed.

### 3.4 Fit with the per-release measurement policy (#186)

- **Approving this design is not a measurement.** It is a protocol and
  harness change; it starts no campaign and produces no number (#186:
  no standalone measurement).
- **No impact estimate applies to the design itself.** An impact estimate
  compares one product change before and after; this design changes no
  product code. The #180 ledger step "then impact estimate" applies only
  if a v1.2.0 product change touches the path these cells execute and its
  adoption depends on the result. **Assumption:** #87 (B1 or B2) may touch
  the feeder events or admission path; whether that includes the code the
  `events` cell executes is known only once the change exists, and the
  cell would then belong in #87's impact-estimate scope.
- **Release qualification (#182).** At the #182 freeze, the scope is
  derived from the diff `v1.1.0..candidate`. The two `-atomic` cells enter
  #182 as deciding cells **only if** their code path changed: for `ops`,
  `/v1/batch` handling with lifecycle operations, admission, the service's
  lifecycle command application and the `compact-bfs` engine; for
  `events`, the same plus `/v1/feeder/events` and `GraphState::apply_events`;
  for both, `Cargo.lock` or toolchain changes (#186 rule) (decision M5 fixes
  this list). Criteria (25% tolerance, B4 interval rule) are frozen before
  collection.
- **If the path did not change**, #182 adds no cell and the requirement gets
  the written disposition of §5 citing the diff; the approved design stays
  on file for the first release that changes the path. The requirements
  remain unqualified until then; no verdict is inferred from earlier runs.

## 4. Alternative: chunk-level replacement import (`events` only)

For `events` only, the reference sends, for each lifecycle chunk that does
not fit one plain request, one `POST /v1/feeder/import` in mode `replace`
carrying the effective graph after that chunk (built by the application
inside the window); other chunks keep plain events. This is the "only
atomic plain route" named in qualification 3 and not adopted there
(decision 8).

- **For:** isolation is server-side and visible to every reader, with no
  reader-routing assumption.
- **Against:** a different cost class (an off-lock O(V + E) build and an
  atomic publish per affected chunk, against incremental cuts); a different
  visible semantics (`generation` changes on each import publication,
  while the candidate's lifecycle commands never change it;
  [§4](../topology-lifecycle.md#4-atomicity-revisions-and-failure)), so
  subsequent events carry the new generation and readers observe a
  generation change the candidate does not produce; the import lane and
  permit differ from the events lane. **No route exists for `ops`**, which
  would still need the §5 disposition.

Rejected without further development: raising the plain request cap for the
reference arm (it changes the build under test and the documented 1–1,024
contract); a hub-free trace (it removes the property under test); splitting
the hub command in the candidate (it changes the candidate's contract).

## 5. Fallback: written disposition (no reference adopted)

The maintainer can adopt this text instead, closing the #180 ledger row
without a reference:

> **Disposition, B4 `ops` and `events`, compact-bfs hub 10k (v1.2.0, H2,
> #180).** The two B4 lifecycle requirements on the compact-bfs `hub` 10k
> graph (`B4-http-ops-…-hub-10000`, `B4-http-events-…-hub-10000`) remain
> **unqualified** and carry **no performance claim**. Reason: a lifecycle
> deactivation of a hub node in their trace expands to 1,425 and 1,565 plain
> operations, above the fixed 1,024-operation limit of a plain batch or
> feeder events request; the standalone plain server has no atomic import;
> so no plain reference offers the candidate's per-command indivisibility
> and per-request isolation, and no comparison under the B4 protocol can
> qualify them. Measurements taken in v0.7.0 qualification 3 and v1.0.0
> are indicative only and are not verdicts. No reference design is adopted
> [or: the reference design in `docs/design/hub-10k-lifecycle-reference.md`
> is approved but not run, because the code path is unchanged since v1.1.0:
> `<diff evidence>`]. The cells may still run inside B4 when B4 is in scope,
> reported descriptively with no verdict. The disposition is revisited only
> if a release changes the lifecycle batch or feeder events path, or the
> plain request limit, or if a reference design is approved. The requirement
> stays listed among the unqualified requirements in the release notes of
> each release until then.

## 6. Decisions for the maintainer

- **M1. Which outcome.** (a) adopt the application gate (§3) for both
  requirements; (b) adopt it for `ops` only and dispose `events` with §5;
  (c) adopt the replacement-import alternative (§4) for `events` and dispose
  `ops` with §5; (d) adopt no reference and record §5 for both.
- **M2. Residual visibility.** Whether isolation "for readers through the
  application" is accepted as equivalent to the candidate's isolation for
  every server client (§3.2), for `ops` and, separately, for `events`.
- **M3. Failure atomicity.** Whether aborting the pair on any non-success
  response is an acceptable treatment of the missing cross-request failure
  atomicity, or the reference must also compensate (and how that is timed).
- **M4. Cells and history.** New `-atomic` cell IDs (proposed), and whether
  the existing not-equivalent `-equivalent-` hub cells keep running as
  descriptive cells or are retired.
- **M5. Inclusion trigger for #182.** The list of paths whose change puts the
  cells in the release qualification (§3.4), and whether #87's impact
  estimate includes the `events` cell if #87 touches its path.
- **M6. Criteria.** Confirm the 25% tolerance and the B4 interval rule of the
  frozen counterparts, and the gate check with its negative control as a
  conformance condition (a failure stops the probe).
- **M7. Disposition text.** If (b), (c) or (d), or if the path is unchanged at
  the #182 freeze, approve the §5 text (with its bracketed variant) for the
  v1.2.0 release notes and the #180 ledger.
