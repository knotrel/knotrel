//! Wire types and HTTP handlers for the versioned graph API.

use crate::service::{Service, ServiceError};
use axum::{
    Json,
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::{HeaderMap, HeaderName, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use knotrel_core::{ConnectivityGraph, GraphError};
use serde::{Deserialize, Serialize};

/// Registered HTTP route templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Health,
    Ready,
    Metrics,
    Info,
    Stats,
    Operations,
    Batch,
    FeederSession,
    FeederHeartbeat,
    FeederImport,
    FeederEvents,
    FeederGap,
    FeederBarrier,
}

impl Route {
    pub(crate) const ALL: [Self; 13] = [
        Self::Health,
        Self::Ready,
        Self::Metrics,
        Self::Info,
        Self::Stats,
        Self::Operations,
        Self::Batch,
        Self::FeederSession,
        Self::FeederHeartbeat,
        Self::FeederImport,
        Self::FeederEvents,
        Self::FeederGap,
        Self::FeederBarrier,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Health => "/health",
            Self::Ready => "/ready",
            Self::Metrics => "/metrics",
            Self::Info => "/v1/info",
            Self::Stats => "/v1/stats",
            Self::Operations => "/v1/operations",
            Self::Batch => "/v1/batch",
            Self::FeederSession => "/v1/feeder/session",
            Self::FeederHeartbeat => "/v1/feeder/heartbeat",
            Self::FeederImport => "/v1/feeder/import",
            Self::FeederEvents => "/v1/feeder/events",
            Self::FeederGap => "/v1/feeder/gap",
            Self::FeederBarrier => "/v1/feeder/barrier",
        }
    }
}

pub(crate) const INSTANCE_ID_HEADER: HeaderName = HeaderName::from_static("x-knotrel-instance-id");
pub(crate) const GRAPH_ID_HEADER: HeaderName = HeaderName::from_static("x-knotrel-graph-id");
pub(crate) const GENERATION_HEADER: HeaderName = HeaderName::from_static("x-knotrel-generation");
pub(crate) const STATE_VERSION_HEADER: HeaderName =
    HeaderName::from_static("x-knotrel-state-version");
pub(crate) const SOURCE_POSITION_HEADER: HeaderName =
    HeaderName::from_static("x-knotrel-source-position");
pub(crate) const SYNC_STATUS_HEADER: HeaderName = HeaderName::from_static("x-knotrel-sync-status");

pub(crate) const DEFAULT_GRAPH_ID: &str = "default";
pub(crate) const MIN_LEASE_DURATION_MS: u64 = 1_000;
pub(crate) const MAX_LEASE_DURATION_MS: u64 = 60_000;
pub(crate) const MAX_EVENT_ID_BYTES: usize = 64;
pub(crate) const MAX_SOURCE_POSITION_BYTES: usize = 256;

/// Stable error identifiers serialized by the HTTP API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    BarrierNotReached,
    BarrierPositionMismatch,
    Busy,
    ConflictingDuplicate,
    Desynchronized,
    DirectMutationDisabled,
    EdgeLimitExceeded,
    EventFailed,
    FencedFeederSession,
    GenerationExhausted,
    GraphAlreadyInitialized,
    GraphDesynchronized,
    GraphUninitialized,
    ImportInProgress,
    InvalidBatchSize,
    InvalidEdge,
    InvalidGraphId,
    InvalidInstanceId,
    InvalidLeaseDuration,
    InvalidNode,
    InvalidRequest,
    LeaseExpired,
    Loading,
    ModeConflict,
    NodeLimitExceeded,
    OutOfOrderSequence,
    PayloadTooLarge,
    PreconditionFailed,
    RequestTimeout,
    SelfLoop,
    SequenceExhausted,
    SessionRequired,
    ShuttingDown,
    SourceEpochMismatch,
    SourceMismatch,
    SourcePredecessorMismatch,
    StaleGeneration,
    StaleInstanceId,
    SupersededBarrier,
    Unavailable,
    Uninitialized,
    UnknownNode,
    UnverifiableDuplicate,
}

impl ErrorCode {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 43] = [
        Self::BarrierNotReached,
        Self::BarrierPositionMismatch,
        Self::Busy,
        Self::ConflictingDuplicate,
        Self::Desynchronized,
        Self::DirectMutationDisabled,
        Self::EdgeLimitExceeded,
        Self::EventFailed,
        Self::FencedFeederSession,
        Self::GenerationExhausted,
        Self::GraphAlreadyInitialized,
        Self::GraphDesynchronized,
        Self::GraphUninitialized,
        Self::ImportInProgress,
        Self::InvalidBatchSize,
        Self::InvalidEdge,
        Self::InvalidGraphId,
        Self::InvalidInstanceId,
        Self::InvalidLeaseDuration,
        Self::InvalidNode,
        Self::InvalidRequest,
        Self::LeaseExpired,
        Self::Loading,
        Self::ModeConflict,
        Self::NodeLimitExceeded,
        Self::OutOfOrderSequence,
        Self::PayloadTooLarge,
        Self::PreconditionFailed,
        Self::RequestTimeout,
        Self::SelfLoop,
        Self::SequenceExhausted,
        Self::SessionRequired,
        Self::ShuttingDown,
        Self::SourceEpochMismatch,
        Self::SourceMismatch,
        Self::SourcePredecessorMismatch,
        Self::StaleGeneration,
        Self::StaleInstanceId,
        Self::SupersededBarrier,
        Self::Unavailable,
        Self::Uninitialized,
        Self::UnknownNode,
        Self::UnverifiableDuplicate,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BarrierNotReached => "barrier_not_reached",
            Self::BarrierPositionMismatch => "barrier_position_mismatch",
            Self::Busy => "busy",
            Self::ConflictingDuplicate => "conflicting_duplicate",
            Self::Desynchronized => "desynchronized",
            Self::DirectMutationDisabled => "direct_mutation_disabled",
            Self::EdgeLimitExceeded => "edge_limit_exceeded",
            Self::EventFailed => "event_failed",
            Self::FencedFeederSession => "fenced_feeder_session",
            Self::GenerationExhausted => "generation_exhausted",
            Self::GraphAlreadyInitialized => "graph_already_initialized",
            Self::GraphDesynchronized => "graph_desynchronized",
            Self::GraphUninitialized => "graph_uninitialized",
            Self::ImportInProgress => "import_in_progress",
            Self::InvalidBatchSize => "invalid_batch_size",
            Self::InvalidEdge => "invalid_edge",
            Self::InvalidGraphId => "invalid_graph_id",
            Self::InvalidInstanceId => "invalid_instance_id",
            Self::InvalidLeaseDuration => "invalid_lease_duration",
            Self::InvalidNode => "invalid_node",
            Self::InvalidRequest => "invalid_request",
            Self::LeaseExpired => "lease_expired",
            Self::Loading => "loading",
            Self::ModeConflict => "mode_conflict",
            Self::NodeLimitExceeded => "node_limit_exceeded",
            Self::OutOfOrderSequence => "out_of_order_sequence",
            Self::PayloadTooLarge => "payload_too_large",
            Self::PreconditionFailed => "precondition_failed",
            Self::RequestTimeout => "request_timeout",
            Self::SelfLoop => "self_loop",
            Self::SequenceExhausted => "sequence_exhausted",
            Self::SessionRequired => "session_required",
            Self::ShuttingDown => "shutting_down",
            Self::SourceEpochMismatch => "source_epoch_mismatch",
            Self::SourceMismatch => "source_mismatch",
            Self::SourcePredecessorMismatch => "source_predecessor_mismatch",
            Self::StaleGeneration => "stale_generation",
            Self::StaleInstanceId => "stale_instance_id",
            Self::SupersededBarrier => "superseded_barrier",
            Self::Unavailable => "unavailable",
            Self::Uninitialized => "uninitialized",
            Self::UnknownNode => "unknown_node",
            Self::UnverifiableDuplicate => "unverifiable_duplicate",
        }
    }
}

