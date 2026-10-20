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

/// A pair of wire nodes representing an edge.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireEdge {
    pub(crate) source: WireNode,
    pub(crate) target: WireNode,
}

/// Mode of graph import: initial creation or atomic replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImportMode {
    CreateOnly,
    Replace,
}

/// Request payload for atomic graph import.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportRequest {
    pub(crate) mode: ImportMode,
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) source_id: String,
    pub(crate) source_epoch: String,
    pub(crate) source_position: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) nodes: Vec<WireNode>,
    pub(crate) edges: Vec<WireEdge>,
}

/// Response payload for successful atomic graph import.
#[derive(Debug, Serialize)]
pub(crate) struct ImportResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) generation: u64,
    pub(crate) state_version: String,
    pub(crate) node_count: usize,
    pub(crate) edge_count: usize,
    pub(crate) source_position: String,
    pub(crate) status: &'static str,
}

/// Request payload for feeder session registration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionRequest {
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) lease_duration_ms: u64,
}

/// Response payload for successful feeder session registration.
#[derive(Debug, Serialize)]
pub(crate) struct SessionResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) lease_duration_ms: u64,
    pub(crate) generation: u64,
    pub(crate) applied_delivery_sequence: u64,
    pub(crate) applied_source_position: Option<String>,
    pub(crate) status: crate::service::SyncStatus,
}

/// Request payload for feeder heartbeat.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeartbeatRequest {
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
}

/// Response payload for successful feeder heartbeat.
#[derive(Debug, Serialize)]
pub(crate) struct HeartbeatResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) lease_expires_in_ms: u64,
    pub(crate) status: crate::service::SyncStatus,
}

/// A feeder event operation descriptor.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeederEvent {
    pub(crate) event_id: String,
    pub(crate) delivery_sequence: u64,
    pub(crate) prev_delivery_sequence: u64,
    pub(crate) source_position: String,
    pub(crate) prev_source_position: String,
    pub(crate) op: String,
    #[serde(default)]
    pub(crate) source: Option<WireNode>,
    #[serde(default)]
    pub(crate) target: Option<WireNode>,
    #[serde(default)]
    pub(crate) node: Option<WireNode>,
}

/// Request payload for ordered feeder incremental events.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeederEventsRequest {
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) generation: u64,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) source_id: String,
    pub(crate) source_epoch: String,
    pub(crate) events: Vec<FeederEvent>,
}

/// Response payload for successfully applied feeder incremental events.
#[derive(Debug, Serialize)]
pub(crate) struct FeederEventsResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) generation: u64,
    pub(crate) applied_delivery_sequence: u64,
    pub(crate) applied_source_position: String,
    pub(crate) state_version: String,
    pub(crate) events_applied: usize,
}

/// Response payload for an exact duplicate retry.
#[derive(Debug, Serialize)]
pub(crate) struct FeederDuplicateResponse {
    pub(crate) result: &'static str,
    pub(crate) status: &'static str,
    pub(crate) instance_id: String,
    pub(crate) generation: u64,
    pub(crate) event_id: String,
    pub(crate) delivery_sequence: u64,
    pub(crate) historical_outcome: HistoricalOutcome,
    pub(crate) current_applied_delivery_sequence: u64,
    pub(crate) current_applied_source_position: String,
    pub(crate) current_state_version: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct HistoricalOutcome {
    pub(crate) changed: bool,
}

/// Error detail inside a partial batch failure.
#[derive(Debug, Serialize)]
pub(crate) struct BatchItemError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

/// Response payload for a feeder batch stopped on error.
#[derive(Debug, Serialize)]
pub(crate) struct PartialBatchFailure {
    pub(crate) result: &'static str,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) instance_id: String,
    pub(crate) generation: u64,
    pub(crate) applied_delivery_sequence: u64,
    pub(crate) applied_source_position: String,
    pub(crate) state_version: String,
    pub(crate) failed_batch_index: usize,
    pub(crate) failed_delivery_sequence: u64,
    pub(crate) failed_event_id: String,
    pub(crate) error: BatchItemError,
}

/// Request payload for declaring an unrecoverable source gap.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GapRequest {
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) generation: u64,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) failed_delivery_sequence: u64,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

