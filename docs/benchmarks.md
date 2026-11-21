---
layout: default
title: Benchmarks
---
# Benchmarks and performance

Knotrel publishes reproducible workloads and release comparison evidence in the [knotrel-benchmarks repository](https://github.com/knotrel/knotrel-benchmarks). Results depend on the graph, workload, engine, hardware and configuration; they are not a general performance guarantee.

## Current release

**v0.8.0 was not performance-qualified against v0.7.0.** Its opt-in inspection routes received a targeted cost check, not a release qualification. Read the [v0.8.0 performance statement](performance-by-version.md#v080-not-qualified) and [inspection cost report](console-inspection-costs.md) for the measurement conditions and limits.

The most recent qualified comparison is **v0.7.0 versus v0.6.0**, accepted with explicit exceptions. The [performance-by-version report](performance-by-version.md#v070-compared-with-v060) explains the results, methodology and limitations. The [release notes](releases/v0.7.0-release-notes.md) contain the release-specific decisions.

## Explore the evidence

- [Performance history](performance-history.md): earlier comparisons, each with its own conditions.
- [Benchmark repository](https://github.com/knotrel/knotrel-benchmarks): workloads, tooling and published reports.
- [v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification): what was and was not measured.

Do not combine measurements from different campaigns into a trend without checking their protocols and baselines.
