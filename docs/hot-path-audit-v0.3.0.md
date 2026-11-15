# Hot-Path Allocation and Contention Audit (v0.3.0)

Date: 2026-11-14. Scope: allocations, cloning, copying, lock contention and
avoidable work on request, mutation, query, import, feeder and metrics paths
of `knotrel-core` and `knotrel-server`. No dependency was added. HTTP bodies,
headers and status codes, serialization, metric series, configuration and
error messages are unchanged.

This note went through an independent review pass. Each change was checked
again against the baseline (`e1e1181`). The final code was then measured in a
dedicated campaign with interleaved baseline, candidate and same-binary A/A
arms (see [Evidence](#evidence)). One optimization, cached HTTP metric
handles, was **reverted** because the evidence did not justify its
complexity.

## Verification

Host: Apple M3 (8 cores, 16 GiB), macOS 26.5.2, AC power. Toolchains: rustc
1.100.0 (`0ef1bc386`) and 1.98.1 (`48a229cea`).

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets [--all-features] --locked -- -D warnings` (1.100.0) | pass, both feature sets |
| `cargo test --workspace --locked` (1.100.0) | 324 passed, 0 failed, including doctests |
| `cargo test --workspace --all-features --locked` (1.100.0) | 453 passed, 0 failed, including doctests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` | pass |
| Core examples `basic`, `bulk_import`, `engine_selection` | pass |
| `cargo build --workspace --release --locked` + `scripts/smoke-test-release-binary.sh` | pass |
| `cargo +1.98.1 test -p knotrel-core --locked` | 60 passed |
| `cargo +1.98.1 clippy -p knotrel-core --all-targets --locked -- -D warnings` | pass |

The table above is the verification of the candidate that still contained the
metric-handle cache. The final tree restores the baseline `metrics.rs` with two
corrected comments and was re-checked with the same commands; see
[Final-tree re-verification](#final-tree-re-verification). The measurement
campaign built and measured that final tree.

## Evidence

All measurements in this note come from one campaign in the sibling
`knotrel-benchmarks` repository:
[`results/hot-path-audit-20261114/`](../../knotrel-benchmarks/results/hot-path-audit-20261114/README.md). Its tables are generated
from the archived raw data and can be regenerated and checked byte for byte
with `python3 results/hot-path-audit-20261114/analyze.py --check`.

| Artifact | sha256 |
| --- | --- |
| [`REPORT.md`](../../knotrel-benchmarks/results/hot-path-audit-20261114/REPORT.md) | `a0e06a1922140676d4d5a27bb1415ae80ae2a52a2e1bbdbaa8d9077cb2866f72` |
| [`analysis/summary.json`](../../knotrel-benchmarks/results/hot-path-audit-20261114/analysis/summary.json) | `746dd29c5db7104b8dae1af8b31b3b37038e6cc8c46fd3396d5807f2938ce82d` |
| [`manifest.json`](../../knotrel-benchmarks/results/hot-path-audit-20261114/manifest.json) (every campaign file with its hash) | `257d6434e7404c9a9f53ac49ece2e3966f7e1c54008004d5673d5200271cc3ac` |

- **Identities.** Baseline: `e1e11818a762baf07575719456a2158959b2e2c8`,
  materialized with `git archive`. Candidate: baseline plus the code-only
  patch whose `git diff HEAD -- . ':(exclude)docs'` sha256 is
  `8b579f7d8a8db52d499263870b486d0bbaec1775135c4bb50f8e9005a55cc9d7`; every
  candidate file was checked against the working tree. The campaign's
  [`provenance/reconstruct_verified_tree.sh`](../../knotrel-benchmarks/results/hot-path-audit-20261114/provenance/reconstruct_verified_tree.sh)
  shows that this is the code verified below (recorded hash
  `b17d7018…20ea36`, `MATCH`). The reverted metric-handle cache is not in the
  candidate.
- **Host and schedule.** Apple M3 (8 cores, 16 GiB), macOS 26.5.2, AC power,
  Rust 1.100.0 for every build, 2026-11-15. Arms `base`, `cand` and `aa` (a
  byte-identical copy of `base` under another path), 12 rounds in two
  sessions, every arm order equally often.
- **Workloads.** Core bulk initialization (`initialization-profile`,
  `facade-limited`, the server's import path) and dynamic link/cut/connected
  workloads for `compact-bfs`, `ett-scan` and `hdt`; an in-process probe that
  drives the full Axum router through `tower::ServiceExt` (extractors,
  middleware, worker admission, the blocking job and response construction,
  no sockets) with mimalloc, metrics off and on; and a separate end-to-end
  HTTP section.
- **Allocation counts** come from a separate probe build with a counting
  wrapper (`stats_alloc`) around the system allocator. They are counts, not
  timings.
- **Statistics.** For each round, the percent change against `base` in the
  same round; the median of those paired changes and a 95% bootstrap CI
  (10,000 resamples). The noise bound is the larger magnitude of the A/A
  pair's 95% CI. A change is **confirmed** only when its CI excludes zero and
  its median exceeds the noise bound; otherwise it is **inconclusive**. There
  are 42 in-process and 9 end-to-end tests without a family-wise correction,
  so a marginal verdict can occur by chance.

Correctness was checked in the same campaign: trace fingerprints identical
across arms for all 12 timed core cells, the initialization oracle 12/12,
router response digests identical for 12/12 scenario and metrics
combinations, and end-to-end oracle agreement 18/18.

## Confirmed Improvements

The probe measures every change together, so the time changes below are joint
effects, not attributions to single items. Times are medians with the
interquartile range width.

| Scenario (metrics off) | Allocations/request: base → cand | Median µs (IQR): base → cand | Paired change, 95% CI | A/A bound | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| Single `connected` | 96 → 80 | 7.05 (0.38) → 7.22 (0.88) | +1.5% [−9.5, +4.7] | ±3.1% | inconclusive |
| 8 concurrent clients, single `connected` | 95.01 → 79.04 | 6.75 (0.28) → 6.86 (0.21) | +1.5% [−1.6, +6.1] | ±10.2% | inconclusive |
| Batch, 1,024 `connected` | 5,223 → 3,161 | 203.6 (2.5) → 187.9 (1.3) | −7.9% [−8.6, −7.0] | ±0.7% | confirmed |
| Batch, 1,024 alternating cut/link | 3,175 → 1,113 | 172.6 (1.8) → 156.7 (1.9) | −9.5% [−10.8, −8.8] | ±0.7% | confirmed |
| Import, 100,000 nodes and 99,999 edges | 400,152 → 100,127 | 11,074 (142) → 8,816 (184) | −21.1% [−22.1, −18.7] | ±1.7% | confirmed |
| Feeder batch, 1,024 events | 10,358.76 → 4,189.53 | 352.2 (6.9) → 301.1 (2.6) | −14.3% [−15.3, −13.5] | ±1.5% | confirmed |

With metrics on, allocations are 115 → 96, 114.01 → 95.03, 5,242 → 3,177,
3,194 → 1,129, 400,178 → 100,148 and 10,379.76 → 4,205.53. The four batch,
import and feeder time changes are confirmed at −6.4%, −9.1%, −21.6% and
−18.6%; the two single-request scenarios are inconclusive (−4.7% and −1.3%).
At about 7 µs per request, 16 fewer allocations do not separate from the A/A
noise of this host.

Core initialization, median ms (IQR width), base → cand:

| Engine | Workload | Base | Candidate | Paired change, 95% CI | A/A bound | Verdict |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| `compact-bfs` | chain, 200,000 nodes | 12.03 (0.26) | 11.32 (0.29) | −6.3% [−7.0, −5.0] | ±0.7% | confirmed |
| `compact-bfs` | blocks, 100,000 nodes | 5.81 (0.08) | 5.38 (0.09) | −7.3% [−9.6, −6.9] | ±0.8% | confirmed |
| `ett-scan` | chain, 200,000 nodes | 236.4 (16.9) | 228.4 (17.1) | −2.9% [−5.8, +0.6] | ±2.9% | inconclusive |
| `ett-scan` | blocks, 100,000 nodes | 106.8 (5.6) | 101.9 (5.7) | −3.9% [−6.2, −2.3] | ±2.7% | confirmed |
| `hdt` | chain, 200,000 nodes | 234.5 (17.0) | 235.1 (8.9) | −1.7% [−5.7, +2.4] | ±1.8% | inconclusive |
| `hdt` | blocks, 100,000 nodes | 107.5 (4.6) | 103.2 (4.4) | −4.0% [−7.2, −2.2] | ±1.4% | confirmed |

`compact-bfs` improves as well: the replaced reflexive `connected(x, x)` made
two ID lookups per endpoint, and `contains_node` makes one.

Dynamic workloads (10,000 nodes, 200 rounds): one confirmed improvement,
`ett-scan` sustained-churn `link_total_ns` −3.5% [−5.5, −1.4] against a
±3.5% bound (marginal). Of the other 23 dynamic timings, 22 are
inconclusive and one is the regression below.

End-to-end, the sampled maximum RSS was lower: base median 49.57 MiB,
candidate 47.13 MiB, a paired change of −4.7% [−9.0, −3.5] against an A/A
bound of ±4.5% (marginal). This is consistent with the import releasing its
body buffer before building the graph.

### Confirmed regression (did not reproduce)

Dynamic `compact-bfs` sustained-churn `connected_total_ns`: +2.1%
[+0.6, +4.2] against an A/A bound of ±2.1% (base 5.22 ms, candidate 5.35 ms).
No code on the `compact-bfs` query path changed; the patch only adds
`Graph::contains_node`, used by the limited facade's limit checks. The same
cell's whole-workload time is inconclusive (+1.8%, bound ±2.2%). With 42
tests and a margin of 0.08 percentage points over the bound, this is
consistent with chance or code layout, but it is reported as measured.

**Re-check (2026-11-15).** A pre-specified re-measurement with the campaign's
exact binaries (verified by sha256), 24 counterbalanced rounds and the same
verdict rule —
[`hot-path-recheck-20261115`](../../knotrel-benchmarks/results/hot-path-recheck-20261115/README.md),
[`REPORT.md`](../../knotrel-benchmarks/results/hot-path-recheck-20261115/REPORT.md)
sha256 `ec2b2cd979b97881d863718aee4653002c70533a146aca6c950924a969312d9e`,
[`analysis/summary.json`](../../knotrel-benchmarks/results/hot-path-recheck-20261115/analysis/summary.json)
sha256 `a9e4401108e554c25db63f42a56bde410c89306bc916d96e62855309b7cc6923` —
measured `connected_total_ns` at +0.83% [+0.10%, +1.78%] against a ±2.14% A/A
bound: **inconclusive**, so the regression does not reproduce. Whole-workload
time was +0.16% [−0.46%, +1.33%]. A slowdown below this host's noise floor
cannot be excluded.

## Inclusion decision (v0.4.0, roadmap Feature 0.4.1)

Decided 2026-11-15 on the evidence above. **Every kept change in "What each
kept change does" is included in v0.4.0.** Each one was reviewed (see
[Review Findings](#review-findings)), leaves client-visible contracts and
results unchanged (identical correctness checks across arms), and is supported
by a confirmed timing improvement, a deterministic allocation reduction or
mechanism evidence. No confirmed regression remains unexplained: the single
one did not reproduce. The cached HTTP metric handles stay reverted. Single
changes were not ablated individually in the campaign, so timing gains are
reported only as joint effects.

The v0.4.0 tree adds M6.2 review fixes and M6.3 logging on top of these
changes; the hot-path changes themselves are unchanged since the campaign.
The metrics-on cost of that tree is qualified in the observability contract,
section 18; its regression qualification against v0.3.0 is roadmap 0.4.2.3.

### What each kept change does

Mechanism figures are medians from the campaign's isolated microbenchmarks,
which replicate the server pattern without calling Knotrel code.

| Change | Mechanism evidence |
| --- | --- |
| `ConnectivityGraph` limit checks use `contains_node` (one expected O(1) ID lookup) instead of `connected(x, x)` (two lookups; O(log V) root walks on ETT/HDT). | Core initialization table. |
| ETT/HDT `link` and HDT `add_node`: one entry-API probe per endpoint and per edge key, instead of three or four lookups. | Part of the ETT/HDT rows. |
| `WireNode` parses the borrowed JSON string: no `String` per node ID. | 2 allocations fewer per link, cut or `connected` operation; about 3 per import node and edge. |
| `WireOp` resolves feeder `op` while parsing; unknown names keep their text for the unchanged validation error. | 1 allocation fewer per event. |
| `Vec::from(Bytes)` instead of `to_vec()`; the import buffer is dropped right after parsing. | 1 MiB body: 17.6 µs to copy versus 0.074 µs to convert a uniquely owned buffer; lower end-to-end peak RSS. |
| Applied feeder events: `clone_from` for the retained position, moved strings for the dedup entry. | Part of the 6 allocations fewer per event. |
| `AppliedStamp`: one clock read per feeder request instead of per applied event, on every exit path, under the lock. | One clock read costs 20.4 ns, about 21 µs per 1,024-event batch. Request-level ablation: prior evidence only, see below. |
| Outstanding jobs: `AtomicUsize` plus a `Notify` signalled when the count reaches zero, instead of `watch::send_modify` (a write lock and eight notifier mutexes per update). | One track/release pair: 83.0 ns → 7.3 ns on one thread, 880 ns → 104 ns with four threads. |
| `Service(Arc<ServiceInner>)`: one reference count per per-request state clone instead of seven plus one or two string copies; workers capture one clone. | Part of the 16 allocations fewer per request. |
| Response metadata headers take owned strings (`HeaderValue::try_from(String)` performs the same validation as `from_str`, without a copy); `HeaderValue::from(u64)`; source position encoded once. | Part of the 16 allocations fewer per request. |
| Logging middleware logs known routes from their static path; the timeout layer pins the handler future on the stack instead of boxing it. | 1 allocation fewer per request each (the second only with metrics on). |

## Reverted

**Cached HTTP metric handles** (`OnceLock` per label set instead of
`Family::get_or_create`). The mechanism is real: in the campaign's
microbenchmark, 63.1 ns → 1.9 ns per lookup on one thread and 273 ns → 24 ns
with four. The decision to revert rests on **prior evidence**, not on this
campaign: a request-level ablation in the earlier screening campaign (archived
outside both repositories in the workspace's `evidence/hot-path-audit-20261114/`)
did not separate from noise with metrics on (single +2.1% [−1.1, +3.8],
concurrent +1.1% [−4.2, +1.7], batch `connected` +0.1% [−0.9, +0.6]); its one
nominally confirmed cell came from metrics-off runs, where the cache never
executes. About 100 lines of indexing and caching were not justified, so the
baseline implementation is restored. The campaign measures the reverted
tree.

**Prior evidence for `AppliedStamp`.** The same screening campaign ablated
`AppliedStamp`: reverting it cost +6.6% [+3.0, +7.8] on the 1,024-event batch
with metrics on (A/A ±2.1%). That ablation was not repeated in this campaign;
the change is kept on the strength of the mechanism and the joint feeder
result above.

## Inconclusive Measurements

- Single-request router scenarios (one client and eight concurrent clients,
  metrics off and on), despite 16 fewer allocations per request.
- `ett-scan` and `hdt` chain initialization.
- 22 of the 24 dynamic core timings (all but the confirmed `ett-scan` `link`
  row and the regression above).
- End-to-end import, catch-up, time-to-ready, query QPS, latency p50/p95,
  final RSS and restart recovery: 6 rounds with an A/A arm, and with
  `Connection: close` TCP setup dominates request timing.
- Single changes inside the joint gains were not ablated in this campaign.
- Earlier numbers in the screening archive (`evidence/hot-path-audit-20261114/`,
  including a shared 2-vCPU Linux container with A/A variation up to 25%) are
  superseded by this campaign and support no claim beyond the prior evidence
  labelled above.

## Review Findings

- **Job counter.** The drain loop creates `Notified` before reading the count.
  Tokio delivers every `notify_waiters` call made after a `Notified` is
  created, even before it is first polled, so a drop to zero after the read
  cannot be missed. A drop before the read is seen as zero. Every decrement
  has a matching increment, guarded by `debug_assert`. Completion still
  requires zero jobs and no open connection tasks, as before.
- **`Arc<ServiceInner>`.** Workers hold a `Service` clone instead of
  separate `Arc`s. Permits, job guards, `WorkerPanicGuard`, `PendingOutcome`,
  the failure flag and lock and hook order are unchanged. The join-error path
  sets the same `unavailable` flag.
- **Zero-copy parsing.** Buffers live only until parsing returns, because
  deserialization yields owned values. Content-type, body-limit and parse
  order are unchanged. `wire_node_json_contract_is_preserved` and
  `feeder_event_op_json_contract_is_preserved` pin accepted inputs and every
  client-visible message.
- **Headers.** `TryFrom<String>` and `from_str` share `try_from_generic`
  (http 1.5): same byte validation, same bytes, and `is_sensitive` false.
  Metadata is still captured under the graph lock with the outcomes.
- **Feeder cloning.** Strings move only after the last read of the event.
  Duplicate, partial-failure, retry and desynchronization paths read the
  event before that point and are unchanged.
- **ETT/HDT lookups.** Self-loop, node-limit, edge-limit precedence and the
  check-before-mutation order are unchanged; direct `link` still registers
  endpoints before reporting a duplicate edge.
  `generous_limits_never_change_outcomes_of_mixed_updates` compares the
  limited and unlimited facades operation by operation.
- **Logging.** Registered paths log the identical string. Unmatched paths log
  an owned copy of the request path, as before.
- Fixed during review: one accidental, behaviour-neutral edit in the import
  worker's telemetry match was restored.

## Intentionally Retained Costs

- **Internally tagged `Operation`.** Serde buffers each operation object,
  about one allocation per operation. A hand-written deserializer would have
  to reproduce serde's missing, duplicate and unknown field messages exactly.
- **Batch response serialization.** `axum::Json` grows its buffer from 128
  bytes. Pre-sizing it needs a serializer outside axum: a new direct
  dependency, or `simd-json` output whose escaping is not proven identical.
- **Feeder strings.** `event_id`, `source_position` and
  `prev_source_position` stay owned, because parsing finishes before the
  graph lock is taken. The dedup window keeps the ID both in its ring entry
  and as its map key. The map keeps std `SipHash`, because event IDs are
  client-controlled.
- **HTTP metric family lookups.** Kept after the reverted cache (see above).
  Histogram observations also take `prometheus-client`'s internal lock.
- **Per-request fixed work.** One `spawn_blocking` task, one Tokio timer, an
  `Arc<AtomicBool>` timeout marker (metrics on), `vec![operation]` for single
  operations, and the decimal version string.
- **Info-level request logging.** One formatted event per request at the
  default filter. It is an operational contract; operators can raise the
  level with `RUST_LOG`.
- **Startup and error paths.** Error messages (`format!`, `to_string`),
  service construction, registry setup and probes allocate, but run once or
  only on failure.

## Future Proposals Requiring Public API Changes

- **Reusable Compact BFS scratch in the server.** `ConnectivityGraph::connected`
  on `compact-bfs` allocates a bitset and a queue per query: 2 allocations,
  about 2.4 MB per 1,024-query batch on 10,000 nodes. Reusing `BfsWorkspace`
  needs a public `ConnectivityGraph` method accepting caller-owned scratch,
  and per-worker storage under the graph mutex. Internal BFS changes were
  already evaluated and parked (visited-put, FIFO); this needs its own
  measured proposal.
- **Capacity hints for bulk construction.** `ConnectivityGraph` cannot
  reserve vertex capacity before an import, so ID-map and arena growth stay
  amortized. This needs a public constructor taking expected node and edge
  counts.
- **Shared reads.** Queries take the same exclusive mutex as writes and set
  the failure marker. A read-write lock would change batch isolation and
  poisoning semantics, which are documented server behaviour. It needs a
  concurrency design and benchmark.

## Final-tree re-verification

The final tree, with the metric-handle cache reverted, was re-checked on the
same host and toolchains. The working-tree diff hash recorded by the run
(`b17d7018…20ea36`) matches the delivered tree; the campaign's
`provenance/reconstruct_verified_tree.sh` reproduces it from the current
checkout. The campaign repeated the repository checks of
`knotrel-benchmarks` and its probes, not of `knotrel`.

| Check | Result |
| --- | --- |
| fmt; Clippy (default and all features, all targets, `-D warnings`) | pass |
| `cargo test --workspace --locked` (1.100.0) | 323 passed, 0 failed, including doctests |
| `cargo test --workspace --all-features --locked` (1.100.0) | 452 passed, 0 failed, including doctests |
| Rustdoc (`-D warnings`), core examples, release build and smoke test | pass |
| `cargo +1.98.1 test` / `clippy -p knotrel-core` | 60 passed / pass |

Each suite has one test fewer than the measured candidate: the cache's
label-slot unit test was removed together with the cache.