/// Response payload for acknowledging a declared source gap.
#[derive(Debug, Serialize)]
pub(crate) struct GapResponse {
    pub(crate) result: &'static str,
    pub(crate) instance_id: String,
    pub(crate) generation: u64,
    pub(crate) status: crate::service::SyncStatus,
    pub(crate) action_required: &'static str,
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
    Changed {
        changed: bool,
    },
    Connected {
        connected: bool,
    },
    Error {
        code: &'static str,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_sequence: Option<u64>,
    },
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
            expected_sequence: None,
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
            Self::Error {
                code: "desynchronized",
                ..
            } => StatusCode::SERVICE_UNAVAILABLE,
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
            error_response(status, "invalid_request", request_error_message(&err))
        })?;
        Ok(Self(value))
    }
}

/// Builds a stable client message without parser internals or echoed input bytes.
///
/// Serde messages are passed through only when their text comes from the fixed
/// schema (field names, our own node-ID text). Others, such as unknown variants
/// or invalid types, quote client input and are replaced by a generic message.
pub(crate) fn request_error_message(err: &simd_json::Error) -> String {
    match err.error() {
        simd_json::ErrorType::Serde(message)
            if err.is_data()
                && (message.starts_with("missing field `")
                    || message.starts_with("duplicate field `")
                    || message.starts_with("node IDs must be")) =>
        {
            message.clone()
        }
        _ if err.is_data() => "request body does not match the expected shape".into(),
        _ => "request body is not valid JSON".into(),
    }
}

pub(crate) fn check_json_content_type(headers: &HeaderMap) -> Option<Response> {
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

    // Log probes at trace level to avoid log pollution in orchestration environments.
    if uri == "/health" || uri == "/ready" {
        tracing::trace!(
            method = %method,
            uri = %uri,
            status,
            latency_us = latency.as_micros(),
            "probe"
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
    if service.mode() == crate::ServerMode::SourceManaged
        && !matches!(operation, Operation::Connected { .. })
    {
        return error_response(
            StatusCode::CONFLICT,
            "direct_mutation_disabled",
            "direct graph mutations are disabled in source-managed mode; use feeder endpoints"
                .into(),
        );
    }
    tracing::debug!("executing single operation");
    match service.execute(vec![operation]).await {
        Ok(mut exec_result) => {
            let mut response = exec_result.outcomes.remove(0).into_response();
            inject_metadata_headers(response.headers_mut(), &exec_result.metadata);
            response
        }
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
    if service.mode() == crate::ServerMode::SourceManaged {
        let has_mutation = batch
            .operations
            .iter()
            .any(|op| !matches!(op, Operation::Connected { .. }));
        if has_mutation {
            return error_response(
                StatusCode::CONFLICT,
                "direct_mutation_disabled",
                "direct graph mutations are disabled in source-managed mode; use feeder endpoints"
                    .into(),
            );
        }
    }
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
        Ok(exec_result) => {
            let mut response = Json(BatchResult {
                results: exec_result.outcomes,
            })
            .into_response();
            inject_metadata_headers(response.headers_mut(), &exec_result.metadata);
            response
        }
        Err(error) => service_error(error),
    }
}

/// Registers an active feeder session.
pub(crate) async fn feeder_session(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<SessionRequest>, Response>,
) -> Response {
    if service.mode() != crate::ServerMode::SourceManaged {
        return error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        );
    }
    let Some(instance_id_header) = headers.get("x-knotrel-instance-id") else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "missing header X-Knotrel-Instance-Id".into(),
        ));
    };
    let Ok(header_val) = instance_id_header.to_str() else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "invalid header X-Knotrel-Instance-Id".into(),
        ));
    };
    if header_val != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "header X-Knotrel-Instance-Id must match server instance ID".into(),
        );
    }
    let SimdJson(session_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if session_req.instance_id != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "body instance_id must match server instance ID".into(),
        );
    }
    match service.register_session(session_req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => feeder_error(err),
    }
}

