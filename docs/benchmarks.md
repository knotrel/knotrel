---
layout: default
title: Benchmarks
---
# Benchmarks and performance

Knotrel publishes reproducible workloads and release comparison evidence in the [knotrel-benchmarks repository](https://github.com/knotrel/knotrel-benchmarks). Results depend on the graph, workload, engine, hardware and configuration; they are not a general performance guarantee.

<div class="actions doc-actions">
  <a class="button primary" href="https://knotrel.github.io/knotrel-benchmarks/"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 20V10M10 20V4M16 20v-7M22 20H2"/></svg>Performance site</a>
  <a class="button" href="https://github.com/knotrel/knotrel-benchmarks"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m8 7-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/></svg>Benchmark repository</a>
</div>
<p class="actions-note">The performance site publishes release-to-release reports from 1.0.0, and engine comparisons on one build.</p>

## Current release

**v1.1.0** was checked against v1.0.0 by the regression checks of its CF5 campaign: the qualification is **accepted with exceptions** for the measured build `02cba68`. No check of the existing engines is a regression; five are left open (two unresolved, one inconclusive, two timer-limited). Host swap activity was not verifiable; the results are accepted by the maintainer with this limitation. Their transfer to the 1.1.0 build is a separate maintainer decision. The [performance-by-version report](performance-by-version.md) explains the results, methodology and limitations, and the [1.1.0 page of the performance site](https://knotrel.github.io/knotrel-benchmarks/releases/1.1.0.html) shows every verdict as measured.

The previous qualified release is **v1.0.0 versus v0.9.0**, accepted with exceptions; see the [performance history](performance-history.md#v100-compared-with-v090).

## Choosing an engine

v1.1.0 adds the experimental `cluster-forest` engine; `compact-bfs` stays the default. Its CF5 qualification compared it with `compact-bfs`, ETT and HDT on one build (`02cba68`), not one release with another. Start from [Choosing an engine](choosing-an-engine.md); the results by workload family, with their limitations, are on the [engine comparison page](https://knotrel.github.io/knotrel-benchmarks/engines/cluster-forest-cf5-02cba68.html).

## Explore the evidence

- [Performance site](https://knotrel.github.io/knotrel-benchmarks/): release-to-release reports from 1.0.0 against v0.9.0 (1.1.0 against v1.0.0 is the latest), and engine comparisons.
- [Performance history](performance-history.md): earlier comparisons, each with its own conditions.
- [Benchmark repository](https://github.com/knotrel/knotrel-benchmarks): workloads, tooling and campaign evidence.
- [v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification): v0.8.0 was not performance-qualified.

Do not combine measurements from different campaigns into a trend without checking their protocols and baselines.
