---
layout: default
title: Benchmarks
---
# Benchmarks and performance

Knotrel publishes reproducible workloads and release comparison evidence in the [knotrel-benchmarks repository](https://github.com/knotrel/knotrel-benchmarks). Results depend on the graph, workload, engine, hardware and configuration; they are not a general performance guarantee.

## Current release

**v0.9.0** was measured against v0.7.0 on its release candidate: the campaign is valid and the release is accepted with explicit exceptions, for v0.9.0 only. The [performance-by-version report](performance-by-version.md) explains the results, methodology and limitations.

v0.8.0 was not performance-qualified, by maintainer decision ([statement](releases/v0.8.0-release-notes.md#performance-qualification), [inspection cost report](console-inspection-costs.md)). The previous qualified release is **v0.7.0 versus v0.6.0**, accepted with explicit exceptions; see the [performance history](performance-history.md#v070-compared-with-v060) and the [v0.7.0 release notes](releases/v0.7.0-release-notes.md).

## Explore the evidence

- [Performance history](performance-history.md): earlier comparisons, each with its own conditions.
- [Performance site](https://knotrel.github.io/knotrel-benchmarks/): release-to-release reports, starting with 1.0.0 against a frozen 0.9.x reference. No report is published yet; the 1.0.0 page describes the plan.
- [Benchmark repository](https://github.com/knotrel/knotrel-benchmarks): workloads, tooling and campaign evidence.
- [v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification): what was and was not measured.

Do not combine measurements from different campaigns into a trend without checking their protocols and baselines.