/// Renews an active feeder session lease.
pub(crate) async fn feeder_heartbeat(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<HeartbeatRequest>, Response>,
) -> Response {
    if service.mode() != crate::ServerMode::SourceManaged {
        return error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        );
    }
    let Some(instance_id_header) = headers.get("x-knotrel-instance-id") else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "missing header X-Knotrel-Instance-Id".into(),
        ));
    };
    let Ok(header_val) = instance_id_header.to_str() else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "invalid header X-Knotrel-Instance-Id".into(),
        ));
    };
    if header_val != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "header X-Knotrel-Instance-Id must match server instance ID".into(),
        );
    }
    let SimdJson(heartbeat_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if heartbeat_req.instance_id != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "body instance_id must match server instance ID".into(),
        );
    }
    match service.heartbeat(heartbeat_req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => feeder_error(err),
    }
}

/// Atomically imports a complete graph topology.
///
/// Validates headers and acquires the single import permit BEFORE reading any body bytes.
pub(crate) async fn feeder_import(
    State(service): State<Service>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    if service.mode() != crate::ServerMode::SourceManaged {
        return error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        );
    }
    let Some(instance_id_header) = headers.get("x-knotrel-instance-id") else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "missing header X-Knotrel-Instance-Id".into(),
        ));
    };
    let Ok(instance_id_str) = instance_id_header.to_str() else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "invalid header X-Knotrel-Instance-Id".into(),
        ));
    };
    if instance_id_str != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            format!(
                "header X-Knotrel-Instance-Id '{instance_id_str}' does not match server instance ID '{}'",
                service.instance_id()
            ),
        );
    }
    if let Some(rejection) = check_json_content_type(&headers) {
        return rejection;
    }
    if service.is_unavailable() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "graph worker unavailable".into(),
        );
    }
    let if_match = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // 1. Acquire the single owned import permit BEFORE consuming body bytes!
    let permit = match service.acquire_import_permit() {
        Ok(permit) => permit,
        Err(err) => return feeder_error(err),
    };

    // 2. Read body bytes with bounded limit under acquired permit
    let max_bytes = service.max_import_body_bytes();
    let bytes = match axum::body::to_bytes(body, max_bytes).await {
        Ok(bytes) => bytes,
        Err(_) => return feeder_error(crate::service::FeederError::PayloadTooLarge),
    };

    // 3. Dispatch to service.import, transferring owned permit and raw bytes
    match service.import(permit, bytes, if_match).await {
        Ok(res) => {
            let metadata = crate::service::ResponseMetadata {
                instance_id: res.instance_id.clone(),
                graph_id: "default",
                generation: res.generation,
                state_version: res.state_version.clone(),
                source_position: Some(res.source_position.clone()),
                sync_status: Some(crate::service::SyncStatus::CatchingUp),
            };
            let mut response = (StatusCode::CREATED, Json(res)).into_response();
            inject_metadata_headers(response.headers_mut(), &metadata);
            response
        }
        Err(err) => feeder_error(err),
    }
}

/// Applies an ordered stream of feeder events under the graph mutex.
pub(crate) async fn feeder_events(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<FeederEventsRequest>, Response>,
) -> Response {
    if service.mode() != crate::ServerMode::SourceManaged {
        return error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        );
    }
    let Some(instance_id_header) = headers.get("x-knotrel-instance-id") else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "missing header X-Knotrel-Instance-Id".into(),
        ));
    };
    let Ok(header_val) = instance_id_header.to_str() else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "invalid header X-Knotrel-Instance-Id".into(),
        ));
    };
    if header_val != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "header X-Knotrel-Instance-Id must match server instance ID".into(),
        );
    }
    let SimdJson(events_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if events_req.instance_id != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "body instance_id must match server instance ID".into(),
        );
    }
    if events_req.graph_id != "default" {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_graph_id",
            format!(
                "unsupported graph ID '{}'; only 'default' is supported",
                events_req.graph_id
            ),
        );
    }
    if service.is_unavailable() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "graph worker unavailable".into(),
        );
    }
    match service.apply_events(events_req).await {
        Ok(crate::service::FeederEventsResult::Applied { response, metadata }) => {
            let mut res = (StatusCode::OK, Json(response)).into_response();
            inject_metadata_headers(res.headers_mut(), &metadata);
            res
        }
        Ok(crate::service::FeederEventsResult::Duplicate { response, metadata }) => {
            let mut res = (StatusCode::OK, Json(response)).into_response();
            inject_metadata_headers(res.headers_mut(), &metadata);
            res
        }
        Ok(crate::service::FeederEventsResult::PartialFailure {
            failure,
            metadata,
            status,
        }) => {
            let mut res = (status, Json(failure)).into_response();
            inject_metadata_headers(res.headers_mut(), &metadata);
            res
        }
        Err(err) => feeder_error(err),
    }
}

