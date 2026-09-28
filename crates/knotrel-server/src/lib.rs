//! Axum HTTP transport for a single in-memory Knotrel graph.
//!
//! The server uses exact connectivity with bounded blocking work.
//! State is ephemeral and there is no authentication or persistence layer.
//! See the repository's `docs/http-api.md` for wire formats and batch semantics.

mod api;
mod configuration;
pub use configuration::{ConfigError, ServerConfig};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_BATCH_OPERATIONS: usize = 1024;
mod service;

/// Builds the HTTP API around an owned, in-memory graph.
///
/// Routes are `GET /health`, `GET /v1/info`, `POST /v1/operations` and
/// `POST /v1/batch`. Request
/// bodies are limited to 1 MiB; batches contain 1–1024 operations. At most 32
/// graph jobs may be queued or running. Excess graph requests receive HTTP 503.
/// A graph mutex isolates each request, including all operations in a batch.
/// Cloned routers share graph state; separate calls construct separate graphs.
///
/// Request handling requires a Tokio runtime. Health reports process liveness,
/// not graph-worker readiness. See `docs/http-api.md` for retry semantics.
pub fn router(graph: knotrel_core::Graph) -> axum::Router {
    build_router(graph.into(), ServerConfig::default().max_pending_jobs())
}

/// Builds the HTTP API around an empty graph using validated startup settings.
///
/// The engine and admission limit remain fixed for this router's lifetime.
/// The configuration address is not bound here; embedded callers own their
/// listener. Other limits and batch semantics are the same as [`router`].
/// This function does not read the environment.
pub fn router_with_config(config: ServerConfig) -> axum::Router {
    build_router(
        knotrel_core::ConnectivityGraph::new(config.engine()),
        config.max_pending_jobs(),
    )
}

fn build_router(graph: knotrel_core::ConnectivityGraph, max_pending_jobs: usize) -> axum::Router {
    use axum::{
        extract::DefaultBodyLimit,
        routing::{get, post},
    };
    axum::Router::new()
        .route(
            "/health",
            get(|| async { axum::Json(Health { status: "ok" }) }),
        )
        .route("/v1/info", get(api::info))
        .route("/v1/operations", post(api::single))
        .route("/v1/batch", post(api::batch))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(service::Service::new(graph, max_pending_jobs))
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
}