/// A decimal-string node identifier with lossless JSON representation.
///
/// Deserialization parses the borrowed JSON string directly, so no `String`
/// is allocated per identifier; imports can carry millions of them. Accepted
/// input and every rejection message match the former `try_from = "String"`
/// implementation (see `wire_node_json_contract_is_preserved`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct WireNode(pub(crate) u64);

impl WireNode {
    /// Parses a nonempty string of ASCII decimal digits in O(len) time.
    pub(crate) fn parse(value: &str) -> Result<Self, &'static str> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("node IDs must be nonempty strings of ASCII decimal digits");
        }
        value.parse().map(Self).map_err(|_| "node ID exceeds u64")
    }
}

impl<'de> Deserialize<'de> for WireNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WireNodeVisitor;

        impl serde::de::Visitor<'_> for WireNodeVisitor {
            type Value = WireNode;

            // Same expectation as `String`'s visitor, so type errors read alike.
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<WireNode, E> {
                WireNode::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(WireNodeVisitor)
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

/// Stable result values returned by successful feeder operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeederResult {
    Published,
    SessionAcquired,
    HeartbeatAcknowledged,
    Applied,
    Duplicate,
    PartialFailure,
    GapAcknowledged,
    AlreadySynchronized,
    Synchronized,
}

impl FeederResult {
    #[cfg(test)]
    const ALL: [Self; 9] = [
        Self::Published,
        Self::SessionAcquired,
        Self::HeartbeatAcknowledged,
        Self::Applied,
        Self::Duplicate,
        Self::PartialFailure,
        Self::GapAcknowledged,
        Self::AlreadySynchronized,
        Self::Synchronized,
    ];
}

/// Status returned for an exactly duplicated feeder event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DuplicateStatus {
    AlreadyApplied,
}

/// Recovery action requested after a declared source gap.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequiredAction {
    FullRebuildRequired,
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
    pub(crate) result: FeederResult,
    pub(crate) instance_id: String,
    pub(crate) graph_id: String,
    pub(crate) generation: u64,
    pub(crate) state_version: String,
    pub(crate) node_count: usize,
    pub(crate) edge_count: usize,
    pub(crate) source_position: String,
    pub(crate) status: crate::service::SyncStatus,
}

/// Request payload for feeder session registration.
#[derive(Clone, Debug, Deserialize)]
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
    pub(crate) result: FeederResult,
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
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeartbeatRequest {
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
}

/// Response payload for successful feeder heartbeat.
#[derive(Debug, Serialize)]
pub(crate) struct HeartbeatResponse {
    pub(crate) result: FeederResult,
    pub(crate) instance_id: String,
    pub(crate) feeder_session_id: String,
    pub(crate) feeder_epoch: u64,
    pub(crate) lease_expires_in_ms: u64,
    pub(crate) status: crate::service::SyncStatus,
}

/// A feeder event operation descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FeederOperation {
    Link,
    Cut,
    AddNode,
    Progress,
}

impl FeederOperation {
    #[cfg(test)]
    const ALL: [Self; 4] = [Self::Link, Self::Cut, Self::AddNode, Self::Progress];

    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Cut => "cut",
            Self::AddNode => "add_node",
            Self::Progress => "progress",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "link" => Self::Link,
            "cut" => Self::Cut,
            "add_node" => Self::AddNode,
            "progress" => Self::Progress,
            _ => return None,
        })
    }
}

/// The `op` member of a feeder event, resolved while parsing.
///
/// Known names are stored as their variant, so a batch of up to
/// `MAX_BATCH_OPERATIONS` events allocates no string per operation. Unknown
/// names keep their text for the `unsupported operation` error, which is
/// still raised during event validation, not by the parser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WireOp {
    Known(FeederOperation),
    Unknown(String),
}

impl From<&str> for WireOp {
    fn from(value: &str) -> Self {
        FeederOperation::parse(value).map_or_else(|| Self::Unknown(value.to_owned()), Self::Known)
    }
}

impl<'de> Deserialize<'de> for WireOp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WireOpVisitor;

        impl serde::de::Visitor<'_> for WireOpVisitor {
            type Value = WireOp;

            // Same expectation as `String`'s visitor, so type errors read alike.
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<WireOp, E> {
                Ok(WireOp::from(value))
            }
        }

        deserializer.deserialize_str(WireOpVisitor)
    }
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
    pub(crate) op: WireOp,
    #[serde(default)]
    pub(crate) source: Option<WireNode>,
    #[serde(default)]
    pub(crate) target: Option<WireNode>,
    #[serde(default)]
    pub(crate) node: Option<WireNode>,
}

/// Request payload for ordered feeder incremental events.
#[derive(Clone, Debug, Deserialize)]
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
    pub(crate) result: FeederResult,
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
    pub(crate) result: FeederResult,
    pub(crate) status: DuplicateStatus,
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
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
}