/// Acknowledges a feeder declared unrecoverable source gap, moving server to Desynchronized.
pub(crate) async fn feeder_gap(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<GapRequest>, Response>,
) -> Response {
    if service.mode() != crate::ServerMode::SourceManaged {
        return error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        );
    }
    let Some(instance_id_header) = headers.get("x-knotrel-instance-id") else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "missing header X-Knotrel-Instance-Id".into(),
        ));
    };
    let Ok(header_val) = instance_id_header.to_str() else {
        return feeder_error(crate::service::FeederError::InvalidInstanceId(
            "invalid header X-Knotrel-Instance-Id".into(),
        ));
    };
    if header_val != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "header X-Knotrel-Instance-Id must match server instance ID".into(),
        );
    }
    let SimdJson(gap_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if gap_req.instance_id != service.instance_id() {
        return error_response(
            StatusCode::CONFLICT,
            "stale_instance_id",
            "body instance_id must match server instance ID".into(),
        );
    }
    if gap_req.graph_id != "default" {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_graph_id",
            format!(
                "unsupported graph ID '{}'; only 'default' is supported",
                gap_req.graph_id
            ),
        );
    }
    if service.is_unavailable() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "graph worker unavailable".into(),
        );
    }
    match service.declare_gap(gap_req).await {
        Ok((res, metadata)) => {
            let mut response = (StatusCode::OK, Json(res)).into_response();
            inject_metadata_headers(response.headers_mut(), &metadata);
            response
        }
        Err(err) => feeder_error(err),
    }
}

fn feeder_error(err: crate::service::FeederError) -> Response {
    use crate::service::FeederError;
    match err {
        FeederError::ImportInProgress => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "import_in_progress",
            "import capacity exhausted; another import is in progress".into(),
        ),
        FeederError::Unavailable => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "graph worker unavailable".into(),
        ),
        FeederError::ModeMismatch => error_response(
            StatusCode::CONFLICT,
            "mode_conflict",
            "feeder endpoints are only available in source_managed mode".into(),
        ),
        FeederError::StaleInstanceId(msg) => {
            error_response(StatusCode::CONFLICT, "stale_instance_id", msg)
        }
        FeederError::InvalidInstanceId(msg) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_instance_id", msg)
        }
        FeederError::InvalidGraphId(msg) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_graph_id", msg)
        }
        FeederError::SessionRequired => error_response(
            StatusCode::CONFLICT,
            "session_required",
            "active feeder session required; register via POST /v1/feeder/session".into(),
        ),
        FeederError::FencedSession => error_response(
            StatusCode::CONFLICT,
            "fenced_feeder_session",
            "feeder session or epoch is stale/fenced".into(),
        ),
        FeederError::LeaseExpired => error_response(
            StatusCode::CONFLICT,
            "lease_expired",
            "feeder lease expired; renew lease or register new session".into(),
        ),
        FeederError::GraphAlreadyInitialized => error_response(
            StatusCode::CONFLICT,
            "graph_already_initialized",
            "create_only requires uninitialized graph (generation 0)".into(),
        ),
        FeederError::GraphUninitialized => error_response(
            StatusCode::CONFLICT,
            "graph_uninitialized",
            "graph is uninitialized; baseline import required".into(),
        ),
        FeederError::GraphDesynchronized => error_response(
            StatusCode::CONFLICT,
            "graph_desynchronized",
            "graph is desynchronized; full rebuild required".into(),
        ),
        FeederError::GenerationExhausted => error_response(
            StatusCode::CONFLICT,
            "generation_exhausted",
            "generation counter exhausted; server restart required".into(),
        ),
        FeederError::StaleGeneration(msg) => {
            error_response(StatusCode::CONFLICT, "stale_generation", msg)
        }
        FeederError::SourceMismatch(msg) => {
            error_response(StatusCode::CONFLICT, "source_mismatch", msg)
        }
        FeederError::SourceEpochMismatch(msg) => {
            error_response(StatusCode::CONFLICT, "source_epoch_mismatch", msg)
        }
        FeederError::OutOfOrderSequence { expected, received } => (
            StatusCode::CONFLICT,
            Json(Outcome::Error {
                code: "out_of_order_sequence",
                message: format!(
                    "expected delivery sequence {}, received {}",
                    expected, received
                ),
                expected_sequence: Some(expected),
            }),
        )
            .into_response(),
        FeederError::SourcePredecessorMismatch(msg) => {
            error_response(StatusCode::CONFLICT, "source_predecessor_mismatch", msg)
        }
        FeederError::ConflictingDuplicate(msg) => {
            error_response(StatusCode::CONFLICT, "conflicting_duplicate", msg)
        }
        FeederError::UnverifiableDuplicate(msg) => {
            error_response(StatusCode::CONFLICT, "unverifiable_duplicate", msg)
        }
        FeederError::SequenceExhausted => error_response(
            StatusCode::CONFLICT,
            "sequence_exhausted",
            "delivery sequence counter exhausted; server restart required".into(),
        ),
        FeederError::PreconditionFailed(msg) => {
            error_response(StatusCode::PRECONDITION_FAILED, "precondition_failed", msg)
        }
        FeederError::InvalidLeaseDuration => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_lease_duration",
            "lease_duration_ms must be between 1000 and 60000".into(),
        ),
        FeederError::MalformedJson => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "request body is not valid JSON".into(),
        ),
        FeederError::UnprocessableJson(msg) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request", msg)
        }
        FeederError::PayloadTooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "import payload exceeds size limit".into(),
        ),
        FeederError::Domain(code, msg) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, code, msg)
        }
        FeederError::InvalidRequest(msg) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request", msg)
        }
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

