# Standalone batch warmup investigation

## Status and motivation

This is measurement tooling, not a server/core optimization. The completed
`p0-coverage-increment-1-campaign-20261107-2158` remains immutable evidence.
Its short HTTP windows show position/session variation. That association does
not establish a transport, cache, scheduler, or thermal cause. CV cutoffs of
5%, 10%, or 15%, and position cutoffs of 3%, are not established qualification
thresholds. In particular, dropping the largest observation is only a
sensitivity calculation, not permission to exclude it from the primary result.

Inspection found that the previous 50-query warmup used **one HTTP request**.
The proposed intervention varies request count while preserving all 50 queries,
query order, empty-edge topology, graph revision, measured trace and connection
model. Connection reuse is deliberately a separate experiment because it would
change the measured transport contract.

## Driver contract

`--batch-warmup-chunk-size N` accepts integers 1 through 50 and is valid only
with `--scenario standalone-batch`. The default is 50, preserving the previous
one-request warmup. Size 1 uses 50 sequential requests; size 8 uses seven
requests, the last containing two queries. Each request uses the existing
`Connection: close` transport and response validation. All warmup requests and
post-warmup state checks precede the measurement window.

The `measure_e2e.batch.v1` JSON adds `configuration.warmup_chunk_size` and
`runs[].warmup.request_count`. Existing fields remain unchanged. The collector
requires the manifest cell's `warmup_chunk_size` to match the report, defaulting
to 50 for historical manifests. Historical reports without either field retain
the original one-request meaning; new reports with chunk metadata must supply
the corresponding request count. All reports must still contain 50 warmup
queries and unchanged revision 1000. Measured operations remain 2000, final
revision 2000, and measured request counts depend only on `--batch-size`.

## Next controlled comparison

Use a separately built candidate driver at a new path, never overwriting the
accepted release driver or frozen server/harness. Compare chunk size 50 versus
1 **using the same new driver binary** and the same frozen baseline server at
measured batch sizes 8, 64, and 256. Keep 15-second process limits, approved host
checks and quiet periods. This is a method A/B experiment, not the existing A/A
campaign: do not relabel existing pairs or mix regimes within a historical cell.

Before collection, fix the complete schedule and record source, binary and
collector hashes in a new bundle. Balance both treatment order and position
within each session. Retain every observation and include the first post-quiet
launch. Report treatment differences, position differences, and session-level
variation together; do not select a regime solely because its mean is faster.

Successful functional validation requires unchanged measured work/topology,
correct metadata, rejection of mixed regimes, and bounded cleanup. Statistical
success is not predefined as an arbitrary CV percentage. With a small pilot,
report whether an association is consistent across sessions and how uncertain
it is; do not claim a minimum detectable effect or elimination of bias. A
repeatable reduction in position-associated variation would support adopting
the method for future comparisons, not an algorithm speedup.

No new release performance measurements have been collected for this change.
P0, M3.1 and v0.3.0 remain open.

## Implementation verification

The driver integration regression first failed because the new option was
unknown, then passed for chunk sizes 1, 8 and 50. It checks fixed warmup query
count, revision preservation, measured request count and final verification.
Invalid sizes and use in the lifecycle scenario are rejected. The collector
regression first demonstrated accidental acceptance of mixed warmup regimes;
it now rejects them and mismatched request counts.

Validation completed: workspace tests in default and test-support modes,
strict Clippy in both configurations, formatting and warning-denied Rustdoc;
90 Python tests and 16 subtests passed. All 204 historical campaign reports
were reparsed successfully offline. The accepted release driver, frozen server
and frozen core harness still match the existing manifest hashes. No release
binary was rebuilt, no performance campaign was run, and staging was preserved.
