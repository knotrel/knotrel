//! Axum HTTP transport for a single in-memory Knotrel graph.
//!
//! The server uses exact connectivity with bounded blocking work.
//! State is ephemeral and there is no authentication, authorization, TLS or
//! persistence layer: serve it only to trusted local clients. The standalone
//! executable binds to loopback unless explicitly configured otherwise.
//! [`serve()`] adds bounded connections, timeouts and graceful shutdown.
//!
//! [`ServerMode::Standalone`] supports direct operations and ordered batches.
//! [`ServerMode::SourceManaged`] maintains derived state loaded by atomic import
//! and updated through ordered feeder events. The external source remains
//! authoritative; after restart, the graph must be rebuilt and synchronized.
//!
//! [`ServerConfig`] controls startup settings, including opt-in Prometheus metrics.
//! Embedded callers can build a router with [`router_with_config`] or provide an
//! explicit lifecycle with [`router_with_lifecycle`], whose `/ready` follows
//! startup and the shutdown drain.
//!
//! # Public dependencies
//!
//! The stable API exposes two foreign types: [`axum::Router`] (axum 0.8),
//! returned by [`router`], [`router_with_config`] and [`router_with_lifecycle`]
//! and accepted by [`serve()`], and [`tokio::net::TcpListener`] (tokio 1),
//! accepted by [`serve()`], which also requires a Tokio runtime. Both
//! dependencies are part of this crate's stable API: use the same major
//! versions in your own crate to pass these values. An upgrade of either to an
//! incompatible version (axum 0.9, tokio 2) changes these signatures, so it
//! requires a SemVer assessment of Knotrel and, from 1.0, is released only as
//! a major version unless the assessment shows the public API unchanged. See the
//! [stability classification](https://github.com/knotrel/knotrel/blob/main/docs/stability.md#public-dependencies-of-knotrel-server).
//!
//! # Node and edge lifecycle
//!
//! The server can keep inactive nodes and disabled edges in its
//! topology and exclude them from connectivity, and can remove nodes
//! permanently (operations `set_node_active`, `set_edge_enabled`,
//! `remove_node`, `node_state` and `edge_state`; flagged imports and feeder
//! events in source-managed mode). This topology lifecycle is unrelated to the
//! process [`Lifecycle`] above. It is opt-in: the executable enables it with
//! `KNOTREL_TOPOLOGY_LIFECYCLE=true`, embedded callers with
//! [`ServerConfig::with_topology_lifecycle`]. It is supported on the
//! `compact-bfs` engine and, new in 1.1, on the experimental
//! `cluster-forest` engine (see the
//! [engine selection guide](https://knotrel.github.io/knotrel/choosing-an-engine.html));
//! enabling it with `ett` or `hdt` returns
//! [`ConfigError`] (the executable exits before binding), and those engines
//! remain available without lifecycle. With lifecycle off, behavior is
//! unchanged and lifecycle operations are refused with
//! `topology_lifecycle_disabled`.
//!
//! ```rust
//! use knotrel_core::EngineConfig;
//! use knotrel_server::ServerConfig;
//!
//! let address = "127.0.0.1:0".parse()?;
//! let config = ServerConfig::new(address, EngineConfig::CompactBfs, 32)?
//!     .with_topology_lifecycle(true)?;
//! assert!(config.topology_lifecycle());
//!
//! let forest = ServerConfig::new(address, EngineConfig::ClusterForest, 32)?;
//! assert!(forest.with_topology_lifecycle(true).is_ok());
//!
//! let ett = ServerConfig::new(address, EngineConfig::EulerTour, 32)?;
//! assert!(ett.with_topology_lifecycle(true).is_err());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See the [lifecycle contract](https://github.com/knotrel/knotrel/blob/main/docs/topology-lifecycle.md)
//! for semantics, costs and memory limits.
//!
//! # Inspection (since 0.8)
//!
//! Read-only routes list nodes and edges with cursor paging and state filters
//! (`GET /v1/nodes`, `/v1/nodes/{node}`, `/v1/nodes/{node}/edges`,
//! `/v1/edges`), for browsing tools such as the Knotrel console. Each page
//! examines a bounded number of entries, independent of graph size. They are
//! opt-in: `KNOTREL_INSPECTION=true` for the executable,
//! [`ServerConfig::with_inspection`] for embedded callers. When off, the
//! routes answer `inspection_disabled` and the server keeps no extra state.
//!
//! ```rust
//! use knotrel_core::EngineConfig;
//! use knotrel_server::ServerConfig;
//!
//! let address = "127.0.0.1:0".parse()?;
//! let config = ServerConfig::new(address, EngineConfig::CompactBfs, 32)?.with_inspection(true);
//! assert!(config.inspection());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See [Inspection](https://github.com/knotrel/knotrel/blob/main/docs/http-api.md#inspection-opt-in)
//! for paging, filters, errors and costs.
//!
//! See the [HTTP API documentation](https://github.com/knotrel/knotrel/blob/main/docs/http-api.md)
//! for wire formats and batch semantics. That page tracks development on `main`;
//! use the corresponding release tag when consulting a published version.

