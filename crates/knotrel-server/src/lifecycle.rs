//! Process lifecycle phase shared by the accept loop and the HTTP router.

use crate::{api, service::Service};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

const STARTING: u8 = 0;
const READY: u8 = 1;
const DRAINING: u8 = 2;

/// Externally visible server phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// The server exists but is not yet accepting connections.
    Starting,
    /// The server is accepting connections and serving requests.
    Ready,
    /// Shutdown has begun: admitted work finishes, everything else is rejected.
    Draining,
}

/// Shared handle for the lifecycle phase.
///
/// Phases only move forward: `Starting`, then `Ready`, then `Draining`.
/// Cloned handles observe the same phase.
#[derive(Clone, Debug)]
pub struct Lifecycle(Arc<AtomicU8>);

impl Lifecycle {
    /// Creates a handle in [`Phase::Starting`].
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(STARTING)))
    }

    /// Creates a handle that already reports [`Phase::Ready`].
    pub(crate) fn ready() -> Self {
        Self(Arc::new(AtomicU8::new(READY)))
    }

    /// Returns the current phase.
    pub fn phase(&self) -> Phase {
        match self.0.load(Ordering::Acquire) {
            STARTING => Phase::Starting,
            READY => Phase::Ready,
            _ => Phase::Draining,
        }
    }

    /// Moves `Starting` to `Ready`; has no effect once draining has begun.
    pub fn mark_ready(&self) {
        let _ = self
            .0
            .compare_exchange(STARTING, READY, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Moves any phase to `Draining`; this is irreversible.
    pub fn begin_draining(&self) {
        self.0.store(DRAINING, Ordering::Release);
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(serde::Serialize)]
struct Status {
    status: &'static str,
}

/// Reports whether the server can currently accept graph work.
///
/// Draining wins over every other state, then starting, then a failed graph
/// worker. It never takes the graph lock or a worker permit.
pub(crate) fn readiness(lifecycle: &Lifecycle, service: &Service) -> Response {
    let (status, label) = match lifecycle.phase() {
        Phase::Draining => (StatusCode::SERVICE_UNAVAILABLE, "draining"),
        Phase::Starting => (StatusCode::SERVICE_UNAVAILABLE, "starting"),
        Phase::Ready if service.is_unavailable() => {
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
        Phase::Ready => (StatusCode::OK, "ready"),
    };
    let mut response = (status, Json(Status { status: label })).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn is_probe(path: &str) -> bool {
    path == "/health" || path == "/ready"
}

fn close_connection(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
}

/// Rejects new work while draining and asks clients to close the connection.
///
/// Probes stay answerable so orchestrators can observe the drain. A request
/// that passed this gate before draining began runs to completion.
pub(crate) async fn shutdown_gate(
    State(lifecycle): State<Lifecycle>,
    request: Request,
    next: Next,
) -> Response {
    if lifecycle.phase() != Phase::Draining {
        return next.run(request).await;
    }
    let mut response = if is_probe(request.uri().path()) {
        next.run(request).await
    } else {
        api::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "server is shutting down; retry against another instance".into(),
        )
    };
    close_connection(&mut response);
    response
}

/// Bounds body receipt plus handling of one request.
///
/// Dropping the handler future cancels body reads and queued admission, but a
/// graph job already handed to a worker still completes: after a timeout the
/// client cannot tell whether its mutation was applied.
pub(crate) async fn request_timeout(
    State(limit): State<Duration>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                limit_ms = limit.as_millis(),
                "request exceeded its time limit"
            );
            let mut response = api::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "request_timeout",
                "request exceeded the server time limit; its effect is unknown".into(),
            );
            close_connection(&mut response);
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_only_move_forward() {
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.phase(), Phase::Starting);
        lifecycle.mark_ready();
        assert_eq!(lifecycle.phase(), Phase::Ready);
        let clone = lifecycle.clone();
        clone.begin_draining();
        assert_eq!(lifecycle.phase(), Phase::Draining);
        lifecycle.mark_ready();
        assert_eq!(lifecycle.phase(), Phase::Draining);
        let early = Lifecycle::new();
        early.begin_draining();
        early.mark_ready();
        assert_eq!(early.phase(), Phase::Draining);
    }
}