pub(crate) fn error_response(status: StatusCode, code: &'static str, message: String) -> Response {
    (
        status,
        Json(Outcome::Error {
            code,
            message,
            expected_sequence: None,
        }),
    )
        .into_response()
}

/// Returns startup metadata without taking the graph lock or a worker permit.
pub(crate) async fn info(State(service): State<Service>) -> Json<crate::ServerInfo> {
    Json(service.info())
}

/// Reads live graph state through bounded worker admission and the graph mutex.
pub(crate) async fn stats(State(service): State<Service>) -> Response {
    let mut response = match service.stats().await {
        Ok(stats_result) => {
            let mut res = Json(stats_result.snapshot).into_response();
            inject_metadata_headers(res.headers_mut(), &stats_result.metadata);
            res
        }
        Err(error) => service_error(error),
    };
    // Monitoring reads must not be served from an intermediary's stored response.
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Injects coherent graph identity and version headers into an HTTP response.
pub(crate) fn inject_metadata_headers(
    headers: &mut HeaderMap,
    metadata: &crate::service::ResponseMetadata,
) {
    if let Ok(val) = axum::http::HeaderValue::from_str(&metadata.etag()) {
        headers.insert(axum::http::header::ETAG, val);
    }
    if let Ok(val) = axum::http::HeaderValue::from_str(&metadata.instance_id) {
        headers.insert(
            axum::http::HeaderName::from_static("x-knotrel-instance-id"),
            val,
        );
    }
    headers.insert(
        axum::http::HeaderName::from_static("x-knotrel-graph-id"),
        axum::http::HeaderValue::from_static("default"),
    );
    if let Ok(val) = axum::http::HeaderValue::from_str(&metadata.generation.to_string()) {
        headers.insert(
            axum::http::HeaderName::from_static("x-knotrel-generation"),
            val,
        );
    }
    if let Ok(val) = axum::http::HeaderValue::from_str(&metadata.state_version) {
        headers.insert(
            axum::http::HeaderName::from_static("x-knotrel-state-version"),
            val,
        );
    }
    if let Some(ref pos) = metadata.source_position {
        let encoded = crate::service::percent_encode_position(pos);
        if let Ok(val) = axum::http::HeaderValue::from_str(&encoded) {
            headers.insert(
                axum::http::HeaderName::from_static("x-knotrel-source-position"),
                val,
            );
        }
    }
    if let Some(status) = metadata.sync_status {
        headers.insert(
            axum::http::HeaderName::from_static("x-knotrel-sync-status"),
            axum::http::HeaderValue::from_static(status.as_str()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lifecycle, ServerConfig, ServerMode};
    use axum::http::HeaderValue;

    fn make_service(mode: ServerMode) -> Service {
        let config = ServerConfig::default().with_mode(mode);
        Service::with_config(ConnectivityGraph::default(), 2, Lifecycle::new(), &config)
    }

    #[test]
    fn test_wire_node_parsing() {
        assert_eq!(WireNode::try_from("123".to_string()).unwrap().0, 123);
        assert_eq!(WireNode::try_from("0".to_string()).unwrap().0, 0);
        assert!(WireNode::try_from("".to_string()).is_err());
        assert!(WireNode::try_from("abc".to_string()).is_err());
        assert!(WireNode::try_from("-1".to_string()).is_err());
        assert!(WireNode::try_from("18446744073709551616".to_string()).is_err());
    }

    #[test]
    fn test_inject_metadata_headers() {
        let mut headers = HeaderMap::new();
        let metadata = crate::service::ResponseMetadata {
            instance_id: "srv-test-1".into(),
            graph_id: "default",
            generation: 2,
            state_version: "42".into(),
            source_position: Some("pos/1".into()),
            sync_status: Some(crate::service::SyncStatus::CatchingUp),
        };
        inject_metadata_headers(&mut headers, &metadata);
        assert_eq!(headers.get("etag").unwrap(), "\"2.42.pos%2F1\"");
        assert_eq!(headers.get("x-knotrel-instance-id").unwrap(), "srv-test-1");
        assert_eq!(headers.get("x-knotrel-graph-id").unwrap(), "default");
        assert_eq!(headers.get("x-knotrel-generation").unwrap(), "2");
        assert_eq!(headers.get("x-knotrel-state-version").unwrap(), "42");
        assert_eq!(headers.get("x-knotrel-source-position").unwrap(), "pos%2F1");
        assert_eq!(headers.get("x-knotrel-sync-status").unwrap(), "catching_up");
    }

    #[tokio::test]
    async fn test_feeder_session_validation() {
        let standalone = make_service(ServerMode::Standalone);
        let resp = feeder_session(
            HeaderMap::new(),
            State(standalone),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let sm = make_service(ServerMode::SourceManaged);
        let instance_id = sm.instance_id().to_string();

        // Missing header
        let resp = feeder_session(
            HeaderMap::new(),
            State(sm.clone()),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Header mismatch
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_static("srv-wrong"),
        );
        let resp = feeder_session(
            headers.clone(),
            State(sm.clone()),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // Body mismatch
        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(&instance_id).unwrap(),
        );
        let body_mismatch = SessionRequest {
            instance_id: "srv-wrong".into(),
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            lease_duration_ms: 1000,
        };
        let resp = feeder_session(
            valid_headers.clone(),
            State(sm.clone()),
            Ok(SimdJson(body_mismatch)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_feeder_heartbeat_validation() {
        let standalone = make_service(ServerMode::Standalone);
        let resp = feeder_heartbeat(
            HeaderMap::new(),
            State(standalone),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let sm = make_service(ServerMode::SourceManaged);
        let instance_id = sm.instance_id().to_string();

        let resp = feeder_heartbeat(
            HeaderMap::new(),
            State(sm.clone()),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(&instance_id).unwrap(),
        );
        let body_mismatch = HeartbeatRequest {
            instance_id: "srv-wrong".into(),
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
        };
        let resp = feeder_heartbeat(
            valid_headers,
            State(sm.clone()),
            Ok(SimdJson(body_mismatch)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_feeder_events_validation() {
        let standalone = make_service(ServerMode::Standalone);
        let resp = feeder_events(
            HeaderMap::new(),
            State(standalone),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let sm = make_service(ServerMode::SourceManaged);
        let instance_id = sm.instance_id().to_string();

        let resp = feeder_events(
            HeaderMap::new(),
            State(sm.clone()),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(&instance_id).unwrap(),
        );
        let body_mismatch = FeederEventsRequest {
            instance_id: "srv-wrong".into(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            source_id: "pg-1".into(),
            source_epoch: "ep-1".into(),
            events: vec![],
        };
        let resp = feeder_events(
            valid_headers,
            State(sm.clone()),
            Ok(SimdJson(body_mismatch)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_feeder_gap_validation() {
        let standalone = make_service(ServerMode::Standalone);
        let resp = feeder_gap(
            HeaderMap::new(),
            State(standalone),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let sm = make_service(ServerMode::SourceManaged);
        let instance_id = sm.instance_id().to_string();

        let resp = feeder_gap(
            HeaderMap::new(),
            State(sm.clone()),
            Err(StatusCode::BAD_REQUEST.into_response()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(&instance_id).unwrap(),
        );
        let body_mismatch = GapRequest {
            instance_id: "srv-wrong".into(),
            graph_id: "default".into(),
            generation: 1,
            feeder_session_id: "f-1".into(),
            feeder_epoch: 1,
            failed_delivery_sequence: 1,
            reason: Some("test".into()),
        };
        let resp = feeder_gap(
            valid_headers,
            State(sm.clone()),
            Ok(SimdJson(body_mismatch)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_source_managed_mutation_rejection() {
        let sm = make_service(ServerMode::SourceManaged);
        let op_link1 = Operation::Link {
            source: WireNode(1),
            target: WireNode(2),
        };
        let op_link2 = Operation::Link {
            source: WireNode(1),
            target: WireNode(2),
        };
        let resp = single(State(sm.clone()), Ok(SimdJson(op_link1))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let batch_req = Batch {
            operations: vec![op_link2],
        };
        let resp_b = batch(State(sm.clone()), Ok(SimdJson(batch_req))).await;
        assert_eq!(resp_b.status(), StatusCode::CONFLICT);

        let empty_batch = Batch { operations: vec![] };
        let resp_empty = batch(State(sm.clone()), Ok(SimdJson(empty_batch))).await;
        assert_eq!(resp_empty.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn test_feeder_error_status_mapping() {
        use crate::service::FeederError;
        let cases = [
            (
                FeederError::InvalidInstanceId("test".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                FeederError::StaleInstanceId("test".into()),
                StatusCode::CONFLICT,
            ),
            (
                FeederError::InvalidGraphId("test".into()),
                StatusCode::BAD_REQUEST,
            ),
            (
                FeederError::StaleGeneration("test".into()),
                StatusCode::CONFLICT,
            ),
            (FeederError::FencedSession, StatusCode::CONFLICT),
            (FeederError::LeaseExpired, StatusCode::CONFLICT),
            (FeederError::GraphAlreadyInitialized, StatusCode::CONFLICT),
            (FeederError::GraphUninitialized, StatusCode::CONFLICT),
            (FeederError::GraphDesynchronized, StatusCode::CONFLICT),
            (
                FeederError::PreconditionFailed("test".into()),
                StatusCode::PRECONDITION_FAILED,
            ),
            (
                FeederError::SourceMismatch("test".into()),
                StatusCode::CONFLICT,
            ),
            (
                FeederError::SourceEpochMismatch("test".into()),
                StatusCode::CONFLICT,
            ),
            (
                FeederError::OutOfOrderSequence {
                    expected: 1,
                    received: 2,
                },
                StatusCode::CONFLICT,
            ),
            (
                FeederError::ConflictingDuplicate("test".into()),
                StatusCode::CONFLICT,
            ),
            (
                FeederError::UnverifiableDuplicate("test".into()),
                StatusCode::CONFLICT,
            ),
            (FeederError::SequenceExhausted, StatusCode::CONFLICT),
            (FeederError::GenerationExhausted, StatusCode::CONFLICT),
            (
                FeederError::InvalidRequest("test".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (FeederError::Unavailable, StatusCode::SERVICE_UNAVAILABLE),
            (
                FeederError::ImportInProgress,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (FeederError::SessionRequired, StatusCode::CONFLICT),
            (FeederError::ModeMismatch, StatusCode::CONFLICT),
        ];
        for (err, expected_status) in cases {
            let resp = feeder_error(err);
            assert_eq!(resp.status(), expected_status);
        }
    }
}
