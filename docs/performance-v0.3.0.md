# v0.3.0 performance evaluation plan

Status: proposed for independent review, 2026-10-24. Planning only: no campaign has been run and no performance acceptance is implied. M3.1 remains pending independent acceptance. This plan adds no persistence or algorithm redesign.

## 1. Reviewed baseline and existing assets

Local references inspected: Knotrel HEAD `90e71ae09e5d0c4f1679245c45bc0dea4ba0e750`, released `v0.2.0^{commit}` `7e9becd54040b9d7f05e2b548e5f86ee3f3e0915`, benchmark HEAD `df635a1e872fd9e2d556b166ac23f342ee45d675`. Both working trees were clean before this documentation change. Resolve and record references again before execution; never replace these with an ambiguous branch name. Local tag resolution is not independent verification of a downloaded release artifact.

Paths below beginning `bench/` refer to the sibling `knotrel-benchmarks` repository. Its README, AGENTS.md, methodology, scenario contracts, historical results policy, collectors and CI were inspected alongside Knotrel's README, AGENTS.md, roadmap, server example and CI.

| Asset | Actual boundary and disposition |
| --- | --- |
| `bench/docs/methodology.md`, Rust harness | Synchronous embedded calls with clock/barrier overhead; validation outside individual call timers. Workload wall time includes dispatch/checks/sample storage, excludes setup and sorting. Preserve setup_ns separately. Not HTTP throughput. |
| `bench/scripts/run_baseline.py` | Deterministic families, fresh/warmed regimes, process trials, rotated backend order, source/binary/artifact fingerprints and manifests. Reuse. |
| `bench/scripts/run_scale.py` | Sustained/dense workloads, isolated processes and platform time-tool peak RSS; RSS includes trace, samples, engine and warmup. Reuse with existing summarize_scale.py and summarize_comparison.py. |
| Initialization collectors / initialization-profile | Separate generation, encoding and construction diagnostics; bulk facade whole timing differs from adapter setup. Preserve calibration/A-A records and their approved, experiment-specific thresholds; do not transfer them to release gates. |
| `bench/results/ranking/README.md` | Historical 74-cell five-engine ranking, three process trials per engine/cell. Best-engine-per-cell aggregates are an oracle selection envelope, not a deployable default. Reproduce cells, retain per-engine losses. These are NOT released v0.2.0 HTTP measurements. |
| Hardware profiles and historical results | Apple M4 profiling and initialization evidence have different boundaries. Historical directories removed from the active tree remain in Git history: pin owning commits and artifact hashes when retrieving them. Never rewrite records. |
| `bench/scripts/run_perf_evaluation.py` | Exploratory convenience script: absolute temporary Cogentco path, assumed existing target/release executable, fixed order, median wall time but last-repetition operation p50, missing engines default to zero, overwritten results/perf_evaluation_latest.json. Not a release gate. Reuse collectors instead of making another runner. |
| `scripts/measure-e2e.sh`, `examples/measure_e2e.rs` | Existing source-managed HTTP lifecycle driver; reuse/extend. Script defaults to dev, builds server/example without --locked, emits timestamped Markdown under target. Spawn clears environment, selects source-managed mode and implicitly the default engine; it does not provide the proposed full configuration matrix. |
| `measure_e2e` timings | Fixture/oracle preparation separate; import client call includes transport/serialization/server work, not isolated candidate build/publication. Catch-up includes sequential event calls. Current total_sync begins after discovery, before registration, and ends after barrier, before readiness verification: not spawn-to-ready. Query call latency excludes later oracle checks; loop throughput includes response parsing/bookkeeping. Recovery has its own enclosing interval. Preserve these distinctions. |
| Existing memory measurement | ps RSS polling every 25 ms, sampled maximum and final sample, not guaranteed peak or engine heap. No comprehensive CPU or lock/queue decomposition. |
| External feeder and E2E tests | Protocol/correctness fixtures, lost-ACK, fencing, barriers and rebuild scenarios are reusable. Tests are not performance results. |
| CI | Core/server CI covers multiple OSes and test-support; benchmark CI uses sibling core_ref, default main, and a small release smoke. Resolve core_ref to SHA in evidence. Shared-runner timings are not performance gates. |

