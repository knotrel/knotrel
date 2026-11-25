# Performance by version

This page summarizes, for users, what the release qualification measured when
comparing the current release with the latest qualified release. It is a
brief extracted from the evidence in
[knotrel-benchmarks](https://github.com/knotrel/knotrel-benchmarks). The
release notes remain the authoritative statement.

Earlier comparisons are kept in the [performance history](performance-history.md).
Each was a separate campaign with its own rules, references and binaries, so
those results cannot be compared with this page or chained into a trend.

## v1.1.0 compared with v1.0.0

v1.1.0 was released on 2026-11-25. These results are the regression checks of
the CF5 campaign: the existing engines of build `02cba68` (crate version 1.0.0
on `main`, never released) measured against the published v1.0.0, built from
source with the same toolchain, on 2026-11-24 and 2026-11-25. 1.1.0 differs
from the measured build by documentation, CI, release checks, version lines,
the console's development tooling and one server dependency patch update
(`sonic-rs` 0.5.11 to 0.5.12); whether the results transfer to the 1.1.0
build is a separate maintainer decision. Binary identity with the measured
build and a new measurement of 1.1.0 are not claimed
([decision record](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.1.0-cf5/DECISION.md)).

The qualification of these results is **accepted with exceptions**:
- no check is a regression or a failure;
- five checks are left open: each keeps its class, and none is a regression
  or a pass;
- **host swap activity was not verifiable; the results are accepted by the
  maintainer with this limitation.** It applies to every result below, PASS
  included.

One Apple M3 host (8 cores, 16 GB, macOS 26.5.2), toolchain 1.100.0; server and
load generator on the same machine. The results hold for this host, these
workloads and these builds only.

### How to read these numbers

Each check pairs 24 runs of v1.0.0 and of the candidate in fresh processes.
The interval is the distribution-free interval of the median paired change
(coverage 97.73 %, not a 95 % interval); positive is slower or larger. PASS
means the interval lies within the tolerance approved before the campaign:
10 % for primary times and throughput, 25 % for p99 latency, 20 % for memory.
It is a decision under a tolerance, not a significance test, and not proof of
a speedup. V1 follows the frozen v1.0.0 protocol. V2 also applies the timer
rule: TIMER_LIMITED means the class could change within the measurement
limit, and it is handled as inconclusive.

### Results

| Check | Outcome |
|---|---|
| V1 core, sparse traversal and sustained churn, 10 000 nodes, `compact-bfs` and `compact-workspace` (4 cells) | 4 PASS |
| V1 HTTP standalone batches b8 and b256 | 2 PASS |
| V1 HTTP batch b1 | **UNRESOLVED**: window time PASS [−6.2 %, +9.0 %]; p99 request latency inconclusive [−41.9 %, +73.8 %] |
| V1 HTTP batch b64 | **UNRESOLVED**: window time PASS [−4.2 %, +3.4 %]; sampled peak memory inconclusive [0.0 %, +22.1 %] |
| V1 HTTP lifecycle, small | **INCONCLUSIVE_DISCLOSED**: time to ready [−18.3 %, +25.9 %] |
| V2 server mixes (6 cells, `compact-bfs`, lifecycle off and on) | 12 of 12 metrics PASS (throughput and p99 latency) |
| V2 `hdt`, `chain-split-rejoin` and `sustained-churn-path`, 100 000 nodes | 2 PASS |
| V2 `ett` (scan), the same two workloads | **TIMER_LIMITED** [−2.2 %, +1.8 %] and [−3.5 %, +2.6 %]; the class without the limit (PASS) is not counted |

The [1.1.0 page of the performance site](https://knotrel.github.io/knotrel-benchmarks/releases/1.1.0.html)
shows every metric with its interval and verdict as measured.

### What this means for you

| If you | Then |
|---|---|
| Upgrade from v1.0.0 and keep the default `compact-bfs` | The measured core and server workloads stayed within their tolerances, except the open checks below. No speedup is claimed |
| Serve HTTP batches of size 1 and depend on p99 latency | Not bounded against v1.0.0 (inconclusive) |
| Depend on peak memory with HTTP batches of size 64 | Not bounded against v1.0.0 (inconclusive) |
| Depend on the time to ready of a small lifecycle server | Not bounded against v1.0.0 (inconclusive) |
| Use `ett` | Two core workloads are at the timer's resolution: no regression was found, and none is excluded |
| Want to try `cluster-forest` | See below; it is new, so it has no release-to-release comparison |

### `cluster-forest` (an engine comparison, not a release comparison)

v1.1.0 adds the experimental `cluster-forest` engine; `compact-bfs` stays the
default. CF5 compared it with `compact-bfs`, ETT and HDT **on one build**: it
is faster on some workload families and slower on others. Two lifecycle
memory checks fail on the sparse 1 000-node graph (projected heap growth over
30 cycles +20.2 % and +14.6 %), accepted as a documented limitation. See
[Choosing an engine](choosing-an-engine.md) and the
[engine comparison](https://knotrel.github.io/knotrel-benchmarks/engines/cluster-forest-cf5-02cba68.html).

## Sources

- [v1.1.0 release notes](releases/v1.1.0-release-notes.md#performance-qualification).
- knotrel-benchmarks [`results/v1.1.0-cf5/DECISION.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.1.0-cf5/DECISION.md),
  the decision record, and [`REPORT.md`](https://github.com/knotrel/knotrel-benchmarks/blob/main/results/v1.1.0-cf5/REPORT.md),
  the campaign report.
- Earlier releases: [performance history](performance-history.md).
