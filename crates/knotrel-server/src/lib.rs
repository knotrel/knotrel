//! Axum HTTP transport for a single in-memory Knotrel graph.
//!
//! The server uses exact reference connectivity with bounded blocking work.
//! State is ephemeral and there is no authentication or persistence layer.
//! See the repository's `docs/http-api.md` for wire formats and batch semantics.

mod api;
mod service;

/// Builds the HTTP API around an owned, in-memory graph.
///
/// Routes are `GET /health`, `POST /v1/operations` and `POST /v1/batch`. Request
/// bodies are limited to 1 MiB; batches contain 1–1024 operations. At most 32
/// graph jobs may be queued or running. Excess graph requests receive HTTP 503.
/// A graph mutex isolates each request, including all operations in a batch.
/// Cloned routers share graph state; separate calls construct separate graphs.
///
/// Request handling requires a Tokio runtime. Health reports process liveness,
/// not graph-worker readiness. See `docs/http-api.md` for retry semantics.
pub fn router(graph: knotrel_core::Graph) -> axum::Router {
    use axum::{
        extract::DefaultBodyLimit,
        routing::{get, post},
    };
    axum::Router::new()
        .route(
            "/health",
            get(|| async { axum::Json(Health { status: "ok" }) }),
        )
        .route("/v1/operations", post(api::single))
        .route("/v1/batch", post(api::batch))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(service::Service::new(graph, 32))
}

#[derive(serde::Serialize)]
struct Health {
    status: &'static str,
}