/// Response payload for a feeder batch stopped on error.
#[derive(Debug, Serialize)]
pub(crate) struct PartialBatchFailure {
    pub(crate) result: FeederResult,
    pub(crate) code: ErrorCode,
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
#[derive(Clone, Debug, Deserialize)]
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
    pub(crate) result: FeederResult,
    pub(crate) instance_id: String,
    pub(crate) generation: u64,
    pub(crate) status: crate::service::SyncStatus,
    pub(crate) action_required: RequiredAction,
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
        code: ErrorCode,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_sequence: Option<u64>,
    },
}

impl From<GraphError> for Outcome {
    fn from(error: GraphError) -> Self {
        let code = match error {
            GraphError::NodeLimitExceeded { .. } => ErrorCode::NodeLimitExceeded,
            GraphError::EdgeLimitExceeded { .. } => ErrorCode::EdgeLimitExceeded,
            GraphError::SelfLoop { .. } => ErrorCode::SelfLoop,
            GraphError::UnknownNode { .. } => ErrorCode::UnknownNode,
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
                code: ErrorCode::UnknownNode,
                ..
            } => StatusCode::NOT_FOUND,
            Self::Error {
                code: ErrorCode::Desynchronized | ErrorCode::Uninitialized | ErrorCode::Loading,
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
                error_response(err.status(), ErrorCode::InvalidRequest, err.body_text())
            })?;
        // simd-json parses in place; reuse a uniquely owned body allocation
        // instead of always copying it.
        let mut buffer = Vec::from(bytes);
        let value = simd_json::from_slice(&mut buffer).map_err(|err| {
            let status = if err.is_data() {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::BAD_REQUEST
            };
            tracing::debug!(%err, is_data = err.is_data(), "failed to parse request JSON with simd-json");
            error_response(status, ErrorCode::InvalidRequest, request_error_message(&err))
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
            ErrorCode::InvalidRequest,
            "Expected request with `Content-Type: application/json`".into(),
        ))
    }
}

/// Traces incoming HTTP requests inside a per-request span (M6.3).
///
/// The `request` span carries a process-unique `request_id`, the method and
/// the route (a registered path, or `unmatched`). Blocking workers enter it, so
/// their log lines share the `request_id`. Handlers record `generation`,
/// `state_version` and `sync_status` on it (see [`inject_metadata_headers`]).
/// On completion one event records status and server-side latency: `info`
/// for API requests, `trace` for probes and metrics scrapes. A request future
/// dropped before a response exists logs `request cancelled before response`
/// instead, so every request leaves exactly one completion or cancellation
/// line. Unregistered paths are logged sanitized (escaped, truncated).
pub(crate) async fn logging_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use tracing::Instrument;

    let method = request.method().clone();
    let path = request.uri().path();
    let route = Route::ALL
        .into_iter()
        .map(Route::as_str)
        .find(|route| *route == path);
    // Unknown paths are copied (the request is moved into the handler) and
    // only then sanitized, so raw caller text never reaches the log.
    let unmatched: Option<String> = match route {
        Some(_) => None,
        None => Some(crate::logging::sanitize(path).to_string()),
    };
    let probe = route == Some(Route::Health.as_str())
        || route == Some(Route::Ready.as_str())
        || crate::lifecycle::is_metrics_scrape(&request);
    let route_label = route.unwrap_or("unmatched");
    let request_id = crate::logging::next_request_id();
    let span = if probe {
        tracing::trace_span!(
            crate::logging::REQUEST_SPAN,
            request_id,
            method = %method,
            route = route_label,
            generation = tracing::field::Empty,
            state_version = tracing::field::Empty,
            sync_status = tracing::field::Empty,
        )
    } else {
        tracing::info_span!(
            crate::logging::REQUEST_SPAN,
            request_id,
            method = %method,
            route = route_label,
            generation = tracing::field::Empty,
            state_version = tracing::field::Empty,
            sync_status = tracing::field::Empty,
        )
    };

    /// Logs a cancellation if dropped before the response was produced.
    struct CancelLog {
        span: tracing::Span,
        probe: bool,
        start: std::time::Instant,
        armed: bool,
    }
    impl Drop for CancelLog {
        fn drop(&mut self) {
            if self.armed && !self.probe {
                let _entered = self.span.enter();
                tracing::info!(
                    latency_us = self.start.elapsed().as_micros(),
                    "request cancelled before response"
                );
            }
        }
    }

    let start = std::time::Instant::now();
    let mut cancel = CancelLog {
        span: span.clone(),
        probe,
        start,
        armed: true,
    };
    let response = next.run(request).instrument(span.clone()).await;
    cancel.armed = false;

    let latency = start.elapsed();
    let status = response.status().as_u16();
    let uri = unmatched.as_deref().unwrap_or(route_label);
    let _entered = span.enter();
    if probe {
        tracing::trace!(
            method = %method,
            uri,
            status,
            latency_us = latency.as_micros(),
            "probe"
        );
    } else {
        tracing::info!(
            method = %method,
            uri,
            status,
            latency_us = latency.as_micros(),
            "handled request"
        );
    }
    drop(_entered);

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
            ErrorCode::DirectMutationDisabled,
            "direct graph mutations are disabled in source-managed mode; use feeder endpoints"
                .into(),
        );
    }
    tracing::debug!("executing single operation");
    match service.execute_operation(operation).await {
        Ok(mut exec_result) => {
            let mut response = exec_result.outcomes.remove(0).into_response();
            inject_metadata_headers(response.headers_mut(), exec_result.metadata);
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
                ErrorCode::DirectMutationDisabled,
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
            ErrorCode::InvalidBatchSize,
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
            inject_metadata_headers(response.headers_mut(), exec_result.metadata);
            response
        }
        Err(error) => service_error(error),
    }
}

fn check_feeder_headers(service: &Service, headers: &HeaderMap) -> Option<Response> {
    if service.mode() != crate::ServerMode::SourceManaged {
        return Some(error_response(
            StatusCode::CONFLICT,
            ErrorCode::ModeConflict,
            "feeder endpoints are only available in source_managed mode".into(),
        ));
    }
    let Some(instance_id_header) = headers.get(&INSTANCE_ID_HEADER) else {
        return Some(feeder_error(
            crate::service::FeederError::InvalidInstanceId(
                "missing header X-Knotrel-Instance-Id".into(),
            ),
        ));
    };
    let Ok(header_val) = instance_id_header.to_str() else {
        return Some(feeder_error(
            crate::service::FeederError::InvalidInstanceId(
                "invalid header X-Knotrel-Instance-Id".into(),
            ),
        ));
    };
    if header_val != service.instance_id() {
        return Some(error_response(
            StatusCode::CONFLICT,
            ErrorCode::StaleInstanceId,
            "header X-Knotrel-Instance-Id must match server instance ID".into(),
        ));
    }
    None
}

