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

**v1.0.0** was measured against v0.9.0: the campaign is valid and the qualification is **accepted with exceptions**, for v1.0.0 only. No deciding result is a regression; four results are not PASS, two hub lifecycle requirements are not qualified, and feeder catch-up under query load (Q3) fails as a declared limitation. The [performance-by-version report](performance-by-version.md) explains the results, methodology and limitations, and the [1.0.0 page of the performance site](https://knotrel.github.io/knotrel-benchmarks/releases/1.0.0.html) shows every verdict as measured.

The previous qualified release is **v0.9.0 versus v0.7.0**, accepted with explicit exceptions; see the [performance history](performance-history.md#v090-compared-with-v070).

## Choosing an engine (1.1.0 candidate)

The unreleased 1.1.0 adds the experimental `cluster-forest` engine; `compact-bfs` stays the default. Its CF5 qualification compared it with `compact-bfs`, ETT and HDT on one build (candidate `02cba68`), not one release with another. Start from [Choosing an engine](choosing-an-engine.md); the results by workload family, with their limitations, are on the [engine comparison page](https://knotrel.github.io/knotrel-benchmarks/engines/cluster-forest-cf5-02cba68.html).

## Explore the evidence

- [Performance site](https://knotrel.github.io/knotrel-benchmarks/): release-to-release reports, starting with 1.0.0 against v0.9.0, and engine comparisons.
- [Performance history](performance-history.md): earlier comparisons, each with its own conditions.
- [Benchmark repository](https://github.com/knotrel/knotrel-benchmarks): workloads, tooling and campaign evidence.
- [v0.8.0 release notes](releases/v0.8.0-release-notes.md#performance-qualification): v0.8.0 was not performance-qualified.

Do not combine measurements from different campaigns into a trend without checking their protocols and baselines.