mod api;
mod configuration;
mod graph;
mod lifecycle;
mod logging;
mod metrics;
mod serve;
mod service;
mod versions;
pub use configuration::{ConfigError, ServerConfig, ServerMode};
#[cfg(feature = "test-support")]
pub use lifecycle::{GateDecision, WorkerPauseHandle};
pub use lifecycle::{Lifecycle, Phase};
pub use serve::{ShutdownOutcome, serve};
#[cfg(feature = "test-support")]
pub use service::{
    advance_clock, lock_clock_for_test, percent_decode_position, percent_encode_position,
    reset_clock,
};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_BATCH_OPERATIONS: usize = 1024;

/// Builds the HTTP API around an owned, in-memory graph.
///
/// Routes are `GET /health`, `GET /ready`, `GET /v1/info`, `GET /v1/stats`,
/// `POST /v1/operations`, `POST /v1/batch` and the read-only inspection routes
/// `GET /v1/nodes`, `GET /v1/nodes/{node}`, `GET /v1/nodes/{node}/edges` and
/// `GET /v1/edges` (which answer `inspection_disabled` unless
/// [`ServerConfig::with_inspection`] is enabled). Request
/// bodies are limited to 1 MiB; batches contain 1–1024 operations. At most 32
/// graph jobs may be queued or running. Excess graph requests receive HTTP 503.
/// A graph lock isolates each request, including all operations in a batch:
/// query-only requests and stats share it, every other request holds it
/// exclusively.
/// Cloned routers share graph state; separate calls construct separate graphs.
///
/// The supplied graph keeps its own cardinality limits (unlimited for a plain
/// [`knotrel_core::Graph`]); use [`router_with_config`] for the server defaults.
///
/// Request handling requires a Tokio runtime. `/health` reports process
/// liveness; `/ready` reports [`Phase::Ready`] immediately because embedded
/// callers own their listener. A request that takes longer than the default
/// request timeout receives HTTP 503. See `docs/http-api.md` for retry semantics.
pub fn router(graph: knotrel_core::Graph) -> axum::Router {
    build_router(
        knotrel_core::ConnectivityGraph::from(graph).into(),
        ServerConfig::default(),
        Lifecycle::ready(),
    )
}

/// Builds the HTTP API around an empty graph using validated startup settings.
///
/// The engine, admission limit and request timeout remain fixed for this
/// router's lifetime. The configuration address is not bound here; embedded
/// callers own their listener. Other limits and batch semantics are the same
/// as [`router`]. The process lifecycle starts ready; in source-managed mode,
/// graph readiness additionally requires bootstrap and synchronization. Use
/// [`router_with_lifecycle`] with [`serve()`] for the full lifecycle.
/// This function does not read the environment.
pub fn router_with_config(config: ServerConfig) -> axum::Router {
    router_with_lifecycle(config, Lifecycle::ready())
}