fn check_body_instance_id(service: &Service, body_instance_id: &str) -> Option<Response> {
    if body_instance_id != service.instance_id() {
        return Some(error_response(
            StatusCode::CONFLICT,
            ErrorCode::StaleInstanceId,
            "body instance_id must match server instance ID".into(),
        ));
    }
    None
}

fn check_default_graph_id(graph_id: &str) -> Option<Response> {
    if graph_id != DEFAULT_GRAPH_ID {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidGraphId,
            format!("unsupported graph ID '{graph_id}'; only '{DEFAULT_GRAPH_ID}' is supported"),
        ));
    }
    None
}

fn check_service_available(service: &Service) -> Option<Response> {
    if service.is_unavailable() {
        service.count_rejection(crate::metrics::Rejection::Unavailable);
        return Some(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "graph worker unavailable".into(),
        ));
    }
    None
}

/// Registers an active feeder session.
pub(crate) async fn feeder_session(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<SessionRequest>, Response>,
) -> Response {
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    let SimdJson(session_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Some(rejection) = check_body_instance_id(&service, &session_req.instance_id) {
        return rejection;
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
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    let SimdJson(heartbeat_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Some(rejection) = check_body_instance_id(&service, &heartbeat_req.instance_id) {
        return rejection;
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
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    if let Some(rejection) = check_json_content_type(&headers) {
        return rejection;
    }
    if let Some(rejection) = check_service_available(&service) {
        return rejection;
    }
    let if_match = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // 1. Acquire the single owned import permit BEFORE consuming body bytes!
    let mut permit = match service.acquire_import_permit() {
        Ok(permit) => permit,
        Err(err) => return feeder_error(err),
    };

    // 2. Read body bytes with bounded limit under acquired permit
    let max_bytes = service.max_import_body_bytes();
    let read = axum::body::to_bytes(body, max_bytes).await;
    permit.received(read.is_ok());
    let bytes = match read {
        Ok(bytes) => bytes,
        Err(_) => return feeder_error(crate::service::FeederError::PayloadTooLarge),
    };

    // 3. Dispatch to service.import, transferring owned permit and raw bytes
    match service.import(permit, bytes, if_match).await {
        Ok(res) => {
            let metadata = crate::service::ResponseMetadata {
                instance_id: res.instance_id.clone(),
                graph_id: DEFAULT_GRAPH_ID,
                generation: res.generation,
                state_version: res.state_version.clone(),
                source_position: Some(res.source_position.clone()),
                sync_status: Some(crate::service::SyncStatus::CatchingUp),
            };
            let mut response = (StatusCode::CREATED, Json(res)).into_response();
            inject_metadata_headers(response.headers_mut(), metadata);
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
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    let SimdJson(events_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Some(rejection) = check_body_instance_id(&service, &events_req.instance_id) {
        return rejection;
    }
    if let Some(rejection) = check_default_graph_id(&events_req.graph_id) {
        return rejection;
    }
    if let Some(rejection) = check_service_available(&service) {
        return rejection;
    }
    match service.apply_events(events_req).await {
        Ok(crate::service::FeederEventsResult::Applied { response, metadata }) => {
            let mut res = (StatusCode::OK, Json(response)).into_response();
            inject_metadata_headers(res.headers_mut(), metadata);
            res
        }
        Ok(crate::service::FeederEventsResult::Duplicate { response, metadata }) => {
            let mut res = (StatusCode::OK, Json(response)).into_response();
            inject_metadata_headers(res.headers_mut(), metadata);
            res
        }
        Ok(crate::service::FeederEventsResult::PartialFailure {
            failure,
            metadata,
            status,
        }) => {
            let mut res = (status, Json(failure)).into_response();
            inject_metadata_headers(res.headers_mut(), metadata);
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
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    let SimdJson(gap_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Some(rejection) = check_body_instance_id(&service, &gap_req.instance_id) {
        return rejection;
    }
    if let Some(rejection) = check_default_graph_id(&gap_req.graph_id) {
        return rejection;
    }
    if let Some(rejection) = check_service_available(&service) {
        return rejection;
    }
    match service.declare_gap(gap_req).await {
        Ok((res, metadata)) => {
            let mut response = (StatusCode::OK, Json(res)).into_response();
            inject_metadata_headers(response.headers_mut(), metadata);
            response
        }
        Err(err) => feeder_error(err),
    }
}

/// Verifies a synchronization barrier and promotes the published graph to Ready.
pub(crate) async fn feeder_barrier(
    headers: HeaderMap,
    State(service): State<Service>,
    request: Result<SimdJson<crate::service::BarrierRequest>, Response>,
) -> Response {
    if let Some(rejection) = check_feeder_headers(&service, &headers) {
        return rejection;
    }
    let SimdJson(barrier_req) = match request {
        Ok(req) => req,
        Err(response) => return response,
    };
    if let Some(rejection) = check_body_instance_id(&service, &barrier_req.instance_id) {
        return rejection;
    }
    if let Some(rejection) = check_default_graph_id(&barrier_req.graph_id) {
        return rejection;
    }
    if let Some(rejection) = check_service_available(&service) {
        return rejection;
    }
    match service.declare_barrier(barrier_req).await {
        Ok((res, metadata)) => {
            let mut response = (StatusCode::OK, Json(res)).into_response();
            inject_metadata_headers(response.headers_mut(), metadata);
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
            ErrorCode::ImportInProgress,
            "import capacity exhausted; another import is in progress".into(),
        ),
        FeederError::Unavailable => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "graph worker unavailable".into(),
        ),
        FeederError::ModeMismatch => error_response(
            StatusCode::CONFLICT,
            ErrorCode::ModeConflict,
            "feeder endpoints are only available in source_managed mode".into(),
        ),
        FeederError::StaleInstanceId(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::StaleInstanceId, msg)
        }
        FeederError::InvalidInstanceId(msg) => {
            error_response(StatusCode::BAD_REQUEST, ErrorCode::InvalidInstanceId, msg)
        }
        FeederError::InvalidGraphId(msg) => {
            error_response(StatusCode::BAD_REQUEST, ErrorCode::InvalidGraphId, msg)
        }
        FeederError::SessionRequired => error_response(
            StatusCode::CONFLICT,
            ErrorCode::SessionRequired,
            "active feeder session required; register via POST /v1/feeder/session".into(),
        ),
        FeederError::FencedSession => error_response(
            StatusCode::CONFLICT,
            ErrorCode::FencedFeederSession,
            "feeder session or epoch is stale/fenced".into(),
        ),
        FeederError::LeaseExpired => error_response(
            StatusCode::CONFLICT,
            ErrorCode::LeaseExpired,
            "feeder lease expired; renew lease or register new session".into(),
        ),
        FeederError::GraphAlreadyInitialized => error_response(
            StatusCode::CONFLICT,
            ErrorCode::GraphAlreadyInitialized,
            "create_only requires uninitialized graph (generation 0)".into(),
        ),
        FeederError::GraphUninitialized => error_response(
            StatusCode::CONFLICT,
            ErrorCode::GraphUninitialized,
            "graph is uninitialized; baseline import required".into(),
        ),
        FeederError::GraphDesynchronized => error_response(
            StatusCode::CONFLICT,
            ErrorCode::GraphDesynchronized,
            "graph is desynchronized; full rebuild required".into(),
        ),
        FeederError::GenerationExhausted => error_response(
            StatusCode::CONFLICT,
            ErrorCode::GenerationExhausted,
            "generation counter exhausted; server restart required".into(),
        ),
        FeederError::StaleGeneration(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::StaleGeneration, msg)
        }
        FeederError::SourceMismatch(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::SourceMismatch, msg)
        }
        FeederError::SourceEpochMismatch(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::SourceEpochMismatch, msg)
        }
        FeederError::OutOfOrderSequence { expected, received } => (
            StatusCode::CONFLICT,
            Json(Outcome::Error {
                code: ErrorCode::OutOfOrderSequence,
                message: format!(
                    "expected delivery sequence {}, received {}",
                    expected, received
                ),
                expected_sequence: Some(expected),
            }),
        )
            .into_response(),
        FeederError::SourcePredecessorMismatch(msg) => error_response(
            StatusCode::CONFLICT,
            ErrorCode::SourcePredecessorMismatch,
            msg,
        ),
        FeederError::ConflictingDuplicate(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::ConflictingDuplicate, msg)
        }
        FeederError::UnverifiableDuplicate(msg) => {
            error_response(StatusCode::CONFLICT, ErrorCode::UnverifiableDuplicate, msg)
        }
        FeederError::SequenceExhausted => error_response(
            StatusCode::CONFLICT,
            ErrorCode::SequenceExhausted,
            "delivery sequence counter exhausted; server restart required".into(),
        ),
        FeederError::PreconditionFailed(msg) => error_response(
            StatusCode::PRECONDITION_FAILED,
            ErrorCode::PreconditionFailed,
            msg,
        ),
        FeederError::BarrierPositionMismatch { expected, received } => error_response(
            StatusCode::CONFLICT,
            ErrorCode::BarrierPositionMismatch,
            format!(
                "barrier source position '{}' does not match applied position '{}'",
                received, expected
            ),
        ),
        FeederError::BarrierNotReached { expected, received } => error_response(
            StatusCode::CONFLICT,
            ErrorCode::BarrierNotReached,
            format!(
                "barrier delivery sequence {} has not been reached; currently at {}",
                received, expected
            ),
        ),
        FeederError::SupersededBarrier { expected, received } => error_response(
            StatusCode::CONFLICT,
            ErrorCode::SupersededBarrier,
            format!(
                "barrier delivery sequence {} is superseded by applied delivery sequence {}",
                received, expected
            ),
        ),
        FeederError::InvalidLeaseDuration => error_response(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidLeaseDuration,
            format!(
                "lease_duration_ms must be between {MIN_LEASE_DURATION_MS} and {MAX_LEASE_DURATION_MS}"
            ),
        ),
        FeederError::MalformedJson => error_response(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "request body is not valid JSON".into(),
        ),
        FeederError::UnprocessableJson(msg) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::InvalidRequest,
            msg,
        ),
        FeederError::PayloadTooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            "import payload exceeds size limit".into(),
        ),
        FeederError::Domain(code, msg) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, code, msg)
        }
        FeederError::InvalidRequest(msg) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::InvalidRequest,
            msg,
        ),
    }
}

