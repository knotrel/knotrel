//! Wire types and HTTP handlers for the versioned graph API.

use crate::service::{Service, ServiceError};
use axum::{
    Json,
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use knotrel_core::{ConnectivityGraph, GraphError};
use serde::{Deserialize, Serialize};

/// A decimal-string node identifier with lossless JSON representation.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct WireNode(pub(crate) u64);

impl TryFrom<String> for WireNode {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("node IDs must be nonempty strings of ASCII decimal digits");
        }
        value.parse().map(Self).map_err(|_| "node ID exceeds u64")
    }
}

/// One operation, deserialized completely before graph execution.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    AddNode { node: WireNode },
    Link { source: WireNode, target: WireNode },
    Cut { source: WireNode, target: WireNode },
    Connected { source: WireNode, target: WireNode },
}

impl Operation {
    /// Applies one operation, keeping graph errors local to this operation.
    pub(crate) fn apply(self, graph: &mut ConnectivityGraph) -> Outcome {
        match self {
            Self::AddNode { node } => graph
                .add_node(node.0)
                .map(|changed| Outcome::Changed { changed })
                .unwrap_or_else(Outcome::from),
            Self::Link { source, target } => graph
                .link(source.0, target.0)
                .map(|changed| Outcome::Changed { changed })
                .unwrap_or_else(Outcome::from),
            Self::Cut { source, target } => graph
                .cut(source.0, target.0)
                .map(|changed| Outcome::Changed { changed })
                .unwrap_or_else(Outcome::from),
            Self::Connected { source, target } => graph
                .connected(source.0, target.0)
                .map(|connected| Outcome::Connected { connected })
                .unwrap_or_else(Outcome::from),
        }
    }
}

/// An operation result; batches preserve errors at the corresponding index.
#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Changed { changed: bool },
    Connected { connected: bool },
    Error { code: &'static str, message: String },
}

impl From<GraphError> for Outcome {
    fn from(error: GraphError) -> Self {
        let code = match error {
            GraphError::NodeLimitExceeded { .. } => "node_limit_exceeded",
            GraphError::EdgeLimitExceeded { .. } => "edge_limit_exceeded",
            GraphError::SelfLoop { .. } => "self_loop",
            GraphError::UnknownNode { .. } => "unknown_node",
        };
        Self::Error {
            code,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for Outcome {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Error {
                code: "unknown_node",
                ..
            } => StatusCode::NOT_FOUND,
            Self::Error { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::OK,
        };
        (status, Json(self)).into_response()
    }
}

/// An envelope whose whole JSON representation must validate before execution.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Batch {
    operations: Vec<Operation>,
}

#[derive(Serialize)]
struct BatchResult {
    results: Vec<Outcome>,
}

/// SIMD-accelerated JSON extractor parsing request bodies in-place.
pub(crate) struct SimdJson<T>(pub(crate) T);

impl<S, T> FromRequest<S> for SimdJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        if let Some(rejection) = check_json_content_type(&parts.headers) {
            tracing::debug!("rejected request with invalid or missing Content-Type");
            return Err(rejection);
        }
        let bytes = Bytes::from_request(Request::from_parts(parts, body), state)
            .await
            .map_err(|err| {
                tracing::warn!(status = %err.status(), "failed to read request body bytes");
                error_response(err.status(), "invalid_request", err.body_text())
            })?;
        let mut buffer = bytes.to_vec();
        let value = simd_json::from_slice(&mut buffer).map_err(|err| {
            let status = if err.is_data() {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::BAD_REQUEST
            };
            tracing::debug!(%err, is_data = err.is_data(), "failed to parse request JSON with simd-json");
            error_response(status, "invalid_request", err.to_string())
        })?;
        Ok(Self(value))
    }
}

fn check_json_content_type(headers: &HeaderMap) -> Option<Response> {
    let valid = headers
        .get(CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .and_then(|value| value.parse::<mime::Mime>().ok())
        .is_some_and(|value| {
            value.type_() == mime::APPLICATION
                && (value.subtype() == mime::JSON || value.suffix() == Some(mime::JSON))
        });
    if valid {
        None
    } else {
        Some(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_request",
            "Expected request with `Content-Type: application/json`".into(),
        ))
    }
}

/// Traces incoming HTTP requests, recording method, path, response status, and duration.
pub(crate) async fn logging_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().path().to_string();
    let start = std::time::Instant::now();

    let response = next.run(request).await;

    let latency = start.elapsed();
    let status = response.status().as_u16();

    // Log health checks at trace level to avoid log pollution in orchestration environments.
    if uri == "/health" {
        tracing::trace!(
            method = %method,
            uri = %uri,
            status,
            latency_us = latency.as_micros(),
            "health check"
        );
    } else {
        tracing::info!(
            method = %method,
            uri = %uri,
            status,
            latency_us = latency.as_micros(),
            "handled request"
        );
    }

    response
}

/// Executes a single parsed operation.
pub(crate) async fn single(
    State(service): State<Service>,
    request: Result<SimdJson<Operation>, Response>,
) -> Response {
    let SimdJson(operation) = match request {
        Ok(operation) => operation,
        Err(response) => return response,
    };
    tracing::debug!("executing single operation");
    match service.execute(vec![operation]).await {
        Ok(mut results) => results.remove(0).into_response(),
        Err(error) => service_error(error),
    }
}

/// Runs a bounded, ordered batch without rollback on individual graph errors.
pub(crate) async fn batch(
    State(service): State<Service>,
    request: Result<SimdJson<Batch>, Response>,
) -> Response {
    let SimdJson(batch) = match request {
        Ok(batch) => batch,
        Err(response) => return response,
    };
    if batch.operations.is_empty() || batch.operations.len() > crate::MAX_BATCH_OPERATIONS {
        tracing::warn!(
            count = batch.operations.len(),
            max = crate::MAX_BATCH_OPERATIONS,
            "rejected batch exceeding size bounds"
        );
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_batch_size",
            format!(
                "batches must contain 1 to {} operations",
                crate::MAX_BATCH_OPERATIONS
            ),
        );
    }
    tracing::debug!(count = batch.operations.len(), "executing batch operations");
    match service.execute(batch.operations).await {
        Ok(results) => Json(BatchResult { results }).into_response(),
        Err(error) => service_error(error),
    }
}

fn service_error(error: ServiceError) -> Response {
    let (code, message) = match error {
        ServiceError::Busy => ("busy", "request capacity exhausted; retry later"),
        ServiceError::Unavailable => {
            tracing::error!("graph worker unavailable while handling request");
            ("unavailable", "graph worker unavailable")
        }
    };
    error_response(StatusCode::SERVICE_UNAVAILABLE, code, message.into())
}

fn error_response(status: StatusCode, code: &'static str, message: String) -> Response {
    (status, Json(Outcome::Error { code, message })).into_response()
}

/// Returns startup metadata without taking the graph lock or a worker permit.
pub(crate) async fn info(State(service): State<Service>) -> Json<crate::ServerInfo> {
    Json(service.info())
}

/// Reads live graph state through bounded worker admission and the graph mutex.
pub(crate) async fn stats(State(service): State<Service>) -> Response {
    let mut response = match service.stats().await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => service_error(error),
    };
    // Monitoring reads must not be served from an intermediary's stored response.
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
