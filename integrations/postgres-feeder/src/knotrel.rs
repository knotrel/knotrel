//! HTTP client for the Knotrel feeder routes.
//!
//! The client only transports requests. Interpreting answers (retry, rebuild,
//! stop) is the feeder's job; see the design document, section 8.1.

use crate::mapping::GraphOp;
use serde_json::{Value, json};
use std::time::Duration;

/// A Knotrel answer with an HTTP status.
#[derive(Clone, Debug)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// The `code` member of an error body, if any.
    pub code: Option<String>,
    /// The JSON body (`Null` if empty or not JSON).
    pub body: Value,
    /// The `ETag` header, if any.
    pub etag: Option<String>,
}

impl Reply {
    /// Whether the status is 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether the error code equals `code`.
    pub fn is(&self, code: &str) -> bool {
        self.code.as_deref() == Some(code)
    }

    /// A string member of the body.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.body.get(key).and_then(Value::as_str)
    }

    /// An unsigned integer member of the body.
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.body.get(key).and_then(Value::as_u64)
    }

    /// A short description for logs and errors.
    pub fn describe(&self) -> String {
        let message = self.str("message").unwrap_or("");
        match &self.code {
            Some(code) => format!("HTTP {} {code}: {message}", self.status),
            None => format!("HTTP {}", self.status),
        }
    }
}

/// A request whose outcome is unknown: the connection failed or timed out
/// before an answer arrived.
#[derive(Debug)]
pub struct Uncertain(pub String);

impl std::fmt::Display for Uncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Knotrel request outcome unknown: {}", self.0)
    }
}

/// One feeder event with its continuity envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Delivery sequence (`D`).
    pub sequence: u64,
    /// Event identifier.
    pub event_id: String,
    /// Source position of this event.
    pub position: String,
    /// Source position of the predecessor.
    pub prev_position: String,
    /// The graph operation.
    pub op: GraphOp,
}

impl Event {
    /// The event as a JSON object for `POST /v1/feeder/events`.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("event_id".into(), json!(self.event_id));
        m.insert("delivery_sequence".into(), json!(self.sequence));
        m.insert("prev_delivery_sequence".into(), json!(self.sequence - 1));
        m.insert("source_position".into(), json!(self.position));
        m.insert("prev_source_position".into(), json!(self.prev_position));
        self.op.json_fields(&mut m);
        Value::Object(m)
    }
}

/// The feeder's identity towards one Knotrel instance.
#[derive(Clone, Debug)]
pub struct Session {
    /// Knotrel `instance_id` the session belongs to.
    pub instance_id: String,
    /// `feeder_session_id`.
    pub session_id: String,
    /// `feeder_epoch`.
    pub epoch: u64,
}

/// HTTP client for one Knotrel server.
#[derive(Clone, Debug)]
pub struct Knotrel {
    base: String,
    http: reqwest::Client,
    import_timeout: Duration,
}