fn service_error(error: ServiceError) -> Response {
    let (code, message) = match error {
        ServiceError::Busy => (ErrorCode::Busy, "request capacity exhausted; retry later"),
        ServiceError::Unavailable => {
            tracing::error!("graph worker unavailable while handling request");
            (ErrorCode::Unavailable, "graph worker unavailable")
        }
    };
    error_response(StatusCode::SERVICE_UNAVAILABLE, code, message.into())
}

pub(crate) fn error_response(status: StatusCode, code: ErrorCode, message: String) -> Response {
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
            inject_metadata_headers(res.headers_mut(), stats_result.metadata);
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
    metadata: crate::service::ResponseMetadata,
) {
    use axum::http::HeaderValue;
    // Correlation fields for the request's log lines (M6.3); recording on a
    // disabled span does nothing and allocates nothing.
    let span = tracing::Span::current();
    if !span.is_disabled() {
        span.record("generation", metadata.generation);
        span.record("state_version", metadata.state_version.as_str());
        if let Some(status) = metadata.sync_status {
            span.record("sync_status", status.as_str());
        }
    }
    // Owned strings become header values without another copy, and the
    // source position is percent-encoded once for both the ETag and its header.
    let encoded_position = metadata
        .source_position
        .as_deref()
        .map(crate::service::percent_encode_position);
    let etag = metadata.etag_with_encoded_position(encoded_position.as_deref().unwrap_or(""));
    if let Ok(val) = HeaderValue::try_from(etag) {
        headers.insert(axum::http::header::ETAG, val);
    }
    if let Ok(val) = HeaderValue::try_from(metadata.instance_id) {
        headers.insert(INSTANCE_ID_HEADER, val);
    }
    headers.insert(GRAPH_ID_HEADER, HeaderValue::from_static(DEFAULT_GRAPH_ID));
    headers.insert(GENERATION_HEADER, HeaderValue::from(metadata.generation));
    if let Ok(val) = HeaderValue::try_from(metadata.state_version) {
        headers.insert(STATE_VERSION_HEADER, val);
    }
    if let Some(encoded) = encoded_position
        && let Ok(val) = HeaderValue::try_from(encoded)
    {
        headers.insert(SOURCE_POSITION_HEADER, val);
    }
    if let Some(status) = metadata.sync_status {
        headers.insert(
            SYNC_STATUS_HEADER,
            HeaderValue::from_static(status.as_str()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Lifecycle, ServerConfig, ServerMode};
    use axum::http::HeaderValue;

    #[test]
    fn typed_wire_vocabulary_matches_the_http_contract() {
        assert_eq!(
            Route::ALL.map(Route::as_str),
            [
                "/health",
                "/ready",
                "/metrics",
                "/v1/info",
                "/v1/stats",
                "/v1/operations",
                "/v1/batch",
                "/v1/feeder/session",
                "/v1/feeder/heartbeat",
                "/v1/feeder/import",
                "/v1/feeder/events",
                "/v1/feeder/gap",
                "/v1/feeder/barrier",
            ]
        );
        assert_eq!(INSTANCE_ID_HEADER.as_str(), "x-knotrel-instance-id");
        assert_eq!(GRAPH_ID_HEADER.as_str(), "x-knotrel-graph-id");
        assert_eq!(GENERATION_HEADER.as_str(), "x-knotrel-generation");
        assert_eq!(STATE_VERSION_HEADER.as_str(), "x-knotrel-state-version");
        assert_eq!(SOURCE_POSITION_HEADER.as_str(), "x-knotrel-source-position");
        assert_eq!(SYNC_STATUS_HEADER.as_str(), "x-knotrel-sync-status");
        assert_eq!(DEFAULT_GRAPH_ID, "default");
        assert_eq!(MIN_LEASE_DURATION_MS, 1_000);
        assert_eq!(MAX_LEASE_DURATION_MS, 60_000);
        assert_eq!(MAX_EVENT_ID_BYTES, 64);
        assert_eq!(MAX_SOURCE_POSITION_BYTES, 256);

        assert_eq!(
            FeederOperation::ALL.map(FeederOperation::as_str),
            ["link", "cut", "add_node", "progress"]
        );
        for operation in FeederOperation::ALL {
            assert_eq!(FeederOperation::parse(operation.as_str()), Some(operation));
        }
        assert_eq!(FeederOperation::parse("unknown"), None);
        assert_eq!(
            FeederResult::ALL.map(|result| serde_json::to_string(&result).unwrap()),
            [
                "\"published\"",
                "\"session_acquired\"",
                "\"heartbeat_acknowledged\"",
                "\"applied\"",
                "\"duplicate\"",
                "\"partial_failure\"",
                "\"gap_acknowledged\"",
                "\"already_synchronized\"",
                "\"synchronized\"",
            ]
        );
        assert_eq!(
            serde_json::to_string(&DuplicateStatus::AlreadyApplied).unwrap(),
            "\"already_applied\""
        );
        assert_eq!(
            serde_json::to_string(&RequiredAction::FullRebuildRequired).unwrap(),
            "\"full_rebuild_required\""
        );

        assert_eq!(
            ErrorCode::ALL.map(ErrorCode::as_str),
            [
                "barrier_not_reached",
                "barrier_position_mismatch",
                "busy",
                "conflicting_duplicate",
                "desynchronized",
                "direct_mutation_disabled",
                "edge_limit_exceeded",
                "event_failed",
                "fenced_feeder_session",
                "generation_exhausted",
                "graph_already_initialized",
                "graph_desynchronized",
                "graph_uninitialized",
                "import_in_progress",
                "invalid_batch_size",
                "invalid_edge",
                "invalid_graph_id",
                "invalid_instance_id",
                "invalid_lease_duration",
                "invalid_node",
                "invalid_request",
                "lease_expired",
                "loading",
                "mode_conflict",
                "node_limit_exceeded",
                "out_of_order_sequence",
                "payload_too_large",
                "precondition_failed",
                "request_timeout",
                "self_loop",
                "sequence_exhausted",
                "session_required",
                "shutting_down",
                "source_epoch_mismatch",
                "source_mismatch",
                "source_predecessor_mismatch",
                "stale_generation",
                "stale_instance_id",
                "superseded_barrier",
                "unavailable",
                "uninitialized",
                "unknown_node",
                "unverifiable_duplicate",
            ]
        );
    }

    fn make_service(mode: ServerMode) -> Service {
        let config = ServerConfig::default().with_mode(mode);
        Service::with_config(ConnectivityGraph::default(), 2, Lifecycle::new(), &config)
    }

    #[test]
    fn test_wire_node_parsing() {
        assert_eq!(WireNode::parse("123").unwrap().0, 123);
        assert_eq!(WireNode::parse("0").unwrap().0, 0);
        assert!(WireNode::parse("").is_err());
        assert!(WireNode::parse("abc").is_err());
        assert!(WireNode::parse("-1").is_err());
        assert!(WireNode::parse("18446744073709551616").is_err());
    }

    /// Pins the node-ID wire contract through the real JSON parser: accepted
    /// forms, the client-visible message of every rejection, and its status.
    #[test]
    fn wire_node_json_contract_is_preserved() {
        fn parse(body: &str) -> Result<Operation, (bool, String)> {
            let mut bytes = body.as_bytes().to_vec();
            simd_json::from_slice::<Operation>(&mut bytes)
                .map_err(|err| (err.is_data(), request_error_message(&err)))
        }
        let connected = |source: &str, target: &str| {
            format!(r#"{{"op":"connected","source":{source},"target":{target}}}"#)
        };
        for (source, expected) in [
            (r#""0""#, 0),
            (r#""42""#, 42),
            (r#""18446744073709551615""#, u64::MAX),
            (r#""007""#, 7),
            (r#""\u0031\u0032""#, 12),
        ] {
            match parse(&connected(source, r#""1""#)) {
                Ok(Operation::Connected { source, target }) => {
                    assert_eq!((source.0, target.0), (expected, 1));
                }
                other => panic!("{source}: {other:?}"),
            }
        }
        let digits = "node IDs must be nonempty strings of ASCII decimal digits";
        let shape = "request body does not match the expected shape";
        for (source, message) in [
            (r#""""#, digits),
            (r#""abc""#, digits),
            (r#""-1""#, digits),
            (r#""+1""#, digits),
            (r#"" 1""#, digits),
            (r#""1.0""#, digits),
            (r#""\u0661""#, digits),
            (r#""18446744073709551616""#, shape),
            ("1", shape),
            ("-1", shape),
            ("1.5", shape),
            ("null", shape),
            ("true", shape),
            ("[]", shape),
            ("{}", shape),
        ] {
            assert_eq!(
                parse(&connected(source, r#""1""#)).unwrap_err(),
                (true, message.to_owned()),
                "{source}"
            );
        }
        assert_eq!(
            parse(r#"{"op":"connected","source":"1""#).unwrap_err(),
            (false, "request body is not valid JSON".to_owned())
        );
    }

    /// Known feeder operations resolve while parsing; unknown names survive
    /// for validation's message; non-strings remain shape errors.
    #[test]
    fn feeder_event_op_json_contract_is_preserved() {
        fn parse(op: &str) -> Result<FeederEvent, (bool, String)> {
            let mut bytes = format!(
                r#"{{"event_id":"e","delivery_sequence":1,"prev_delivery_sequence":0,"source_position":"p","prev_source_position":"","op":{op}}}"#
            )
            .into_bytes();
            simd_json::from_slice::<FeederEvent>(&mut bytes)
                .map_err(|err| (err.is_data(), request_error_message(&err)))
        }
        for operation in FeederOperation::ALL {
            let event = parse(&format!(r#""{}""#, operation.as_str())).unwrap();
            assert_eq!(event.op, WireOp::Known(operation));
        }
        assert_eq!(
            parse(r#""LINK""#).unwrap().op,
            WireOp::Unknown("LINK".into())
        );
        assert_eq!(parse(r#""""#).unwrap().op, WireOp::Unknown(String::new()));
        for op in ["1", "null", "[]"] {
            assert_eq!(
                parse(op).unwrap_err(),
                (
                    true,
                    "request body does not match the expected shape".to_owned()
                ),
                "{op}"
            );
        }
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
        inject_metadata_headers(&mut headers, metadata);
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
            (FeederError::InvalidLeaseDuration, StatusCode::BAD_REQUEST),
            (FeederError::MalformedJson, StatusCode::BAD_REQUEST),
            (
                FeederError::UnprocessableJson("err".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (FeederError::PayloadTooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            (
                FeederError::Domain(ErrorCode::InvalidEdge, "err_msg".into()),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                FeederError::SupersededBarrier {
                    expected: 1,
                    received: 0,
                },
                StatusCode::CONFLICT,
            ),
            (
                FeederError::SourcePredecessorMismatch("err".into()),
                StatusCode::CONFLICT,
            ),
        ];
        for (err, expected_status) in cases {
            let resp = feeder_error(err);
            assert_eq!(resp.status(), expected_status);
        }
    }

    /// Every counted feeder rejection is labelled with the wire code its
    /// response carries, except domain errors, which collapse to
    /// `event_failed`; admission, adapter-only and import-only errors are
    /// not counted.
    #[tokio::test]
    async fn feeder_rejection_labels_match_wire_codes() {
        use crate::metrics::FeederRejection;
        use crate::service::FeederError;
        let s = || "x".to_owned();
        let cases = [
            (FeederError::ImportInProgress, None),
            (FeederError::Unavailable, None),
            (FeederError::ModeMismatch, None),
            (FeederError::StaleInstanceId(s()), None),
            (FeederError::InvalidInstanceId(s()), None),
            (FeederError::InvalidGraphId(s()), None),
            (FeederError::SessionRequired, Some("session_required")),
            (FeederError::FencedSession, Some("fenced_feeder_session")),
            (FeederError::LeaseExpired, Some("lease_expired")),
            (FeederError::GraphAlreadyInitialized, None),
            (FeederError::GraphUninitialized, Some("graph_uninitialized")),
            (
                FeederError::GraphDesynchronized,
                Some("graph_desynchronized"),
            ),
            (FeederError::GenerationExhausted, None),
            (FeederError::StaleGeneration(s()), Some("stale_generation")),
            (FeederError::SourceMismatch(s()), Some("source_mismatch")),
            (
                FeederError::SourceEpochMismatch(s()),
                Some("source_epoch_mismatch"),
            ),
            (
                FeederError::OutOfOrderSequence {
                    expected: 1,
                    received: 2,
                },
                Some("out_of_order_sequence"),
            ),
            (
                FeederError::SourcePredecessorMismatch(s()),
                Some("source_predecessor_mismatch"),
            ),
            (
                FeederError::ConflictingDuplicate(s()),
                Some("conflicting_duplicate"),
            ),
            (
                FeederError::UnverifiableDuplicate(s()),
                Some("unverifiable_duplicate"),
            ),
            (FeederError::SequenceExhausted, Some("sequence_exhausted")),
            (FeederError::PreconditionFailed(s()), None),
            (
                FeederError::InvalidLeaseDuration,
                Some("invalid_lease_duration"),
            ),
            (FeederError::MalformedJson, None),
            (FeederError::UnprocessableJson(s()), None),
            (FeederError::PayloadTooLarge, None),
            (
                FeederError::Domain(ErrorCode::SelfLoop, s()),
                Some("event_failed"),
            ),
            (FeederError::InvalidRequest(s()), Some("invalid_request")),
            (
                FeederError::BarrierPositionMismatch {
                    expected: s(),
                    received: s(),
                },
                Some("barrier_position_mismatch"),
            ),
            (
                FeederError::BarrierNotReached {
                    expected: 1,
                    received: 2,
                },
                Some("barrier_not_reached"),
            ),
            (
                FeederError::SupersededBarrier {
                    expected: 1,
                    received: 0,
                },
                Some("superseded_barrier"),
            ),
        ];
        let mut labels = std::collections::BTreeSet::new();
        for (err, expected) in cases {
            let label = FeederRejection::of(&err).map(FeederRejection::label);
            assert_eq!(label, expected, "{err:?}");
            let is_domain = matches!(err, FeederError::Domain(..));
            let response = feeder_error(err);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if let Some(label) = label {
                let wire = if is_domain {
                    "event_failed"
                } else {
                    body["code"].as_str().unwrap()
                };
                assert_eq!(wire, label);
                labels.insert(label);
            }
        }
        let all: std::collections::BTreeSet<_> =
            FeederRejection::ALL.iter().map(|r| r.label()).collect();
        assert_eq!(labels, all);
    }

    #[tokio::test]
    async fn test_feeder_checks_and_batch_handling() {
        use axum::extract::State;
        let service = make_service(ServerMode::SourceManaged);

        // 1. Invalid UTF-8 in X-Knotrel-Instance-Id header
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        let resp = check_feeder_headers(&service, &headers).unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 2. Non-default graph_id
        let resp = check_default_graph_id("custom_graph").unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(check_default_graph_id("default").is_none());

        // 3. check_service_available
        assert!(check_service_available(&service).is_none());
        service.mark_unavailable_for_test();
        let resp = check_service_available(&service).unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 4. Batch size bounds: empty and oversized
        let standalone_service = make_service(ServerMode::Standalone);
        let empty_batch = Batch { operations: vec![] };
        let resp = batch(State(standalone_service.clone()), Ok(SimdJson(empty_batch))).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let mut ops = Vec::new();
        for _ in 0..crate::MAX_BATCH_OPERATIONS + 1 {
            ops.push(Operation::Connected {
                source: WireNode(1),
                target: WireNode(2),
            });
        }
        let oversized_batch = Batch { operations: ops };
        let resp = batch(State(standalone_service), Ok(SimdJson(oversized_batch))).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let err_resp = error_response(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "bad".into(),
        );
        let resp = batch(State(make_service(ServerMode::Standalone)), Err(err_resp)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 5. service_error mappings
        assert_eq!(
            service_error(ServiceError::Busy).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            service_error(ServiceError::Unavailable).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        // 6. Batch error path when service is unavailable
        let unavailable_service = make_service(ServerMode::Standalone);
        unavailable_service.mark_unavailable_for_test();
        let valid_batch = Batch {
            operations: vec![Operation::Connected {
                source: WireNode(1),
                target: WireNode(2),
            }],
        };
        let resp = batch(State(unavailable_service), Ok(SimdJson(valid_batch))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 7. SimdJson error propagation in feeder handlers
        let make_err = || {
            error_response(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "syntax error".into(),
            )
        };
        let headers = HeaderMap::new();

        let resp = feeder_session(headers.clone(), State(service.clone()), Err(make_err())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = feeder_heartbeat(headers.clone(), State(service.clone()), Err(make_err())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = feeder_events(headers.clone(), State(service.clone()), Err(make_err())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = feeder_gap(headers.clone(), State(service.clone()), Err(make_err())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = feeder_barrier(headers.clone(), State(service.clone()), Err(make_err())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 8. Graph ID and instance ID validation in feeder handlers
        let mut valid_headers = HeaderMap::new();
        valid_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(service.instance_id()).unwrap(),
        );

        let make_events = |graph: &str| FeederEventsRequest {
            instance_id: service.instance_id().to_string(),
            graph_id: graph.into(),
            generation: 1,
            source_id: "s".into(),
            source_epoch: "1".into(),
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            events: vec![],
        };
        let resp = feeder_events(
            valid_headers.clone(),
            State(service.clone()),
            Ok(SimdJson(make_events("invalid"))),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let make_gap = |graph: &str| GapRequest {
            instance_id: service.instance_id().to_string(),
            graph_id: graph.into(),
            generation: 1,
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            failed_delivery_sequence: 1,
            reason: None,
        };
        let resp = feeder_gap(
            valid_headers.clone(),
            State(service.clone()),
            Ok(SimdJson(make_gap("invalid"))),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let make_barrier = |graph: &str, inst: &str| crate::service::BarrierRequest {
            instance_id: inst.into(),
            graph_id: graph.into(),
            generation: 1,
            feeder_session_id: "s1".into(),
            feeder_epoch: 1,
            barrier_delivery_sequence: 1,
            barrier_source_position: Some("0".into()),
        };
        let resp = feeder_barrier(
            valid_headers.clone(),
            State(service.clone()),
            Ok(SimdJson(make_barrier("invalid", service.instance_id()))),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = feeder_barrier(
            valid_headers.clone(),
            State(service.clone()),
            Ok(SimdJson(make_barrier("default", "wrong_inst"))),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // 9. Unavailable service check in feeder handlers
        let unavail = make_service(ServerMode::SourceManaged);
        unavail.mark_unavailable_for_test();

        let mut unavail_headers = HeaderMap::new();
        unavail_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(unavail.instance_id()).unwrap(),
        );

        let mut unavail_import_hdrs = unavail_headers.clone();
        unavail_import_hdrs.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let resp = feeder_import(
            State(unavail.clone()),
            unavail_import_hdrs,
            axum::body::Body::from("{}"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut unavail_events = make_events("default");
        unavail_events.instance_id = unavail.instance_id().to_string();
        let resp = feeder_events(
            unavail_headers.clone(),
            State(unavail.clone()),
            Ok(SimdJson(unavail_events)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut unavail_gap = make_gap("default");
        unavail_gap.instance_id = unavail.instance_id().to_string();
        let resp = feeder_gap(
            unavail_headers.clone(),
            State(unavail.clone()),
            Ok(SimdJson(unavail_gap)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let resp = feeder_barrier(
            unavail_headers,
            State(unavail.clone()),
            Ok(SimdJson(make_barrier("default", unavail.instance_id()))),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 10. feeder_gap error propagation
        let gap_service = make_service(ServerMode::SourceManaged);
        let mut gap_headers = HeaderMap::new();
        gap_headers.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(gap_service.instance_id()).unwrap(),
        );
        let mut gap_req = make_gap("default");
        gap_req.instance_id = gap_service.instance_id().to_string();
        let resp = feeder_gap(gap_headers, State(gap_service), Ok(SimdJson(gap_req))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // 11. feeder_import payload too large
        let small_limit_service = Service::with_config(
            knotrel_core::ConnectivityGraph::default(),
            2,
            Lifecycle::ready(),
            &crate::ServerConfig::default()
                .with_mode(ServerMode::SourceManaged)
                .with_max_import_body_bytes(10)
                .unwrap(),
        );
        let mut hdrs2 = HeaderMap::new();
        hdrs2.insert(
            "x-knotrel-instance-id",
            HeaderValue::from_str(small_limit_service.instance_id()).unwrap(),
        );
        hdrs2.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let resp = feeder_import(
            State(small_limit_service),
            hdrs2,
            axum::body::Body::from(vec![b'x'; 20]),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