Core matrix: compact-bfs, compact-workspace, ett-scan, hdt, petgraph-dfs. Server configuration supports compact-bfs, ett and hdt only. Workspace and petgraph are not HTTP engine selectors. Preserve experimental labels. No GraphScope or other HTTP competitor claim without an actual equivalent adapter.

## 2. Architecture and minimal ownership

Keep embedded measurement, deterministic trace generation/import, source provenance, core comparison and campaign manifests in the Apache-2.0 benchmark repository. Keep HTTP protocol driving in Knotrel's existing Elastic-2.0 measure_e2e example and external feeder helpers. Extend those tools rather than copying an HTTP protocol implementation into a new harness. Orchestration/reporting in the benchmark repository may invoke a pinned driver executable and explicit server binary over public HTTP; it must not depend on private server internals.

Do not copy licensed implementation code across repositories without preserving its terms. Protocol-neutral data can be shared with explicit provenance. Reuse existing generated trace schema 1 and validated import envelope; they are different formats. A thin versioned mapping manifest should describe original node IDs, decimal-string HTTP IDs, operations, expected outcomes, initial isolates, source-event sequence/cursors and request batch boundaries. Store original and mapped SHA-256 hashes. Do not invent a universal replacement trace schema or lose u64 precision in JSON consumers.

Minimal future changes, in order:
1. Benchmark collector preflight: explicit immutable source/server/driver identity, target path checks, non-overwriting evidence, portable --trace-in, complete repetitions and failures. Reuse collector helpers; retire the convenience script from qualification or make it delegate.
2. Extend measure_e2e with explicit server-binary/profile/engine/configuration, machine-readable raw records, preserved stderr, transport policy, finite timeouts and clean process teardown. Keep existing lifecycle scenario.
3. Add standalone and controlled concurrent load modes, resource sampling and request records. Add tests for framing, schema, invalid metrics and sample accounting before timing claims.
4. Only after M6, correlate opt-in server instrumentation for queue/lock/build/publication with black-box timings. Before then report unavailable, never estimate phases by subtracting unrelated medians.

## 3. Checkpoints

All checkpoints use the matrix, metric definitions, provenance and repeat protocol below. Full matrices are planned manifests, not an unbounded Cartesian product.

| Checkpoint | Versions and coverage | Required evidence / exit |
| --- | --- | --- |
| P0: released baseline | Exact v0.2.0 SHA above, fixed benchmark/driver SHA. All five core engines and three HTTP engines; standalone and source-managed cells. Start bounded pilot then approved full manifest. | Validated source linkage, A/A noise pilot, raw baseline, unsupported phase list, correctness passed. Must precede comparisons of new execution behavior. |
| P1: M3/M4 regression | P0 plus exact candidate SHA after execution/batch changes and again after lifecycle/deployment changes. Retest affected cells and fixed sentinel core cells; full baseline remeasurement for important regressions. | Batch-size, mixed traffic, admission, shutdown/rebuild results; preserved regressions and reviewed tradeoffs. M3.1 documentation alone does not imply a speedup. |
| P2: M6 and M5 evidence | P0 plus fixed pre-M6 and post-M6 revisions. Same-revision instrumentation off/on where supported. Measure coarse mutex before any alternative. | Instrumentation overhead, series cardinality, queues/lock waits, reader tails and per-client fairness. Any concurrency candidate must rerun identical cells against coarse mutex; no-change is valid. |
| P3: release qualification | Pinned v0.3.0 release candidate versus freshly rerun v0.2.0 on same host/toolchain/driver; retain P0 historical comparison separately. | Full required manifest, all supported engines, reviewed resource ceilings, no unresolved correctness failure, reviewed reproducible regressions and explicitly listed unsupported/waived cells. Archive evidence before tagging. |

Primary controlled platforms: dedicated macOS ARM64 and Linux x86_64, separately analyzed. P0 begins on one named available host; P3 needs both or a documented owner-approved coverage exception. Windows retains correctness/CLI smoke until resource collector support is qualified. Never combine architectures in one speedup number.

## 4. Workload matrix

Reuse historical names and fingerprints, seed 42 for continuity. Add seeds 0 and 2026 as separate robustness cells in qualification; do not silently replace historical traces.

