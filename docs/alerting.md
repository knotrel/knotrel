# Alerting (M6.4)

**Status:** Released in v0.4.0. Rules:
[`examples/observability/prometheus-alerts.yml`](../examples/observability/prometheus-alerts.yml).

These rules turn the opt-in metrics of the
[observability contract](observability-contract.md) into alerts for the
failures an operator must act on: worker failure, saturation, repeated
errors, feeder loss, desynchronization and source gaps, slow bootstrap and
readiness flapping. They need `KNOTREL_METRICS_ENABLED=true` and a Prometheus
that scrapes `GET /metrics` (unauthenticated: keep it inside the trust
boundary described in the [deployment runbook](deployment.md)).

Thresholds and durations are starting points, not service-level objectives.
Tune them to the source's replay speed, the request mix and the paging
policy. Every series is per process and resets on restart; a restart itself
is visible as a reset of the counters and as `starting`/`uninitialized`
readiness.

To find the request behind an alert, search the structured logs
(`LOG_FORMAT=json`) for the messages named below; each line carries the
`request_id` of the request that produced it
([contract section 17](observability-contract.md#17-m63-structured-logs-released-in-v040)).

## Rules

| Alert | Severity | Condition | Mode |
| --- | --- | --- | --- |
| `KnotrelServiceUnavailable` | critical | `knotrel_service_unavailable == 1` | both |
| `KnotrelQueueSaturated` | warning | one admission lane (except `import`) above 80% of its permits for 5 min, per `lane` | both |
| `KnotrelRequestsRejectedBusy` | warning | any `busy` rejection rate on a lane for 5 min, per `lane` | both |
| `KnotrelHighServerErrorRatio` | warning | over 5% of completed requests are 5xx for 10 min | both |
| `KnotrelFeederLost` | critical | lease remaining below zero for 30 s | source-managed |
| `KnotrelDesynchronized` | critical | readiness state `desynchronized` | source-managed |
| `KnotrelSourceGapDeclared` | warning | a declared gap in the last 15 min | source-managed |
| `KnotrelSlowBootstrap` | warning | `uninitialized`, `loading` or `catching_up` for 15 min | source-managed |
| `KnotrelReadinessFlapping` | warning | more than 4 changes of the `ready` state in 15 min | source-managed |
| `KnotrelRepeatedFeederRejections` | warning | more than 10 rejections with one code per 10 min, sustained 10 min | source-managed |

Source-managed series are absent in standalone mode, so those rules never
fire there.

### KnotrelServiceUnavailable

A worker panicked and the service marked itself unavailable; every graph
request returns 503 `unavailable` until the process restarts. `/ready`
reports `unavailable`. Logs: `graph worker thread failed` (or the import or
feeder variant). Restart the process; in source-managed mode the feeder must
then rebuild (import and catch up) per the deployment runbook. Report the
crash: panic payloads are not logged, so keep the process's stderr.

### KnotrelQueueSaturated

Admitted work on one admission lane (queued plus running) stays near that
lane's permit limit; the alert's `lane` label says which
([contract section 21](observability-contract.md#21-per-lane-admission-metrics-v100-unreleased)).
The expression divides `knotrel_graph_jobs_outstanding` by
`knotrel_graph_job_capacity` with the same label set, so each lane's
numerator meets only its own capacity; the ratio never exceeds 1 and is 1
only when the lane's next request would be refused. Keep that matching if you
adapt the rule: aggregate both sides `by (lane)` (plus your target labels),
never one side alone. The `import` lane has a single permit and is full for
the whole duration of every import, so it is excluded; watch
`KnotrelSlowBootstrap` and `knotrel_import_duration_seconds` instead.

- `shared`: queries, stats and inspection (every graph job in standalone
  mode) stay near `KNOTREL_MAX_PENDING_JOBS`. Callers see rising latency and
  soon `busy`. Check `knotrel_graph_lock_wait_seconds` and
  `knotrel_graph_job_execution_seconds` by `kind` to tell contention from
  expensive work (large batches, `compact-bfs` queries on big graphs, imports
  holding the graph lock). Reduce load, batch differently, or raise
  `KNOTREL_MAX_PENDING_JOBS` only if latency allows.
- `feeder_data`: the feeder keeps `KNOTREL_FEEDER_RESERVED_JOBS` events,
  barrier or gap requests in flight. Expected briefly during catch-up with a
  pipelining feeder; sustained, it means feeder jobs wait on the graph lock
  (check the `feeder_events` kind) and the feeder is being throttled. Lower
  the feeder's concurrency or raise `KNOTREL_FEEDER_RESERVED_JOBS`.
- `feeder_control`: two session or heartbeat requests stay in flight; a
  heartbeat waiting this long risks lease expiry. Check lock wait for the
  `feeder_heartbeat` and `feeder_session` kinds and for competing feeders.

### KnotrelRequestsRejectedBusy

Requests are refused because the admission permits of the lane in the
alert's `lane` label are exhausted (`shared`, `feeder_data` or
`feeder_control`; the import lane's refusals are `import_capacity`). Logs:
`request capacity exhausted; rejecting with 503 busy`, with the same `lane`
field. Same diagnosis as saturation of that lane. Clients and feeders should
back off and retry; state changes of rejected requests did not happen.

### KnotrelHighServerErrorRatio

More than 5% of completed requests end in 5xx. Break down
`knotrel_requests_rejected_total` by `reason` and
`knotrel_requests_outcome_uncertain_total` by `cause`: `busy` points to
saturation, `request_timeout` to slow jobs (their outcome is uncertain:
clients must reconcile), `shutting_down` to a drain, `unavailable` to a
worker failure. Cancelled requests are excluded from the ratio.

### KnotrelFeederLost

The feeder lease expired: no feeder heartbeat or call renewed it, so the
graph is `stale` and `/ready` returns 503. Logs: `feeder lease expired;
readiness is stale` and `synchronization state changed` to `stale`. Check
the feeder process and its connectivity, then restart the feeder. A heartbeat
alone does not restore `ready`: recovery needs a new session epoch plus a
synchronized barrier, or a restart and rebuild
([deployment runbook](deployment.md#probes-and-states)). Queries keep
returning answers from the stale graph.

### KnotrelDesynchronized

The graph no longer matches its source: a declared gap, an epoch change, a
predecessor mismatch or a conflicting duplicate. `knotrel_desynchronizations_total`
by `cause` says which. Logs: `synchronization state changed` to
`desynchronized` (warn), with the request ID of the feeder call. The feeder
must publish a replacement import; events are rejected with
`graph_desynchronized` until then.

### KnotrelSourceGapDeclared

The feeder declared that it cannot deliver a contiguous history (log
retention, lost messages). Logs: `feeder declared unrecoverable source gap`
with the sanitized `reason`. Expect `KnotrelDesynchronized` too. Fix source
retention if gaps recur.

### KnotrelSlowBootstrap

The server has not become ready within 15 minutes of starting, importing or
replacing the graph. `uninitialized`: no import arrived (feeder down or not
configured). `loading`: an import is being received or built; check
`knotrel_import_duration_seconds` by `phase`. `catching_up`: the feeder is
replaying events but has not reached a synchronized barrier; compare
`knotrel_catchup_duration_seconds` with past catch-ups and check
`knotrel_feeder_events_total`. Raise the duration if your source legitimately
needs longer.

### KnotrelReadinessFlapping

Readiness enters and leaves `ready` repeatedly: typically a lease close to
the heartbeat interval (lease expiries and renewals), repeated session
takeovers, or repeated replacement imports. Logs: `synchronization state
changed` lines show the transitions and their jobs. Lengthen the lease
relative to the heartbeat interval, or fix the competing feeders.

### KnotrelRepeatedFeederRejections

One rejection code dominates for 10 minutes. Logs: `feeder request rejected`
with `code`. `fenced_feeder_session` or `lease_expired`: two feeders compete
or a feeder lost its lease. `out_of_order_sequence` or
`source_predecessor_mismatch`: the feeder resends from the wrong position.
`event_failed`: the source emits events that violate graph rules or limits.
The integration contract's retry table gives the expected feeder reaction.

## Not alerted

- **Feeder inactivity.** `knotrel_feeder_last_applied_age_seconds` grows when
  the source is idle; it is not lag (contract section 2). Alert on it only if
  your source is known to produce events continuously, with a threshold above
  its quietest period.
- **Lag.** No lag metric exists (contract section 9).
- **Memory and process health.** Use the platform's process or container
  metrics.

## Verification

`tests/metrics.rs` (`alert_rules_reference_only_exported_series`) checks that
every metric name and every exact label value used in the rules file exists
in the exposition of a source-managed server, so a renamed metric or label
breaks the build. `lanes::alert_expressions_divide_and_count_by_lane` parses
the `KnotrelQueueSaturated` expression, evaluates it against a real
exposition with one-to-one matching on identical label sets (each lane's
numerator paired with its own capacity) and checks that
`KnotrelRequestsRejectedBusy` aggregates `by (lane)`; the per-lane tests
evaluate it while lanes fill. The file is not validated with `promtool` in CI; validate
it with `promtool check rules` before loading it.
