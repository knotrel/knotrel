# Stability classification

**Status: APPROVED by the maintainer on 2026-11-21 (roadmap item 0.9.4);
applies from v0.9.0.** Classes and rules: [compatibility.md](compatibility.md). This list
covers every public surface on `main` as of 2026-11-21; a surface not listed
is internal.

**Corrections of 2026-11-22** (v1.0 contract freeze, maintainer decisions 1,
2, 3 and 8 of the [freeze review](releases/v1.0-contract-freeze-review.md#7-maintainer-decisions-2026-11-22)):
`#[non_exhaustive]` on five types, `router_with_lifecycle` Stable, the public
dependencies of `knotrel-server`, and the classification corrections R6–R8,
H8, C1, C2 and M2. Every item that moves from Stable to Experimental is listed
in [Reclassified from Stable to Experimental](#reclassified-from-stable-to-experimental);
nothing else was narrowed.

## How the Rust tables are checked

The Rust tables below are checked in CI (job `public API`,
`scripts/public-api.py`) against the committed public API snapshot
(`docs/api/public-api-knotrel-core.txt`, `docs/api/public-api-knotrel-server.txt`,
generated from rustdoc JSON) and the per-item classification
`docs/api/public-api-classification.txt`, which is the machine-readable
source of truth:

- the snapshot must equal the code, and every public item must have exactly
  one class (`stable`, `experimental` or `internal`) in the classification
  file, so a new public item fails CI until it is classified;
- each row has exactly one class, and names only real items in its first
  column (relative to the crate; `*` matches within one path segment);
- a type's row covers its members (methods, fields, variants, associated
  constants) unless a member has its own row; every crate-root item, and every
  member whose class differs from its type's, must have a row, with the same
  class as in the classification file.

Editing this document alone cannot satisfy the check: it can only make it
fail when it disagrees with the classification file.

## Rust: `knotrel-core`

| Item | Class | Notes |
| --- | --- | --- |
| `NodeId`, `Graph`, `BfsWorkspace` | Stable | `Graph`: `new`, `add_node`, `link`, `cut`, `connected`, `connected_with_workspace`, `node_count`, `edge_count`; `degree`, `neighbors_after` and `enable_neighbor_listing` added in 0.8.0. Limits live on `ConnectivityGraph` and `LifecycleGraph`, not on `Graph` |
| `IncidenceError` | Stable | Added in 0.8.0; `#[non_exhaustive]` from 1.0 |
| `GraphError` | Stable | `#[non_exhaustive]` from 1.0 |
| `GraphLimits` | Stable | Frozen as is: public fields, built with a struct literal; a new field requires a major release |
| `ConnectivityGraph`, `ConnectivityGraph::new`, `ConnectivityGraph::with_limits`, `ConnectivityGraph::try_from_edges`, `ConnectivityGraph::limits`, `ConnectivityGraph::engine`, `ConnectivityGraph::contains_node` | Stable | Every other method too, including `From<Graph>` |
| `ConnectivityGraph::as_compact`, `ConnectivityGraph::enable_neighbor_listing` | Stable | Added in 0.8.0 |
| `EngineConfig`, `EngineConfig::CompactBfs`, `EngineConfig::as_str` | Stable | `#[non_exhaustive]` from 1.0. `as_str` returns the stable `KNOTREL_ENGINE` values |
| `EngineConfig::EulerTour`, `EngineConfig::Hdt`, `EngineConfig::is_experimental` | Experimental | The ETT (`as_str` value `ett`) and HDT engines ([v1.0 assessment](releases/v1.0-readiness.md#experimental-engines)). Before 2026-11-22 this row named `EngineConfig::Ett`, which does not exist (R7) |
| `ForestGraph`, `ForestStats`, `HdtGraph`, `HdtStats`, `HdtLevelStorage`, `HdtStorageStats` | Experimental | Engine types and their work and storage counters |
| `LifecycleGraph`, `LifecycleError`, `NodeSpec`, `EdgeSpec` | Stable | Lifecycle on `compact-bfs` ([topology-lifecycle.md](topology-lifecycle.md)). `NodeSpec` and `EdgeSpec` are frozen as is (struct literals; a new field requires a major release); `LifecycleError` is `#[non_exhaustive]` |
| `LifecycleGraph::degree`, `LifecycleGraph::incident_after` | Stable | Added in 0.8.0 |
| `EdgeState`, `EdgeState::new` | Stable | `#[non_exhaustive]` from 1.0; `EdgeState::new` (new in 1.0) builds expected values outside the crate |
| `LifecycleStorage` | Experimental | Storage introspection. `LifecycleGraph::storage_stats`, which returns it, stays Stable under the `LifecycleGraph` row: the freeze review proposes Experimental, which is not among the approved corrections |
| `ReferenceGraph` | Internal | Test oracle; public for the test suites and benchmarks, no promise |

## Rust: `knotrel-server`

| Item | Class | Notes |
| --- | --- | --- |
| `serve`, `ShutdownOutcome` | Stable | `ShutdownOutcome` is frozen exhaustive: a new outcome requires a major release |
| `router`, `router_with_config` | Stable | |
| `router_with_lifecycle` | Stable | Internal before 1.0. Stable including its readiness behavior during the drain: `/ready` follows the supplied `Lifecycle` (503 `starting`; 200 `ready`, or the synchronization status in source-managed mode; 503 `draining` from the start of the drain, whatever the synchronization status), probes and `/metrics` stay answerable while draining, every other request gets 503 `shutting_down`, all with `Connection: close` ([rustdoc](https://docs.rs/knotrel-server/latest/knotrel_server/fn.router_with_lifecycle.html), [http-api.md](http-api.md#readiness-evaluation-precedence)) |
| `ServerConfig`, `ServerConfig::DEFAULT_*`, `ServerConfig::MAX_*`, `ServerMode`, `ConfigError` | Stable | Constructors, `from_env`, `with_*` builders, accessors and constants, except the rows below. `ServerMode` is frozen exhaustive |
| `ServerConfig::with_inspection`, `ServerConfig::inspection` | Experimental | With `KNOTREL_INSPECTION` and the inspection routes (R6; [reclassified](#reclassified-from-stable-to-experimental)) |
| `ServerConfig::with_feeder_reserved_jobs`, `ServerConfig::feeder_reserved_jobs` | Experimental | New in 0.9.0, with `KNOTREL_FEEDER_RESERVED_JOBS` |
| `ServerConfig::DEFAULT_FEEDER_RESERVED_JOBS`, `ServerConfig::MAX_FEEDER_RESERVED_JOBS` | Experimental | With `KNOTREL_FEEDER_RESERVED_JOBS` (R6; [reclassified](#reclassified-from-stable-to-experimental)) |
| `Lifecycle`, `Phase` | Stable | `Phase` is `#[non_exhaustive]` from 1.0 |
| `router_with_seeded_version`, `router_with_seeded_state`, `GateDecision`, `WorkerPauseHandle`, `advance_clock`, `lock_clock_for_test`, `reset_clock`, `percent_decode_position`, `percent_encode_position` | Internal | Behind the `test-support` feature: test instrumentation, unsupported in production |
| `Lifecycle::pause_next_worker`, `Lifecycle::pause_next_worker_after_lock`, `Lifecycle::pause_next_lock_request`, `Lifecycle::pause_next_ready_probe`, `Lifecycle::on_drain_loop`, `Lifecycle::on_gate` | Internal | Behind the `test-support` feature |

## Public dependencies of `knotrel-server`

The stable Rust API of `knotrel-server` exposes types of two other crates,
which are therefore part of that stable API:

| Dependency | Exposed in | Class |
| --- | --- | --- |
| axum 0.8 | `axum::Router`, returned by `router`, `router_with_config` and `router_with_lifecycle` and accepted by `serve` | Stable |
| tokio 1 | `tokio::net::TcpListener`, accepted by `serve`; `serve` and request handling require a Tokio runtime | Stable |

An embedder passes these values only with the same major versions (axum 0.8,
tokio 1). Upgrading either to an incompatible version (axum 0.9, tokio 2)
changes these signatures, so it requires a SemVer assessment of Knotrel,
recorded in the release notes: it is released as a major version from 1.0
unless the assessment shows that the public API is unchanged. Compatible
upgrades (axum 0.8.x, tokio 1.x) are ordinary dependency updates. No other
dependency appears in a public signature; the snapshot in
`docs/api/public-api-knotrel-server.txt` shows every foreign type used.

## Exhaustiveness and `#[non_exhaustive]`

From 1.0, `EngineConfig`, `GraphError`, `IncidenceError` and `Phase` (enums)
and `EdgeState` (struct) are `#[non_exhaustive]`, like `LifecycleError` and
`LifecycleStorage` before them: Knotrel may add variants or fields in a minor
release, and code outside the crate must match them with a wildcard arm and
build `EdgeState` with `EdgeState::new`. This was a breaking change against
v0.9.0, approved by the maintainer without a deprecation window (none is
possible) and declared in the crates' `cargo-semver-checks` configuration.

`#[non_exhaustive]` only allows additions. It does **not** authorize removing
or renaming a public variant, field or engine: removing a stable one remains
a breaking change that requires deprecation and a major release
([compatibility.md](compatibility.md#3-what-compatible-means)), and an
experimental one follows the rules of [Experimental items](#experimental-items).

`ServerMode`, `ShutdownOutcome`, `GraphLimits`, `NodeSpec` and `EdgeSpec` are
frozen as they are: exhaustive, so a new variant or field requires a major
release.

## Experimental items

Every item classified Experimental here, in `docs/api/public-api-classification.txt`,
or with `x-knotrel-stability: experimental` in `docs/openapi.yaml` has these
guarantees ([compatibility.md](compatibility.md#21-guarantees-of-experimental-items)):

- it is documented, tested and supported in the release that contains it;
- it may change or be removed only in a minor (or major) release, never in a
  patch release, and every such change is listed in the release notes;
- removing an experimental Rust item still fails `cargo-semver-checks`, so the
  removal is declared as an approved exception with the change; removing an
  experimental HTTP field or error code that OpenAPI does not mark (see
  [HTTP API](#http-api)) is listed in `docs/api/approved-breaking-changes.yaml`;
- an experimental member never makes its stable parent experimental: for
  example, removing `EngineConfig::Hdt` would not change `EngineConfig` or
  `EngineConfig::CompactBfs`;
- promotion to Stable is recorded in the release notes and here.

## Reclassified from Stable to Experimental

**Clearly marked list (decision 8 of 2026-11-22).** These items were covered
by a Stable rule of the 2026-11-21 classification (a blanket row or sentence)
and are Experimental from the 2026-11-22 corrections. The maintainer approved
each correction on the condition that it is called out here. Nothing else
moved from Stable to Experimental.

| Item | Previously Stable through | Finding |
| --- | --- | --- |
| `ServerConfig::with_inspection`, `ServerConfig::inspection` | The `ServerConfig` row ("`with_*`, accessors") | R6 |
| `ServerConfig::DEFAULT_FEEDER_RESERVED_JOBS`, `ServerConfig::MAX_FEEDER_RESERVED_JOBS` | The `ServerConfig` row ("`DEFAULT_*`/`MAX_*` constants") | R6 |
| HTTP error codes `inspection_disabled`, `inspection_unsupported`, `invalid_cursor` | "Every error `code` in `docs/openapi.yaml` is stable" | H8 |
| `/v1/info` field `inspection` | The stable `GET /v1/info` row | H8 |
| `knotrel_build_info` label values `engine="ett"` and `engine="hdt"` | "All 27 families … are stable", closed label value sets | M2 |
| Label values `route="/v1/nodes"`, `route="/v1/nodes/{node}"`, `route="/v1/nodes/{node}/edges"`, `route="/v1/edges"` of `knotrel_http_requests_total` and `knotrel_http_request_duration_seconds`, and their series | Same | M2 |

Items that move the other way (now Stable): `router_with_lifecycle`
(Internal before), `EngineConfig::as_str`, `ConnectivityGraph::new`,
`with_limits`, `limits` and `engine` (unnamed before, R8), `LOG_FORMAT` (C2) and
the console variables `KNOTREL_UPSTREAM_TIMEOUT`, `KNOTREL_CONSOLE_LISTEN` and
`KNOTREL_CONSOLE_SERVER_NAMES` (C1).

## HTTP API

| Route | Class | Notes |
| --- | --- | --- |
| `GET /health`, `GET /ready` | Stable | |
| `GET /v1/info`, `GET /v1/stats` | Stable | Fields `feeder_reserved_jobs` (new in 0.9.0) and `inspection` of `/v1/info`: Experimental |
| `POST /v1/operations`, `POST /v1/batch` | Stable | Including the lifecycle operations |
| `POST /v1/feeder/session`, `/heartbeat`, `/import`, `/events`, `/barrier`, `/gap` | Stable | The feeder protocol of the [integration contract](integration-contract.md) |
| `GET /v1/nodes`, `GET /v1/nodes/{node}`, `GET /v1/nodes/{node}/edges`, `GET /v1/edges` | Experimental | Inspection routes, opt-in, added in 0.8.0; marked `x-knotrel-stability: experimental` in `docs/openapi.yaml` |
| `GET /metrics` | Stable | Families: see below |

Response headers `X-Knotrel-Instance-Id`, `X-Knotrel-Graph-Id`,
`X-Knotrel-Generation`, `X-Knotrel-State-Version`,
`X-Knotrel-Source-Position`, `X-Knotrel-Sync-Status` and `ETag` are stable.
Every error `code` in `docs/openapi.yaml` is stable except
`inspection_disabled`, `inspection_unsupported` and `invalid_cursor`, which
only the experimental inspection routes emit and are Experimental; `message`
texts are internal.

OpenAPI marks stability per operation only. The experimental `/v1/info`
fields and inspection error codes carry no marker there, so
`scripts/check-openapi-compat.py` treats their removal as breaking: it is
done only with an entry in `docs/api/approved-breaking-changes.yaml`.

## Configuration

| Variable | Class |
| --- | --- |
| `KNOTREL_ADDR`, `KNOTREL_ALLOW_NON_LOOPBACK`, `KNOTREL_MODE`, `KNOTREL_MAX_PENDING_JOBS`, `KNOTREL_MAX_NODES`, `KNOTREL_MAX_EDGES`, `KNOTREL_MAX_CONNECTIONS`, `KNOTREL_HEADER_READ_TIMEOUT_MS`, `KNOTREL_REQUEST_TIMEOUT_MS`, `KNOTREL_SHUTDOWN_TIMEOUT_MS`, `KNOTREL_MAX_IMPORT_BODY_BYTES`, `KNOTREL_DEDUP_WINDOW_SIZE`, `KNOTREL_METRICS_ENABLED`, `KNOTREL_TOPOLOGY_LIFECYCLE` | Stable |
| `KNOTREL_ENGINE` | Stable for `compact-bfs`; the values `ett` and `hdt` are Experimental |
| `KNOTREL_INSPECTION` | Experimental (with the inspection routes) |
| `KNOTREL_FEEDER_RESERVED_JOBS` | Experimental (new in 0.9.0) |
| `LOG_FORMAT` | Stable: `json` (any case) selects JSON log lines, any other value or none selects text. The content of log lines is internal |
| `RUST_LOG` | Internal: filter syntax owned by `tracing-subscriber`; the default is `info` |

## Metrics

All 27 families of the [observability contract](observability-contract.md)
are stable: name, type, unit and label names. Label values documented as
closed sets (for example `kind`, `reason`, `result`) are extended only
additively. These label values are experimental, with the surfaces they
describe:

- `kind="inspect"`, with the inspection routes;
- `route="/v1/nodes"`, `route="/v1/nodes/{node}"`, `route="/v1/nodes/{node}/edges"`
  and `route="/v1/edges"`, with every series that carries them, with the
  inspection routes (the series are registered even when inspection is off);
- `engine="ett"` and `engine="hdt"` of `knotrel_build_info`, with the
  experimental engines.

`knotrel_build_info{version}` is an open set: it carries the crate version.

## Other artifacts

| Artifact | Class |
| --- | --- |
| Console UI and its container configuration (`KNOTREL_UPSTREAM`, `KNOTREL_UPSTREAM_TIMEOUT`, `KNOTREL_CONSOLE_LISTEN`, `KNOTREL_CONSOLE_SERVER_NAMES`, `KNOTREL_CONSOLE_SERVER_LABEL`) | Stable for configuration; the UI itself carries no API promise |
| Console static archive layout | Internal |
| `integrations/postgres-feeder` (code, settings, positions, exit codes) | Example: no promise ([compatibility.md](compatibility.md#7-what-this-does-not-cover)) |
| Log messages and fields | Internal |