| Layer | Mandatory cases | Proposed bounded dimensions |
| --- | --- | --- |
| Core sparse | chain-split-rejoin-v1, cycle-alternatives-v1, components-join-split-v1, hub-alternatives-v1 | 1,024 / 10k / 100k / 1M vertices, initial/setup plus replay |
| Core persistent churn | sustained-churn-path-v1, sustained-churn-blocks-v1 | 1k / 100k / 1M; 10/50/90% queries, 99% supplement |
| Core dense | dense-bridge-churn-v1, redundant-bridge-churn-v1 | 128 / 512 vertices; 10/50/90% queries; record actual edges/density |
| Imported topology | existing Topology Zoo import and Cogentco historical trace | Pin source/license/projection/hash. Recover historical trace from Git or regenerate with provenance; do not rely on /private/tmp. Outage replay is synthetic unless event provenance proves otherwise. Contacts/AML are prospective, not existing coverage. |
| HTTP standalone | query-only; single link/cut; legacy batches; 10/50/90% mixed reads | small 1k, medium 10k, large 50k initial tiers; batches 1/8/64/256/1024; clients 1/2/4/8/16/32 |
| HTTP source-managed | registration, heartbeat, initial import, ordered events, no-op/duplicate/progress, barrier, queries, restart/rebuild | Existing small/medium/large fixture counts (1k/2k, 10k/20k, 50k/100k vertices/edges); event batches 1/8/64/256/1024, one ordered feeder writer, concurrent readers |
| Interference | readers during updates, candidate import and replacement; repeated session/rebuild cycles | small and large tiers, readers 1/8/32, fixed writer schedule; compare before/during/after windows |
| Saturation | increasing offered load with fixed admission/resource settings | Open-loop rate steps selected by pilot, finite outstanding cap; record offered/achieved rates and dropped arrivals, no uncontrolled queue growth |
| Scale supplement | server 100k and 1M sparse vertices; core 10M optional | Only after memory/time budget preflight, payload-limit review and owner-approved resource ceiling. Dense size is never inferred from sparse node count. |

Freeze a manageable coverage manifest: one-factor sweeps around the 10k, 50% query, 8-client anchor plus small/large corner cells and complete historical core matrix. Required dimensions must be represented; mark unrun combinations explicitly. Do not claim complete cross-product coverage. Timeouts/OOMs are retained observations, never silently omitted wins.

## 5. Correctness, load and cache protocol

Validate deterministic expected answers independently of measured engines. Keep oracle computation outside timed operation intervals. Validate every completed response, changed flag, final topology, generation/revision/source frontier and readiness transition; correctness mismatches invalidate performance qualification.

Concurrent queries must be checked against the graph state identified by their response version, not against an arbitrarily later final graph. Use one ordered writer for exact replay; for multiple independent writers use disjoint commuting mutations or record enough ordering evidence to validate histories. Mixed legacy batches and feeder batches have distinct failure policies. Preserve generation/session fencing, no-op revision behavior and bounded deduplication; do not advance checkpoints beyond acknowledged applied state. Ambiguous mutations remain uncertain and require reconciliation/rebuild, not blind retry. Measure the recovery separately.

Fresh means a new process/graph without deliberate replay warmup, NOT flushed CPU/page caches. Warmed means a fixed predeclared untimed replay/setup then the measured graph regime; record which state is rebuilt and which buffers are reused. Immediate query repeats are a separate locality workload, not evidence of answer caching. Record query selection and repeat ratio. No undocumented cache flushes.

Use release builds, fixed Rust 1.98.1, fixed dependency context and no test-support in timing binaries. Compile and validate before measurement. Record power source/power mode, thermal constraints, host background load and sleep events. No concurrent builds/profilers. Stop and retain invalidated trials on sleep or identity changes; do not selectively rerun slow samples.

Proposed pilot: 6 independent A/A process pairs per anchor cell, balanced order, fresh and warmed separate. Qualification starting schedule: 10 baseline/candidate process pairs per cell, alternating AB/BA with recorded schedule seed, across at least two sessions. These counts are proposals, not confidence guarantees. Within-process repetitions are correlated observations, not independent trials. Freeze final counts after pilot, before examining candidate wins. For HTTP steady-state cells propose 10 s warmup and 30 s measurement; retain operation counts and extend the predeclared duration if tails are underpowered. Report p99 sample count/tail count; do not extrapolate rare-event precision.

