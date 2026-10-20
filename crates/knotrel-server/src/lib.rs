//! Axum HTTP transport for a single in-memory Knotrel graph.
//!
//! The server uses exact connectivity with bounded blocking work.
//! State is ephemeral and there is no authentication, authorization, TLS or
//! persistence layer: serve it only to trusted local clients. The standalone
//! executable binds to loopback unless explicitly configured otherwise.
//! [`serve`] adds bounded connections, timeouts and graceful shutdown.
//! See the repository's `docs/http-api.md` for wire formats and batch semantics.

mod api;
mod configuration;
mod lifecycle;
mod serve;
mod service;
pub use configuration::{ConfigError, ServerConfig, ServerMode};
#[cfg(feature = "test-support")]
pub use lifecycle::{GateDecision, WorkerPauseHandle};
pub use lifecycle::{Lifecycle, Phase};
pub use serve::{ShutdownOutcome, serve};
#[cfg(feature = "test-support")]
pub use service::{percent_decode_position, percent_encode_position};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_BATCH_OPERATIONS: usize = 1024;

/// Builds the HTTP API around an owned, in-memory graph.
///
/// Routes are `GET /health`, `GET /ready`, `GET /v1/info`, `GET /v1/stats`,
/// `POST /v1/operations` and `POST /v1/batch`. Request
/// bodies are limited to 1 MiB; batches contain 1–1024 operations. At most 32
/// graph jobs may be queued or running. Excess graph requests receive HTTP 503.
/// A graph mutex isolates each request, including all operations in a batch.
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
    build_router(graph.into(), ServerConfig::default(), Lifecycle::ready())
}

/// Builds the HTTP API around an empty graph using validated startup settings.
///
/// The engine, admission limit and request timeout remain fixed for this
/// router's lifetime. The configuration address is not bound here; embedded
/// callers own their listener. Other limits and batch semantics are the same
/// as [`router`]. Readiness is immediately `ready`; use
/// [`router_with_lifecycle`] with [`serve`] for the full lifecycle.
/// This function does not read the environment.
pub fn router_with_config(config: ServerConfig) -> axum::Router {
    router_with_lifecycle(config, Lifecycle::ready())
}

/// Like [`router_with_config`], but readiness and shutdown rejection follow
/// the supplied [`Lifecycle`], which [`serve`] drives.
pub fn router_with_lifecycle(config: ServerConfig, lifecycle: Lifecycle) -> axum::Router {
    build_router(
        knotrel_core::ConnectivityGraph::with_limits(config.engine(), config.graph_limits()),
        config,
        lifecycle,
    )
}

fn build_router(
    graph: knotrel_core::ConnectivityGraph,
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
    let graph =
        knotrel_core::ConnectivityGraph::with_limits(config.engine(), config.graph_limits());
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
    let graph =
        knotrel_core::ConnectivityGraph::with_limits(config.engine(), config.graph_limits());
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
    axum::Router::new()
        .route(
            "/health",
            get(|| async { axum::Json(Health { status: "ok" }) }),
        )
        .route(
            "/ready",
            get(move |State(service): State<service::Service>| async move {
                lifecycle::readiness(&ready_lifecycle, &service)
            }),
        )
        .route("/v1/info", get(api::info))
        .route("/v1/stats", get(api::stats))
        .route(
            "/v1/operations",
            post(api::single).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/batch",
            post(api::batch).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/feeder/session",
            post(api::feeder_session).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/feeder/heartbeat",
            post(api::feeder_heartbeat).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/feeder/import",
            post(api::feeder_import).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/v1/feeder/events",
            post(api::feeder_events).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/feeder/gap",
            post(api::feeder_gap).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .layer(middleware::from_fn_with_state(
            request_timeout,
            lifecycle::request_timeout,
        ))
        .layer(middleware::from_fn_with_state(
            lifecycle,
            lifecycle::shutdown_gate,
        ))
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
}
