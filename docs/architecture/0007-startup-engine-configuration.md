# Explicit engine selection at startup

Status: Implemented, 2026-09-27. Historical configuration milestone.
Extended on 2026-09-28 by [ADR 0008](0008-experimental-hdt.md), which adds the
verified experimental HDT backend and the `hdt` selector.

## Context

Compact BFS remains the default. Experimental `ForestGraph` now has enough
correctness coverage to be an explicit opt-in for HTTP experiments. Selecting
an engine must not change the operation contract or imply HDT is implemented.
The core must remain synchronous, dependency-free and independent of deployment
configuration. Existing callers may supply a populated `Graph` to the server.

## Decision

The core exposes `EngineConfig::{CompactBfs, EulerTour}` and
`ConnectivityGraph::new(config)`. Construction is infallible: there are no
unsupported values in the enum. A private backend enum owns the selected graph;
all graph methods dispatch to it. Backend choice has no mutation API. Existing
`Graph` and `ForestGraph` APIs are unchanged. Converting an existing `Graph`
transfers ownership without rebuilding, preserving isolated vertices and edges.
The wrapper introduces constant-time enum dispatch, not a new algorithm or cache.

`ServerConfig` loads a snapshot of the environment in the server crate, validates
it, and passes a typed engine choice to the core. Supported settings are
`KNOTREL_ADDR`, `KNOTREL_ENGINE` and `KNOTREL_MAX_PENDING_JOBS`. Unknown keys in the
reserved `KNOTREL_` namespace, non-Unicode settings, unsupported engines and
invalid values cause startup failure before binding. Unrelated variables are
ignored; logging independently uses `RUST_LOG`. Missing settings retain the old
defaults: `127.0.0.1:8080`, compact BFS and 32 admitted jobs.

The admission capacity is positive and no greater than Tokio's maximum semaphore
capacity, avoiding constructor panics. Private config fields ensure embedded
callers also pass validation. Body and batch limits remain fixed. This is a
startup configuration facility, not a general parameter framework or file loader.

`router_with_config(config)` constructs an empty selected graph. Its address is
only a requested standalone listener address; embedded callers own binding.
The existing `router(Graph)` retains supplied graph state and default limits.
Both routes use the same service, admission semaphore and mutex spanning the
entire request/batch. ETT selection does not introduce concurrent graph writes.

`GET /v1/info` reports actual engine selection, experimental status, admission
capacity and fixed body/batch limits. Metadata is derived from the owned graph
and service construction arguments, stored immutably and readable without the
graph mutex or a worker permit. It is not a graph readiness probe.

## Runtime and future engines

Operation JSON remains unchanged and rejects unknown fields. There is no
per-request engine selection, automatic fallback, hot reload or migration API.
A live engine change requires constructing new state with all nodes and edges
and replacing the old state only after success. Snapshot consistency, concurrent
updates, failure handling and temporary memory requirements need a separate
design. A restart currently loses all graph state.

HDT is deliberately absent from both the typed enum and the accepted server
selectors. It may be added only with an implemented and verified engine. Levels
and promotion invariants remain internal algorithm concerns, not user knobs.
The benchmark CLI retains its existing engine names (`ett-scan`, for example);
these are benchmark selectors, distinct from the server's `ett` selector.

## Verification

Tests cover both selected engines, post-cut query freshness, replacement edges,
full-width IDs, graph errors and populated legacy-router compatibility. HTTP
tests verify effective metadata and reject per-request overrides before mutation.
Subprocess tests verify unsupported and malformed environment settings fail
startup. Isolated environment snapshots cover defaults, explicit IPv6/engine/
capacity selection and invalid Unicode without mutating the test process's
environment. Existing admission/cancellation and graph suites remain applicable.
No performance claims or new benchmark measurements are introduced here.