Closed-loop fixed clients measures achieved throughput and latency under backpressure. Separate open-loop saturation uses scheduled arrivals; record scheduling lag and both send-to-response and scheduled-arrival-to-response latency to expose coordinated omission. Preserve connection reuse/new-connection policy, HTTP version, payload bytes, network location, driver CPU and resource use. A saturated load generator invalidates server-capacity conclusions.

## 6. Metrics and boundaries

All durations are raw nanoseconds with display units explicit; RSS is bytes. Preserve percentile estimator and sample count. Summarize independent trial distributions; never average p99s into a purported pooled p99.

- Core: setup_ns, operation count/total/min/p50/p95/p99/max by link/cut/connected, workload_wall_ns. No reciprocal-median throughput claims.
- HTTP: offered, completed and successful requests/s over the same declared wall interval; attempted/executed logical operations/s separately; effective graph mutations/s counts changed:true only. Queries, duplicate and progress events are not effective mutations. State batch size and request/operation denominators.
- Request latency: client send start through complete response body; label connection/serialization inclusion. Retain errors/timeouts separately from successes, with status/code and censored durations; never count timeout as a successful latency sample.
- Import: client import duration and elements/s (vertices+edges) with explicit denominator. Candidate construction/publication/lock acquisition unavailable on uninstrumented v0.2.0; later histograms must have exact boundaries and instrumentation overhead controls.
- Catch-up: source records/s, effective mutations/s, interval start/end, duplicate/no-op/progress counts. Barrier HTTP duration separate. Session and heartbeat latency/error rates separate.
- Readiness: preserve existing registration-to-barrier duration under its actual name. Add spawn-to-first-successful-ready, load-start-to-ready and restart-to-ready with polling interval stated. Include source retrieval only in a separately labeled source-inclusive recovery measure.
- Queue wait, lock wait and graph execution: absent until independently instrumented; client response time cannot identify them. Record admission limits and outstanding jobs where observable.
- Resources: whole-server CPU user/system time and wall-normalized CPU usage (state cores convention), driver CPU separately; platform peak RSS and sampled RSS series distinguished. ps polling has cost and misses brief peaks; zero/missing must not mean zero memory. Replacement overlap may dominate RSS. Core process RSS includes the harness. Do not subtract independent peaks to estimate graph heap.
- Fairness: per-client completions, p95/p99/max and longest observed starvation interval; writer progress and heartbeat health while readers saturate service.

## 7. Safe P0 execution and source isolation

Use isolated checkouts after plan approval; do not switch the active M3.1 checkout or edit its Cargo files. Prepare two independent parent directories, each containing sibling `knotrel` and `knotrel-benchmarks` checkouts. Baseline knotrel is the exact released SHA above; candidate is an explicitly selected SHA. Both benchmark siblings use the SAME approved harness SHA. Native managed worktrees are preferred where they can preserve the sibling layout; otherwise use isolated local clones/checkouts with verified paths. No worktrees are created by this planning task.

Before building, save `git rev-parse HEAD`, status (including untracked files), Cargo.lock hashes and `cargo metadata --locked --format-version 1` for each arm. Assert the resolved knotrel-core manifest canonical path is inside that arm's sibling checkout, and its SHA matches the manifest. Reject external symlinks or accidental resolution to the active workspace. Use distinct absolute target directories per arm; run release --locked builds from explicit directories. Save rustc -Vv, Cargo version, complete build commands/flags, executable SHA-256 and resolved artifact paths. Never infer identity from target/release filename or crate version alone.

If current harness cannot compile against v0.2.0, stop: choose a compatible common harness revision or review a minimal benchmark-only compatibility adapter, equally applied to both arms. Never modify released production source to make the benchmark work. Path-package version changes may require a derived lockfile in the isolated benchmark checkout: preserve original, record exact diff/hash, keep third-party versions fixed and review it before proceeding; --locked failure must not be bypassed silently.

The HTTP driver must be identical across baseline/candidate and accept an explicit independently built server executable. Existing auto-discovery must not choose the candidate binary for baseline runs. A same-revision P0 pilot can use released tooling at its known boundaries, but richer comparisons require the common driver extension first. Label source-built released baseline separately from an official downloaded binary; archive release artifact checksums if the latter is used.

First future execution task for Gemini: establish the isolated released-SHA layout, prove Cargo path resolution and binary identity, validate a tiny trace on all five core backends and the existing small source-managed lifecycle. Produce an immutable preflight manifest and a tiny evidence bundle. This is a correctness/collector smoke, NOT the full P0 campaign or a speedup claim. Stop for review before approving pilot resource budget and larger runs.