/// Like [`router_with_config`], but readiness and shutdown rejection follow
/// the supplied [`Lifecycle`], which [`serve()`] drives.
///
/// This is the way to connect a [`Lifecycle`] to a router: use it whenever
/// `/ready` must follow startup and the shutdown drain, as the executable does.
/// Clones of `lifecycle` (for example the one passed to [`serve()`]) share
/// the phase. The behavior below is Stable.
///
/// # Readiness and drain
///
/// - [`Phase::Starting`]: `GET /ready` answers 503 `{"status":"starting"}`;
///   other routes are served normally.
/// - [`Phase::Ready`]: `GET /ready` answers 200 `{"status":"ready"}` in
///   [`ServerMode::Standalone`]. In [`ServerMode::SourceManaged`] it reports
///   the graph's synchronization status instead (503 `uninitialized`,
///   `loading`, `catching_up`, `stale` or `desynchronized` until the feeder
///   has bootstrapped and synchronized the graph).
/// - [`Phase::Draining`]: `GET /ready` answers 503 `{"status":"draining"}`,
///   whatever the synchronization status. `GET /health`, and `GET /metrics`
///   when metrics are enabled, are still answered normally so orchestrators can
///   observe the drain (`HEAD` works like `GET` on these paths); every other
///   request, including unknown routes and other methods on the probe and
///   metrics paths, is rejected with 503 `shutting_down` before it reaches a
///   handler. A request
///   admitted before draining began runs to completion. While draining, every
///   response carries `Connection: close`.
/// - A failed graph worker makes `/ready` answer 503 `{"status":"unavailable"}`
///   in every phase.
///
/// Every `/ready` response carries `Cache-Control: no-store`. The phase only
/// moves forward ([`Lifecycle`]), so a draining router never becomes ready
/// again.
///
/// ```rust
/// use knotrel_core::EngineConfig;
/// use knotrel_server::{Lifecycle, Phase, ServerConfig, router_with_lifecycle};
///
/// let config = ServerConfig::new("127.0.0.1:0".parse()?, EngineConfig::CompactBfs, 32)?;
/// let lifecycle = Lifecycle::new();
/// let app: axum::Router = router_with_lifecycle(config.clone(), lifecycle.clone());
/// // `serve(listener, app, &config, &lifecycle, shutdown)` marks the lifecycle
/// // ready before accepting and moves it to draining when `shutdown` completes.
/// assert_eq!(lifecycle.phase(), Phase::Starting);
/// # drop(app);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn router_with_lifecycle(config: ServerConfig, lifecycle: Lifecycle) -> axum::Router {
    build_router(empty_graph(&config), config, lifecycle)
}

/// An empty graph for the configured engine, limits and lifecycle support.
fn empty_graph(config: &ServerConfig) -> graph::ServiceGraph {
    graph::ServiceGraph::empty(
        config.engine(),
        config.graph_limits(),
        config.topology_lifecycle(),
        config.inspection(),
    )
}

fn build_router(
    graph: graph::ServiceGraph,
    config: ServerConfig,
    lifecycle: Lifecycle,
) -> axum::Router {
    let service =
        service::Service::with_config(graph, config.max_pending_jobs(), lifecycle.clone(), &config);
    build_router_from_service(service, config, lifecycle)
}

/// Builds an HTTP router with a seeded decimal state version for arbitrary-precision testing.
///
/// Only available when the `test-support` feature is enabled.
#[cfg(feature = "test-support")]
pub fn router_with_seeded_version(
    config: ServerConfig,
    lifecycle: Lifecycle,
    version_str: &str,
) -> axum::Router {
    let graph = empty_graph(&config);
    let service =
        service::Service::with_config(graph, config.max_pending_jobs(), lifecycle.clone(), &config);
    service.seed_version_for_test(version_str);
    build_router_from_service(service, config, lifecycle)
}

/// Builds an application router with seeded version and/or delivery sequence.
///
/// Only available when the `test-support` feature is enabled.
#[cfg(feature = "test-support")]
pub fn router_with_seeded_state(
    config: ServerConfig,
    lifecycle: Lifecycle,
    version_str: Option<&str>,
    delivery_sequence: Option<u64>,
) -> axum::Router {
    let graph = empty_graph(&config);
    let service =
        service::Service::with_config(graph, config.max_pending_jobs(), lifecycle.clone(), &config);
    if let Some(v) = version_str {
        service.seed_version_for_test(v);
    }
    if let Some(seq) = delivery_sequence {
        service.seed_sequence_for_test(seq);
    }
    build_router_from_service(service, config, lifecycle)
}

