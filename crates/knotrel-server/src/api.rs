//! Wire types and HTTP handlers for the versioned graph API.

use crate::service::{Service, ServiceError};
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
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

/// Executes a single parsed operation.
pub(crate) async fn single(
    State(service): State<Service>,
    request: Result<Json<Operation>, JsonRejection>,
) -> Response {
    let operation = match request {
        Ok(Json(operation)) => operation,
        Err(error) => return invalid_json(error),
    };
    match service.execute(vec![operation]).await {
        Ok(mut results) => results.remove(0).into_response(),
        Err(error) => service_error(error),
    }
}

/// Runs a bounded, ordered batch without rollback on individual graph errors.
pub(crate) async fn batch(
    State(service): State<Service>,
    request: Result<Json<Batch>, JsonRejection>,
) -> Response {
    let batch = match request {
        Ok(Json(batch)) => batch,
        Err(error) => return invalid_json(error),
    };
    if batch.operations.is_empty() || batch.operations.len() > crate::MAX_BATCH_OPERATIONS {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_batch_size",
            format!(
                "batches must contain 1 to {} operations",
                crate::MAX_BATCH_OPERATIONS
            ),
        );
    }
    match service.execute(batch.operations).await {
        Ok(results) => Json(BatchResult { results }).into_response(),
        Err(error) => service_error(error),
    }
}

fn invalid_json(error: JsonRejection) -> Response {
    error_response(error.status(), "invalid_request", error.body_text())
}

fn service_error(error: ServiceError) -> Response {
    let (code, message) = match error {
        ServiceError::Busy => ("busy", "request capacity exhausted; retry later"),
        ServiceError::Unavailable => ("unavailable", "graph worker unavailable"),
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