## 8. Evidence schema and reporting

Use new directories `results/v0.3.0/<checkpoint>/<campaign-id>/{core,http}/` in the benchmark evidence store (large artifacts may be external with immutable URI/hash). Do not retain sole evidence under disposable target/ or overwrite latest files. Maintain a checked-in small index pointing to immutable bundles; preserve historical records at their original hashes.

Proposed campaign schema v1:
- manifest: campaign ID/checkpoint, timestamps, scope, status, baseline/candidate/driver/harness full SHAs and dirty flags, patches and untracked-source inventory if exploratory, lockfile hashes, binary hashes, build profiles/features, OS/kernel/architecture/CPU/RAM, power context, tool versions, explicit server config, engine selector/implementation ID, transport topology, seed, original/mapped trace SHA-256 and FNV, fixture/projection/license IDs, planned case/trial order, repetitions/warmup/concurrency/duration, timing definitions and unavailable metrics.
- per trial: case/arm/process IDs, command/environment allowlist, start/end, raw output/stderr, exit code, validity reason, request samples or lossless histogram with estimator, status/error counts, operation outcomes/frontier evidence, resource samples and CPU/peak scope.
- summary: per-cell baseline/candidate distributions, paired deltas, sample counts, exclusions with reasons, correctness verdict, regressions, inconclusive cells, coverage/waivers and reviewer decision.

Schema consumers reject missing required engines, duplicate IDs, incompatible trace/config identities, impossible counters and absent denominators. Unknown/unsupported metrics use null plus reason, not zero. Hash all artifacts and write a completion marker only after successful validation; preserve partial manifests on failure. Keep raw samples where affordable; histogram-only storage must preserve tail resolution and disclose loss of request correlation. No secrets in archived environments.

For latency/runtime/RSS report `100*(candidate/baseline-1)` (positive is worse); for throughput the same formula means positive is better. Also show absolute values. Report speedup baseline/candidate only with its metric named. Per-engine versus petgraph rankings are core-only. Any geometric mean must name fixed cells/weights and retain each regression; best-engine envelopes cannot masquerade as configured-server performance.

## 9. Review policy and release gates

Correctness, stale binary/path identity, invalid protocol, missing raw evidence or broken accounting blocks qualification. An expected rejection in saturation is not itself correctness failure, but must appear in completion/error denominators. Reproducible regressions need investigation and an explicit accepted tradeoff or correction; benefits elsewhere do not erase them.

No global percentage threshold is approved here. Pilot A/A estimates noise per metric/workload/host. Compare paired trial distributions and practical absolute cost, not a single fastest run. Any confidence interval method must state independent unit, estimator and assumptions; small-trial exploratory intervals are not proof of equivalence. Ambiguous/underpowered results are inconclusive, not pass or fail: freeze an additional replication schedule, retain all original observations and escalate remaining release-critical uncertainty to the owner. Do not repeatedly sample until significant.

CI: tiny deterministic trace replay, default/test-support correctness, collector schema/framing/provenance tests and bounded release smoke; no shared-runner speed threshold. Controlled campaigns: dedicated comparable hosts, frozen schedules, measured resource budgets and reviewed per-cell deltas. P3: complete matrix coverage (or explicit owner-approved exceptions), no unresolved correctness/identity defect, regression decisions and reproducible bundle before release approval. Keep coarse mutex unless this evidence justifies an alternative.

## 10. Sequence and open decisions

1. Independent review of this plan and roadmap; M3.1 acceptance remains separate.
2. Provenance/portable collector and explicit driver changes with correctness smoke.
3. P0 A/A pilot; approve host/resource ceilings, final cell manifest, durations and replication counts, then collect P0.
4. M3/M4 scoped implementation and P1; M6 telemetry and P2 overhead assessment; M5 concurrency decision from measured contention.
5. P3 common-host remeasurement, review and release qualification.

Unresolved owner decisions: available dedicated hosts and coverage exceptions; time/RAM/disk ceilings for large cells; artifact storage location/retention policy; acceptable workload-specific performance tradeoffs after variance is measured. None authorizes benchmarks, implementation or synchronization changes during this planning task. No WAL, AOF, local persistent snapshots or crate/version/license changes are included.