fn build_router_from_service(
    service: service::Service,
    config: ServerConfig,
    lifecycle: Lifecycle,
) -> axum::Router {
    use axum::{
        extract::{DefaultBodyLimit, State},
        middleware,
        routing::{get, post},
    };
    let ready_lifecycle = lifecycle.clone();
    let request_timeout = config.request_timeout();
    let metrics = config
        .metrics_enabled()
        .then(|| metrics::Metrics::new(service.clone(), lifecycle.clone()));
    let mut router = axum::Router::new()
        .route(
            api::Route::Health.as_str(),
            get(|| async { axum::Json(Health { status: "ok" }) }),
        )
        .route(
            api::Route::Ready.as_str(),
            get(move |State(service): State<service::Service>| async move {
                lifecycle::readiness(&ready_lifecycle, &service).await
            }),
        )
        .route(api::Route::Info.as_str(), get(api::info))
        .route(api::Route::Stats.as_str(), get(api::stats))
        .route(api::Route::Nodes.as_str(), get(api::list_nodes))
        .route(api::Route::Node.as_str(), get(api::get_node))
        .route(api::Route::NodeEdges.as_str(), get(api::list_node_edges))
        .route(api::Route::Edges.as_str(), get(api::list_edges))
        .route(
            api::Route::Operations.as_str(),
            post(api::single).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::Batch.as_str(),
            post(api::batch).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::FeederSession.as_str(),
            post(api::feeder_session).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::FeederHeartbeat.as_str(),
            post(api::feeder_heartbeat).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::FeederImport.as_str(),
            post(api::feeder_import).layer(DefaultBodyLimit::disable()),
        )
        .route(
            api::Route::FeederEvents.as_str(),
            post(api::feeder_events).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::FeederGap.as_str(),
            post(api::feeder_gap).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            api::Route::FeederBarrier.as_str(),
            post(api::feeder_barrier).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        );
    if let Some(metrics) = &metrics {
        let scrape = std::sync::Arc::clone(metrics);
        router = router.route(
            api::Route::Metrics.as_str(),
            get(move || async move { metrics::render(&scrape) }),
        );
    }
    router = router
        .layer(middleware::from_fn_with_state(
            lifecycle::TimeoutPolicy {
                limit: request_timeout,
                mark_timeouts: service.telemetry().is_some(),
            },
            lifecycle::request_timeout,
        ))
        .layer(middleware::from_fn_with_state(
            lifecycle::GateState {
                lifecycle,
                telemetry: service.telemetry().cloned(),
            },
            lifecycle::shutdown_gate,
        ));
    if let Some(metrics) = metrics {
        // Outside the gate and timeout, so their rejections are counted.
        router = router.layer(middleware::from_fn_with_state(metrics, metrics::track_http));
    }
    router
        .layer(middleware::from_fn(api::logging_middleware))
        .with_state(service)
}

#[derive(serde::Serialize)]
struct Health {
    status: &'static str,
}

/// Immutable effective settings; available without taking the graph mutex.
#[derive(Clone, serde::Serialize)]
pub(crate) struct ServerInfo {
    engine: &'static str,
    /// The engine's qualification ([`knotrel_core::EngineConfig::is_experimental`]),
    /// not a stability class: `true` when its performance, maturity and usage
    /// recommendation carry no promise. It may change from `true` to `false`
    /// in a minor release when the engine is promoted, never from `false` to
    /// `true` for an existing engine (`docs/compatibility.md` §2.2).
    experimental: bool,
    max_pending_jobs: usize,
    max_body_bytes: usize,
    max_batch_operations: usize,
    max_nodes: Option<usize>,
    max_edges: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
    /// Serialized only when node and edge lifecycle support is enabled.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    topology_lifecycle: bool,
    /// Serialized only when the inspection routes are enabled.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    inspection: bool,
    /// Admission permits reserved for feeder events, barriers and gaps;
    /// serialized only in source-managed mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    feeder_reserved_jobs: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_constructors_build_valid_routers() {
        let _ = router(knotrel_core::Graph::new());
        let _ = router_with_config(ServerConfig::default());
        let _ = router_with_lifecycle(ServerConfig::default(), Lifecycle::ready());

        #[cfg(feature = "test-support")]
        {
            let _ = router_with_seeded_version(ServerConfig::default(), Lifecycle::ready(), "42");
            let _ = router_with_seeded_state(
                ServerConfig::default(),
                Lifecycle::ready(),
                Some("100"),
                Some(50),
            );
            let _ =
                router_with_seeded_state(ServerConfig::default(), Lifecycle::ready(), None, None);
        }
    }
}
