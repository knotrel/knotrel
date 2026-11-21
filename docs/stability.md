# Stability classification

**Status: PROPOSED for v0.9.0 (roadmap item 0.9.4), awaiting maintainer
approval.** Classes and rules: [compatibility.md](compatibility.md). This list
covers every public surface on `main` as of 2026-11-21; a surface not listed
is internal.

## Rust: `knotrel-core`

| Item | Class | Notes |
| --- | --- | --- |
| `NodeId`, `Graph` (construction, `add_node`, `link`, `cut`, `connected`, `connected_with_workspace`, counts, limits) | Stable | |
| `Graph::degree`, `Graph::neighbors_after`, `Graph::enable_neighbor_listing`, `IncidenceError` | Stable | Added in 0.8.0 |
| `BfsWorkspace` | Stable | |
| `GraphError`, `GraphLimits` | Stable | |
| `ConnectivityGraph`, `EngineConfig::CompactBfs`, `ConnectivityGraph::try_from_edges`, `contains_node` | Stable | |
| `ConnectivityGraph::as_compact`, `ConnectivityGraph::enable_neighbor_listing` | Stable | Added in 0.8.0 |
| `EngineConfig::Ett`, `EngineConfig::Hdt`, `EngineConfig::is_experimental` | Experimental | The ETT and HDT engines ([v1.0 assessment](releases/v1.0-readiness.md#experimental-engines)) |
| `ForestGraph`, `ForestStats`, `HdtGraph`, `HdtStats`, `HdtLevelStorage`, `HdtStorageStats` | Experimental | Engine types and their work and storage counters |
| `LifecycleGraph`, `LifecycleError`, `NodeSpec`, `EdgeSpec`, `EdgeState` | Stable | Lifecycle on `compact-bfs` ([topology-lifecycle.md](topology-lifecycle.md)) |
| `LifecycleGraph::degree`, `LifecycleGraph::incident_after` | Stable | Added in 0.8.0 |
| `LifecycleStorage` | Experimental | Storage introspection |
| `ReferenceGraph` | Internal | Test oracle; public for the test suites and benchmarks, no promise |

## Rust: `knotrel-server`

| Item | Class | Notes |
| --- | --- | --- |
| `serve`, `ShutdownOutcome` | Stable | |
| `ServerConfig` (constructors, `from_env`, `with_*`, accessors, `DEFAULT_*`/`MAX_*` constants), `ServerMode`, `ConfigError` | Stable | `with_feeder_reserved_jobs` and `feeder_reserved_jobs`: Experimental (new in 0.9.0) |
| `router`, `router_with_config` | Stable | |
| `Lifecycle`, `Phase` | Stable | |
| `router_with_lifecycle`, `router_with_seeded_version`, `router_with_seeded_state` | Internal | Embedding and test helpers |
| Everything behind the `test-support` feature (`GateDecision`, `WorkerPauseHandle`, clock and position helpers) | Internal | |

## HTTP API

| Route | Class | Notes |
| --- | --- | --- |
| `GET /health`, `GET /ready` | Stable | |
| `GET /v1/info`, `GET /v1/stats` | Stable | Field `feeder_reserved_jobs` of `/v1/info`: Experimental (new in 0.9.0) |
| `POST /v1/operations`, `POST /v1/batch` | Stable | Including the lifecycle operations |
| `POST /v1/feeder/session`, `/heartbeat`, `/import`, `/events`, `/barrier`, `/gap` | Stable | The feeder protocol of the [integration contract](integration-contract.md) |
| `GET /v1/nodes`, `GET /v1/nodes/{node}`, `GET /v1/nodes/{node}/edges`, `GET /v1/edges` | Experimental | Inspection routes, opt-in, added in 0.8.0; marked `x-knotrel-stability: experimental` in `docs/openapi.yaml` |
| `GET /metrics` | Stable | Families: see below |

Response headers `X-Knotrel-Instance-Id`, `X-Knotrel-Graph-Id`,
`X-Knotrel-Generation`, `X-Knotrel-State-Version`,
`X-Knotrel-Source-Position`, `X-Knotrel-Sync-Status` and `ETag` are stable.
Every error `code` in `docs/openapi.yaml` is stable; `message` texts are
internal.

## Configuration

| Variable | Class |
| --- | --- |
| `KNOTREL_ADDR`, `KNOTREL_ALLOW_NON_LOOPBACK`, `KNOTREL_MODE`, `KNOTREL_MAX_PENDING_JOBS`, `KNOTREL_MAX_NODES`, `KNOTREL_MAX_EDGES`, `KNOTREL_MAX_CONNECTIONS`, `KNOTREL_HEADER_READ_TIMEOUT_MS`, `KNOTREL_REQUEST_TIMEOUT_MS`, `KNOTREL_SHUTDOWN_TIMEOUT_MS`, `KNOTREL_MAX_IMPORT_BODY_BYTES`, `KNOTREL_DEDUP_WINDOW_SIZE`, `KNOTREL_METRICS_ENABLED`, `KNOTREL_TOPOLOGY_LIFECYCLE` | Stable |
| `KNOTREL_ENGINE` | Stable for `compact-bfs`; the values `ett` and `hdt` are Experimental |
| `KNOTREL_INSPECTION` | Experimental (with the inspection routes) |
| `KNOTREL_FEEDER_RESERVED_JOBS` | Experimental (new in 0.9.0) |

## Metrics

All 27 families of the [observability contract](observability-contract.md)
are stable: name, type, unit and label names. Label values documented as
closed sets (for example `kind`, `reason`, `result`) are extended only
additively. The `kind="inspect"` label value is experimental with the
inspection routes.

## Other artifacts

| Artifact | Class |
| --- | --- |
| Console UI and its container configuration (`KNOTREL_UPSTREAM`, `KNOTREL_CONSOLE_SERVER_LABEL`) | Stable for configuration; the UI itself carries no API promise |
| Console static archive layout | Internal |
| `integrations/postgres-feeder` (code, settings, positions, exit codes) | Example: no promise ([compatibility.md](compatibility.md#7-what-this-does-not-cover)) |
| Log messages and fields | Internal |