impl Knotrel {
    /// A client for `base` (for example `http://knotrel:8080`).
    ///
    /// # Errors
    ///
    /// If the HTTP client cannot be built.
    pub fn new(base: &str, timeout: Duration, import_timeout: Duration) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("cannot build HTTP client: {e}"))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
            import_timeout,
        })
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Reply, Uncertain> {
        let response = request
            .send()
            .await
            .map_err(|e| Uncertain(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Uncertain(e.without_url().to_string()))?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = body.get("code").and_then(Value::as_str).map(str::to_string);
        Ok(Reply {
            status,
            code,
            body,
            etag,
        })
    }

    fn post(&self, path: &str, instance_id: &str, body: &Value) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base))
            .header("X-Knotrel-Instance-Id", instance_id)
            .json(body)
    }

    /// `GET /v1/info`.
    pub async fn info(&self) -> Result<Reply, Uncertain> {
        self.send(self.http.get(format!("{}/v1/info", self.base)))
            .await
    }

    /// `GET /v1/stats` (its `ETag` is the replacement precondition).
    pub async fn stats(&self) -> Result<Reply, Uncertain> {
        self.send(self.http.get(format!("{}/v1/stats", self.base)))
            .await
    }

    /// `POST /v1/feeder/session`: registers or renews the session; the answer
    /// reports Knotrel's applied frontier.
    pub async fn session(&self, s: &Session, lease_ms: u64) -> Result<Reply, Uncertain> {
        let body = json!({
            "instance_id": s.instance_id,
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
            "lease_duration_ms": lease_ms,
        });
        self.send(self.post("/v1/feeder/session", &s.instance_id, &body))
            .await
    }

    /// `POST /v1/feeder/heartbeat`.
    pub async fn heartbeat(&self, s: &Session) -> Result<Reply, Uncertain> {
        let body = json!({
            "instance_id": s.instance_id,
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
        });
        self.send(self.post("/v1/feeder/heartbeat", &s.instance_id, &body))
            .await
    }

    /// `POST /v1/feeder/import` with a complete image.
    #[allow(clippy::too_many_arguments)]
    pub async fn import(
        &self,
        s: &Session,
        replace_etag: Option<&str>,
        source_id: &str,
        source_epoch: &str,
        position: &str,
        nodes: &[(u64, bool)],
        edges: &[(u64, u64, bool)],
    ) -> Result<Reply, Uncertain> {
        let nodes: Vec<Value> = nodes
            .iter()
            .map(|&(id, active)| {
                if active {
                    json!(id.to_string())
                } else {
                    json!({"node": id.to_string(), "active": false})
                }
            })
            .collect();
        let edges: Vec<Value> = edges
            .iter()
            .map(|&(s, t, enabled)| {
                if enabled {
                    json!({"source": s.to_string(), "target": t.to_string()})
                } else {
                    json!({"source": s.to_string(), "target": t.to_string(), "enabled": false})
                }
            })
            .collect();
        let body = json!({
            "instance_id": s.instance_id,
            "graph_id": "default",
            "mode": if replace_etag.is_some() { "replace" } else { "create_only" },
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
            "source_id": source_id,
            "source_epoch": source_epoch,
            "source_position": position,
            "nodes": nodes,
            "edges": edges,
        });
        let mut request = self
            .post("/v1/feeder/import", &s.instance_id, &body)
            .timeout(self.import_timeout);
        if let Some(etag) = replace_etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.send(request).await
    }

    /// `POST /v1/feeder/events`.
    pub async fn events(
        &self,
        s: &Session,
        generation: u64,
        source_id: &str,
        source_epoch: &str,
        events: &[Event],
    ) -> Result<Reply, Uncertain> {
        let body = json!({
            "instance_id": s.instance_id,
            "graph_id": "default",
            "generation": generation,
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
            "source_id": source_id,
            "source_epoch": source_epoch,
            "events": events.iter().map(Event::to_json).collect::<Vec<_>>(),
        });
        self.send(self.post("/v1/feeder/events", &s.instance_id, &body))
            .await
    }

    /// `POST /v1/feeder/barrier` at the given frontier.
    pub async fn barrier(
        &self,
        s: &Session,
        generation: u64,
        sequence: u64,
        position: &str,
    ) -> Result<Reply, Uncertain> {
        let body = json!({
            "instance_id": s.instance_id,
            "graph_id": "default",
            "generation": generation,
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
            "barrier_delivery_sequence": sequence,
            "barrier_source_position": position,
        });
        self.send(self.post("/v1/feeder/barrier", &s.instance_id, &body))
            .await
    }

    /// `POST /v1/feeder/gap`: declares that source history after `sequence`
    /// is lost.
    pub async fn gap(
        &self,
        s: &Session,
        generation: u64,
        failed_sequence: u64,
        reason: &str,
    ) -> Result<Reply, Uncertain> {
        let body = json!({
            "instance_id": s.instance_id,
            "graph_id": "default",
            "generation": generation,
            "feeder_session_id": s.session_id,
            "feeder_epoch": s.epoch,
            "failed_delivery_sequence": failed_sequence,
            "reason": reason,
        });
        self.send(self.post("/v1/feeder/gap", &s.instance_id, &body))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_has_the_continuity_envelope() {
        let e = Event {
            sequence: 1,
            event_id: "0/10.1".into(),
            position: "p1".into(),
            prev_position: "p0".into(),
            op: GraphOp::Link(1, 2),
        };
        assert_eq!(
            e.to_json(),
            json!({
                "event_id": "0/10.1",
                "delivery_sequence": 1,
                "prev_delivery_sequence": 0,
                "source_position": "p1",
                "prev_source_position": "p0",
                "op": "link",
                "source": "1",
                "target": "2"
            })
        );
    }
}
