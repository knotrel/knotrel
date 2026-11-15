# Hot-Path Allocation and Contention Audit (v0.3.0)

Date: 2026-11-14. Scope: allocations, cloning, copying, lock contention and
avoidable work on request, mutation, query, import, feeder and metrics paths
of `knotrel-core` and `knotrel-server`. No dependency was added. HTTP bodies,
headers and status codes, serialization, metric series, configuration and
error messages are unchanged.

This note went through an independent review pass. Each change was checked
again against the baseline (`e1e1181`), then measured on the official
toolchains with interleaved baseline, candidate, ablation and same-binary A/A
arms. One optimization, cached HTTP metric handles, was **reverted** because
the evidence did not justify its complexity.

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

The final tree differs from the measured candidate only by the reverted
metric-handle cache, which restores the baseline `metrics.rs` with two
corrected comments. It was re-checked with the same commands; see
[Final-tree re-verification](#final-tree-re-verification).

## Method

Raw data, scripts and analysis output are archived outside the repository in
`evidence/hot-path-audit-20261114/` of the workspace, beside the other
campaign records.

- **In-process probe.** It drives the full Axum router through
  `tower::ServiceExt`: extractors, middleware, worker admission, the blocking
  job and response construction, with no sockets. A counting wrapper around
  the system allocator replaces mimalloc. Allocation counts are deterministic
  and identical in every round. Times are microseconds per request. There
  were 10 rounds, arm order rotated each round, five arms (`base`, `cand`,
  two ablations, and `aa`, a byte-identical copy of `base` under another
  path), with metrics both off and on.
- **Core initialization.** `knotrel-benchmarks initialization-profile
  --surface facade-limited --init-mode bulk --timing whole`, which is the
  server's import construction path. Each sample is the minimum of 3
  repetitions after 1 warmup, over 10 rotated rounds with `base`, `cand` and
  `aa`.
- **Statistics.** For each round, the percent change against `base` in the
  same round, then the median of those paired changes and a 95% bootstrap CI
  (10,000 resamples). The noise bound is the larger magnitude of the A/A
  pair's 95% CI. A change is **confirmed** only when its CI excludes zero and
  its median exceeds the noise bound. Otherwise it is **inconclusive**.
- **End-to-end.** 6 interleaved `measure_e2e --scenario lifecycle --dataset
  large` pairs against the release binaries (mimalloc, loopback TCP,
  `Connection: close`), with no A/A arm.

## Confirmed Improvements

The probe measures every change together, so the time changes below are joint
effects, not attributions to single items.

| Scenario (metrics off) | Allocations/request: base → cand | Median µs (IQR): base → cand | Paired change, 95% CI | A/A bound |
| --- | ---: | ---: | ---: | ---: |
| Single `connected` | 96 → 80 | 7.53 (7.44–8.04) → 7.04 (6.63–7.54) | −8.0% [−11.8, −3.6] | ±1.9% |
| 8 concurrent clients, single `connected` | 103 → 87 | 9.34 (8.72–9.48) → 7.55 (7.34–7.83) | −16.5% [−20.3, −13.2] | ±6.7% |
| Batch, 1,024 `connected` | 5,223 → 3,161 | 245.3 (243.8–250.9) → 216.0 (214.7–220.2) | −11.8% [−14.3, −9.9] | ±2.8% |
| Batch, 1,024 alternating cut/link | 3,175 → 1,113 | 193.2 (191.7–195.8) → 162.3 (161.5–163.1) | −15.9% [−16.4, −15.3] | ±1.0% |
| Import, 100,000 nodes and 99,999 edges | 400,161 → 100,136 | 13,968 (13,896–14,411) → 9,329 (9,204–9,648) | −33.5% [−34.3, −29.8] | ±4.8% |
| Feeder batch, 1,024 events | 10,358.8 → 4,189.5 | 432.1 (429.9–436.9) → 338.0 (336.3–346.4) | −21.4% [−22.2, −20.5] | ±1.8% |

With metrics on, allocations are 115 → 96, 122 → 103, 5,242 → 3,177,
3,194 → 1,129, 400,187 → 100,157 and 10,379.8 → 4,205.5. Every time change is
confirmed, at −10.8%, −19.6%, −12.1%, −15.8%, −34.9% and −25.7%.

Core initialization, median ms (IQR), base → cand:

| Engine | Workload | Base | Candidate | Paired change, 95% CI | A/A bound | Verdict |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| `compact-bfs` | chain, 200,000 nodes | 12.18 (12.11–12.33) | 11.48 (11.30–11.54) | −6.3% [−7.3, −5.7] | ±0.9% | confirmed |
| `hdt` | chain, 200,000 nodes | 239.8 (234.0–248.1) | 226.8 (226.0–238.0) | −5.5% [−8.6, −1.0] | ±3.7% | confirmed |
| `hdt` | blocks, 100,000 nodes | 107.4 (107.0–109.1) | 103.6 (103.2–104.8) | −3.3% [−6.5, −3.1] | ±1.9% | confirmed |
| `ett-scan` | chain, 200,000 nodes | 242.5 (235.3–254.4) | 227.4 (226.2–234.2) | −4.5% [−8.7, −3.5] | ±5.2% | inconclusive |
| `ett-scan` | blocks, 100,000 nodes | 107.3 (107.0–113.6) | 101.5 (100.9–102.0) | −5.9% [−9.2, −5.2] | ±6.6% | inconclusive |

`compact-bfs` improves as well: the replaced reflexive `connected(x, x)` made
two ID lookups per endpoint, and `contains_node` makes one.

End-to-end, the sampled maximum RSS was lower in all 6 pairs: base 48.77–52.56
MiB, median 50.80; candidate 45.91–47.59 MiB, median 45.98; a paired median of
−9.2% with no overlap between the ranges (exact sign test p = 0.031). This is
consistent with the import releasing its body buffer before building the
graph.

### What each kept change does

| Change | Mechanism evidence |
| --- | --- |
| `ConnectivityGraph` limit checks use `contains_node` (one expected O(1) ID lookup) instead of `connected(x, x)` (two lookups; O(log V) root walks on ETT/HDT). | Core initialization table. |
| ETT/HDT `link` and HDT `add_node`: one entry-API probe per endpoint and per edge key, instead of three or four lookups. | Part of the ETT/HDT rows. |
| `WireNode` parses the borrowed JSON string: no `String` per node ID. | 2 allocations fewer per link, cut or `connected` operation; about 3 per import node and edge. |
| `WireOp` resolves feeder `op` while parsing; unknown names keep their text for the unchanged validation error. | 1 allocation fewer per event. |
| `Vec::from(Bytes)` instead of `to_vec()`; the import buffer is dropped right after parsing. | 1 MiB body: 16–52 µs to copy versus 0.04–0.19 µs to convert a uniquely owned buffer (medians of three runs); lower end-to-end peak RSS. |
| Applied feeder events: `clone_from` for the retained position, moved strings for the dedup entry. | Part of the 6 allocations fewer per event. |
| `AppliedStamp`: one clock read per feeder request instead of per applied event, on every exit path, under the lock. | Ablation: reverting it costs **+6.6% [+3.0, +7.8]** on the 1,024-event batch with metrics on (A/A ±2.1%). One clock read costs 20.4 ns, about 21 µs per full batch. |
| Outstanding jobs: `AtomicUsize` plus a `Notify` signalled when the count reaches zero, instead of `watch::send_modify` (a write lock and eight notifier mutexes per update). | One track/release pair: 82.7–87.4 ns → 7.3 ns on one thread, 876–904 ns → 92–153 ns per thread with four threads. |
| `Service(Arc<ServiceInner>)`: one reference count per per-request state clone instead of seven plus one or two string copies; workers capture one clone. | Part of the 16 allocations fewer per request and the concurrent-client gain. |
| Response metadata headers take owned strings (`HeaderValue::try_from(String)` performs the same validation as `from_str`, without a copy); `HeaderValue::from(u64)`; source position encoded once. | Part of the 16 allocations fewer per request. |
| Logging middleware logs known routes from their static path; the timeout layer pins the handler future on the stack instead of boxing it. | 1 allocation fewer per request each (the second only with metrics on). |

## Reverted

**Cached HTTP metric handles** (`OnceLock` per label set instead of
`Family::get_or_create`). The mechanism is real: 55 ns → 1.9 ns per lookup on
one thread, 255–273 ns → 25–28 ns with four. At request level the ablation
did not separate from noise with metrics on: single +2.1% [−1.1, +3.8],
concurrent +1.1% [−4.2, +1.7], batch `connected` +0.1% [−0.9, +0.6]. The one
nominally confirmed cell came from metrics-off runs, where the cache never
executes, so it reflects build-layout noise. About 100 lines of indexing and
caching were not justified, so the baseline implementation is restored.

## Inconclusive Measurements

- ETT bulk initialization (both workloads): about −5%, but within the ±5–7%
  A/A bound on this host.
- End-to-end import, catch-up, time-to-ready, query QPS and latency, and
  restart recovery: paired medians between −13% and +14%, signs split, and no
  A/A arm. With `Connection: close`, TCP setup dominates request timing, and
  6 pairs are too few. Run them on the controlled hosts before quoting any
  number.
- Single changes inside the joint probe gains, other than `AppliedStamp`,
  were not ablated individually.
- An earlier screening run on a shared 2-vCPU Linux container had A/A
  variation up to 25% on dynamic core workloads. It is superseded by this
  campaign and supports no claim.

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
(`b17d7018…20ea36`) matches the delivered tree.

| Check | Result |
| --- | --- |
| fmt; Clippy (default and all features, all targets, `-D warnings`) | pass |
| `cargo test --workspace --locked` (1.100.0) | 323 passed, 0 failed, including doctests |
| `cargo test --workspace --all-features --locked` (1.100.0) | 452 passed, 0 failed, including doctests |
| Rustdoc (`-D warnings`), core examples, release build and smoke test | pass |
| `cargo +1.98.1 test` / `clippy -p knotrel-core` | 60 passed / pass |

Each suite has one test fewer than the measured candidate: the cache's
label-slot unit test was removed together with the cache.
