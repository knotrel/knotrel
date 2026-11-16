//! HTTP, feeder and import tests for node and edge lifecycle support (M10).
//!
//! Covers the request validation order, opt-in rejection, revision accounting,
//! state queries, source-managed authority, flagged imports, lifecycle feeder
//! events with duplicate fingerprints, and lifecycle telemetry.

#![cfg(feature = "test-support")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use knotrel_core::EngineConfig;
use knotrel_server::{ServerConfig, ServerMode, router_with_config};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Engines with lifecycle support in v0.5.0; `ett` and `hdt` lifecycle support
/// is deferred and rejected at configuration time.
const ENGINES: [EngineConfig; 1] = [EngineConfig::CompactBfs];

fn config(engine: EngineConfig, lifecycle: bool, mode: ServerMode) -> ServerConfig {
    ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 8)
        .unwrap()
        .with_mode(mode)
        .with_topology_lifecycle(lifecycle)
        .unwrap()
        .with_metrics_enabled(true)
}

struct Reply {
    status: StatusCode,
    body: Value,
    raw: String,
    version: Option<String>,
    etag: Option<String>,
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    instance: Option<&str>,
    body: &str,
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(instance) = instance {
        request = request.header("x-knotrel-instance-id", instance);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let version = response
        .headers()
        .get("x-knotrel-state-version")
        .map(|value| value.to_str().unwrap().to_owned());
    let etag = response
        .headers()
        .get(header::ETAG)
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 4 << 20).await.unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    let body = serde_json::from_str(&raw).unwrap_or(Value::Null);
    Reply {
        status,
        body,
        raw,
        version,
        etag,
    }
}

async fn op(app: &Router, operation: Value) -> Reply {
    send(
        app,
        Method::POST,
        "/v1/operations",
        None,
        &operation.to_string(),
    )
    .await
}

async fn batch(app: &Router, operations: Value) -> Reply {
    send(
        app,
        Method::POST,
        "/v1/batch",
        None,
        &json!({ "operations": operations }).to_string(),
    )
    .await
}

async fn get(app: &Router, path: &str) -> Value {
    send(app, Method::GET, path, None, "").await.body
}

fn connected(source: &str, target: &str) -> Value {
    json!({"op":"connected","source":source,"target":target})
}

const LIFECYCLE_OPS: [&str; 5] = [
    r#"{"op":"set_node_active","node":"1","active":false}"#,
    r#"{"op":"set_edge_enabled","source":"1","target":"2","enabled":false}"#,
    r#"{"op":"remove_node","node":"1"}"#,
    r#"{"op":"node_state","node":"1"}"#,
    r#"{"op":"edge_state","source":"1","target":"2"}"#,
];

#[tokio::test]
async fn disabled_lifecycle_rejects_every_new_operation_before_execution() {
    let app = router_with_config(config(
        EngineConfig::CompactBfs,
        false,
        ServerMode::Standalone,
    ));
    assert!(
        get(&app, "/v1/info")
            .await
            .get("topology_lifecycle")
            .is_none()
    );
    assert_eq!(
        op(&app, json!({"op":"link","source":"1","target":"2"}))
            .await
            .status,
        StatusCode::OK
    );
    for operation in LIFECYCLE_OPS {
        let reply = send(&app, Method::POST, "/v1/operations", None, operation).await;
        assert_eq!(reply.status, StatusCode::CONFLICT, "{operation}");
        assert_eq!(reply.body["code"], "topology_lifecycle_disabled");
        // The whole batch is refused, including the legacy operations in it.
        let body =
            format!(r#"{{"operations":[{{"op":"cut","source":"1","target":"2"}},{operation}]}}"#);
        let reply = send(&app, Method::POST, "/v1/batch", None, &body).await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.body["code"], "topology_lifecycle_disabled");
    }
    // Nothing ran: the edge is intact and the revision is unchanged.
    let stats = get(&app, "/v1/stats").await;
    assert_eq!(stats["edge_count"], 1);
    assert_eq!(stats["state_version"], "1");
    assert!(stats.get("active_node_count").is_none());
    // A malformed operation fails the shape check (422) before step 5a.
    let body = format!(
        r#"{{"operations":[{{"op":"link","source":"1"}},{}]}}"#,
        LIFECYCLE_OPS[0]
    );
    assert_eq!(
        send(&app, Method::POST, "/v1/batch", None, &body)
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Legacy operations never accept lifecycle flags.
    let reply = op(
        &app,
        json!({"op":"link","source":"1","target":"3","enabled":true}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply.body["code"], "invalid_request");
}

#[tokio::test]
async fn standalone_lifecycle_operations_follow_the_contract_on_every_engine() {
    for engine in ENGINES {
        let app = router_with_config(config(engine, true, ServerMode::Standalone));
        assert_eq!(get(&app, "/v1/info").await["topology_lifecycle"], true);
        // Hub 202 between 201 and 203.
        for (s, t) in [("201", "202"), ("202", "203")] {
            assert_eq!(
                op(&app, json!({"op":"link","source":s,"target":t}))
                    .await
                    .version
                    .as_deref(),
                Some(if s == "201" { "1" } else { "2" })
            );
        }
        let deactivate = op(
            &app,
            json!({"op":"set_node_active","node":"202","active":false}),
        )
        .await;
        assert_eq!(deactivate.body, json!({"result":"changed","changed":true}));
        assert_eq!(
            deactivate.version.as_deref(),
            Some("3"),
            "{engine:?}: one increment"
        );
        assert_eq!(
            op(&app, connected("201", "203")).await.body["connected"],
            false
        );
        assert_eq!(
            op(&app, connected("202", "202")).await.body["connected"],
            false
        );
        // Repeated setter: no-op, no revision increment.
        let again = op(
            &app,
            json!({"op":"set_node_active","node":"202","active":false}),
        )
        .await;
        assert_eq!(again.body["changed"], false);
        assert_eq!(again.version.as_deref(), Some("3"));
        // Disable one incident edge while 202 is inactive: revision advances
        // although connectivity does not change.
        let disable = op(
            &app,
            json!({"op":"set_edge_enabled","source":"203","target":"202","enabled":false}),
        )
        .await;
        assert_eq!(disable.body["changed"], true);
        assert_eq!(disable.version.as_deref(), Some("4"));
        assert_eq!(
            op(
                &app,
                json!({"op":"edge_state","source":"202","target":"203"})
            )
            .await
            .body,
            json!({"result":"edge_state","registered":true,"enabled":false,"effective":false})
        );
        // Reactivate: only the still-enabled edge returns.
        op(
            &app,
            json!({"op":"set_node_active","node":"202","active":true}),
        )
        .await;
        assert_eq!(
            op(&app, connected("201", "202")).await.body["connected"],
            true
        );
        assert_eq!(
            op(&app, connected("201", "203")).await.body["connected"],
            false
        );
        assert_eq!(
            op(&app, json!({"op":"node_state","node":"202"})).await.body,
            json!({"result":"node_state","registered":true,"active":true})
        );
        let stats = get(&app, "/v1/stats").await;
        assert_eq!(
            (
                &stats["node_count"],
                &stats["edge_count"],
                &stats["active_node_count"],
                &stats["enabled_edge_count"],
                &stats["effective_edge_count"]
            ),
            (&json!(3), &json!(2), &json!(3), &json!(1), &json!(1))
        );
        // Remove and recreate: old edges do not return.
        let removal = op(&app, json!({"op":"remove_node","node":"202"})).await;
        assert_eq!(removal.body["changed"], true);
        assert_eq!(
            removal.version.as_deref(),
            Some("6"),
            "cascade is one increment"
        );
        assert_eq!(
            op(&app, json!({"op":"remove_node","node":"202"}))
                .await
                .body["changed"],
            false
        );
        assert_eq!(
            op(&app, connected("201", "202")).await.status,
            StatusCode::NOT_FOUND
        );
        op(&app, json!({"op":"add_node","node":"202"})).await;
        assert_eq!(
            op(&app, connected("201", "202")).await.body["connected"],
            false
        );
        assert_eq!(
            op(
                &app,
                json!({"op":"edge_state","source":"201","target":"202"})
            )
            .await
            .body,
            json!({"result":"edge_state","registered":false,"enabled":false,"effective":false})
        );
        assert_eq!(
            op(&app, json!({"op":"node_state","node":"999"})).await.body,
            json!({"result":"node_state","registered":false,"active":false})
        );
    }
}

#[tokio::test]
async fn lifecycle_errors_map_to_documented_codes_and_batches_continue() {
    let app = router_with_config(config(
        EngineConfig::CompactBfs,
        true,
        ServerMode::Standalone,
    ));
    op(&app, json!({"op":"link","source":"1","target":"2"})).await;
    op(&app, json!({"op":"add_node","node":"3"})).await;
    let cases = [
        (
            json!({"op":"set_node_active","node":"9","active":true}),
            404,
            "unknown_node",
        ),
        (
            json!({"op":"set_edge_enabled","source":"1","target":"1","enabled":true}),
            422,
            "self_loop",
        ),
        (
            json!({"op":"set_edge_enabled","source":"8","target":"9","enabled":true}),
            404,
            "unknown_node",
        ),
        (
            json!({"op":"set_edge_enabled","source":"1","target":"3","enabled":true}),
            404,
            "unknown_edge",
        ),
        (
            json!({"op":"edge_state","source":"4","target":"4"}),
            422,
            "self_loop",
        ),
        (
            json!({"op":"set_node_active","node":"1"}),
            422,
            "invalid_request",
        ),
    ];
    for (operation, status, code) in cases {
        let reply = op(&app, operation.clone()).await;
        assert_eq!(reply.status.as_u16(), status, "{operation}");
        assert_eq!(reply.body["code"], code, "{operation}: {}", reply.raw);
    }
    let reply = batch(
        &app,
        json!([
            {"op":"set_edge_enabled","source":"1","target":"3","enabled":false},
            {"op":"set_node_active","node":"1","active":false},
            connected("1", "2"),
        ]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["results"][0]["code"], "unknown_edge");
    assert_eq!(reply.body["results"][1]["changed"], true);
    assert_eq!(reply.body["results"][2]["connected"], false);
    assert_eq!(reply.version.as_deref(), Some("3"));
}

#[tokio::test]
async fn limits_count_inactive_and_disabled_topology() {
    let config = config(EngineConfig::CompactBfs, true, ServerMode::Standalone).with_graph_limits(
        knotrel_core::GraphLimits {
            max_nodes: Some(2),
            max_edges: Some(1),
        },
    );
    let app = router_with_config(config);
    op(&app, json!({"op":"link","source":"1","target":"2"})).await;
    op(
        &app,
        json!({"op":"set_node_active","node":"2","active":false}),
    )
    .await;
    op(
        &app,
        json!({"op":"set_edge_enabled","source":"1","target":"2","enabled":false}),
    )
    .await;
    let reply = op(&app, json!({"op":"add_node","node":"3"})).await;
    assert_eq!(reply.body["code"], "node_limit_exceeded");
    let stats = get(&app, "/v1/stats").await;
    assert_eq!(stats["remaining_nodes"], 0);
    assert_eq!(stats["remaining_edges"], 0);
    op(&app, json!({"op":"remove_node","node":"2"})).await;
    assert_eq!(
        op(&app, json!({"op":"link","source":"1","target":"3"}))
            .await
            .body["changed"],
        true
    );
}

#[tokio::test]
async fn lifecycle_metrics_are_bounded_and_published_only_when_enabled() {
    let plain = router_with_config(config(
        EngineConfig::CompactBfs,
        false,
        ServerMode::Standalone,
    ));
    let exposition = send(&plain, Method::GET, "/metrics", None, "").await.raw;
    assert!(!exposition.contains("knotrel_graph_active_nodes"));
    assert!(!exposition.contains("set_node_active"));

    let app = router_with_config(config(
        EngineConfig::CompactBfs,
        true,
        ServerMode::Standalone,
    ));
    op(&app, json!({"op":"link","source":"1","target":"2"})).await;
    op(&app, json!({"op":"link","source":"2","target":"3"})).await;
    op(
        &app,
        json!({"op":"set_node_active","node":"2","active":false}),
    )
    .await;
    op(
        &app,
        json!({"op":"set_edge_enabled","source":"1","target":"2","enabled":false}),
    )
    .await;
    op(&app, json!({"op":"node_state","node":"7"})).await;
    let exposition = send(&app, Method::GET, "/metrics", None, "").await.raw;
    for line in [
        "knotrel_graph_nodes 3",
        "knotrel_graph_edges 2",
        "knotrel_graph_active_nodes 2",
        "knotrel_graph_enabled_edges 1",
        "knotrel_graph_effective_edges 0",
        r#"knotrel_graph_operations_total{op="set_node_active",outcome="changed"} 1"#,
        r#"knotrel_graph_operations_total{op="set_edge_enabled",outcome="changed"} 1"#,
        r#"knotrel_graph_operations_total{op="node_state",outcome="unregistered"} 1"#,
        r#"knotrel_graph_operations_total{op="remove_node",outcome="error"} 0"#,
    ] {
        assert!(exposition.lines().any(|l| l == line), "missing {line}");
    }
    // No identifier ever appears as a label value.
    for line in exposition
        .lines()
        .filter(|l| l.starts_with("knotrel_graph_operations_total"))
    {
        assert!(
            !line.contains("node=\"") && !line.contains("source=\""),
            "{line}"
        );
    }
}

// ---------------------------------------------------------------- source-managed

struct Feeder {
    app: Router,
    instance: String,
    sequence: u64,
    position: u64,
}

impl Feeder {
    async fn start(engine: EngineConfig, lifecycle: bool) -> Self {
        let app = router_with_config(config(engine, lifecycle, ServerMode::SourceManaged));
        let instance = get(&app, "/v1/info").await["instance_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let feeder = Self {
            app,
            instance,
            sequence: 0,
            position: 0,
        };
        let reply = feeder
            .post(
                "/v1/feeder/session",
                json!({"instance_id":feeder.instance,"feeder_session_id":"s","feeder_epoch":1,"lease_duration_ms":60000}),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw);
        feeder
    }

    async fn post(&self, path: &str, body: Value) -> Reply {
        send(
            &self.app,
            Method::POST,
            path,
            Some(&self.instance),
            &body.to_string(),
        )
        .await
    }

    async fn import(&mut self, nodes: Value, edges: Value) -> Reply {
        self.position = 100;
        self.post(
            "/v1/feeder/import",
            json!({
                "mode":"create_only","instance_id":self.instance,"graph_id":"default",
                "source_id":"src","source_epoch":"e1","source_position":"p100",
                "feeder_session_id":"s","feeder_epoch":1,"nodes":nodes,"edges":edges,
            }),
        )
        .await
    }

    /// Builds the next in-order event with the given operation fields.
    fn event(&mut self, id: &str, fields: Value) -> Value {
        let mut event = json!({
            "event_id": id,
            "delivery_sequence": self.sequence + 1,
            "prev_delivery_sequence": self.sequence,
            "source_position": format!("p{}", self.position + 1),
            "prev_source_position": format!("p{}", self.position),
        });
        for (key, value) in fields.as_object().unwrap() {
            event[key] = value.clone();
        }
        self.sequence += 1;
        self.position += 1;
        event
    }

    async fn events(&self, events: Vec<Value>) -> Reply {
        self.post(
            "/v1/feeder/events",
            json!({
                "instance_id":self.instance,"graph_id":"default","generation":1,
                "feeder_session_id":"s","feeder_epoch":1,"source_id":"src","source_epoch":"e1",
                "events":events,
            }),
        )
        .await
    }

    async fn query(&self, operation: Value) -> Reply {
        op(&self.app, operation).await
    }
}

#[tokio::test]
async fn flagged_import_publishes_complete_registered_topology() {
    for engine in ENGINES {
        let mut feeder = Feeder::start(engine, true).await;
        let reply = feeder
            .import(
                json!(["201", {"node":"202","active":false}, "203", "900", {"node":"901","active":true}]),
                json!([
                    {"source":"201","target":"202"},
                    {"source":"202","target":"203","enabled":false},
                    {"source":"201","target":"203","enabled":true},
                ]),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.raw);
        assert_eq!(reply.body["node_count"], 5);
        assert_eq!(reply.body["edge_count"], 3);
        assert_eq!(reply.body["active_node_count"], 4);
        assert_eq!(reply.body["enabled_edge_count"], 2);
        assert_eq!(reply.body["effective_edge_count"], 1);
        assert_eq!(
            feeder
                .query(json!({"op":"node_state","node":"202"}))
                .await
                .body["active"],
            false
        );
        assert_eq!(
            feeder
                .query(json!({"op":"edge_state","source":"203","target":"202"}))
                .await
                .body,
            json!({"result":"edge_state","registered":true,"enabled":false,"effective":false})
        );
        assert_eq!(
            feeder.query(connected("201", "203")).await.body["connected"],
            true
        );
        assert_eq!(
            feeder.query(connected("900", "900")).await.body["connected"],
            true
        );
        // Direct lifecycle mutations cannot bypass the feeder.
        let reply = feeder
            .query(json!({"op":"set_node_active","node":"202","active":true}))
            .await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.body["code"], "direct_mutation_disabled");
    }
}

#[tokio::test]
async fn flagged_import_rejects_conflicting_entries() {
    let mut feeder = Feeder::start(EngineConfig::CompactBfs, true).await;
    let reply = feeder
        .import(json!(["1", {"node":"1","active":false}]), json!([]))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply.body["code"], "invalid_node");
    let reply = feeder
        .import(
            json!([]),
            json!([{"source":"1","target":"2"},{"source":"2","target":"1","enabled":false}]),
        )
        .await;
    assert_eq!(reply.body["code"], "invalid_edge");
    let reply = feeder.import(json!([{"node":"1"}]), json!([])).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn disabled_lifecycle_rejects_flagged_imports_and_events_without_applying_any() {
    // Every engine, including the ones whose lifecycle support is deferred:
    // with lifecycle off they keep the same source-managed behavior.
    for engine in [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ] {
        disabled_lifecycle_rejects_flagged_requests(engine).await;
    }
}

async fn disabled_lifecycle_rejects_flagged_requests(engine: EngineConfig) {
    let mut feeder = Feeder::start(engine, false).await;
    for (nodes, edges) in [
        (json!([{"node":"1","active":true}]), json!([])),
        (
            json!(["1", "2"]),
            json!([{"source":"1","target":"2","enabled":true}]),
        ),
    ] {
        let reply = feeder.import(nodes, edges).await;
        assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw);
        assert_eq!(reply.body["code"], "topology_lifecycle_disabled");
    }
    assert_eq!(
        feeder.import(json!(["1", "2"]), json!([])).await.status,
        StatusCode::CREATED
    );
    let link = feeder.event("e1", json!({"op":"link","source":"1","target":"2"}));
    let lifecycle = feeder.event(
        "e2",
        json!({"op":"set_node_active","node":"1","active":false}),
    );
    let reply = feeder.events(vec![link, lifecycle]).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.raw);
    assert_eq!(reply.body["code"], "topology_lifecycle_disabled");
    let stats = get(&feeder.app, "/v1/stats").await;
    assert_eq!(
        stats["edge_count"], 0,
        "no event of the request was applied"
    );
    // A legacy event carrying a flag is invalid whatever the setting.
    feeder.sequence = 0;
    feeder.position = 100;
    let flagged = feeder.event(
        "e3",
        json!({"op":"link","source":"1","target":"2","enabled":true}),
    );
    let reply = feeder.events(vec![flagged]).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply.body["code"], "invalid_request");
}

#[tokio::test]
async fn lifecycle_events_apply_in_order_with_flag_aware_duplicate_detection() {
    for engine in ENGINES {
        let mut feeder = Feeder::start(engine, true).await;
        feeder
            .import(
                json!(["1", "2", "3"]),
                json!([{"source":"1","target":"2"},{"source":"2","target":"3"}]),
            )
            .await;
        let deactivate = feeder.event(
            "e1",
            json!({"op":"set_node_active","node":"2","active":false}),
        );
        let reply = feeder.events(vec![deactivate.clone()]).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw);
        assert_eq!(reply.body["state_version"], "1");
        assert_eq!(
            feeder.query(connected("1", "3")).await.body["connected"],
            false
        );

        // Exact duplicate: answered from the window, not applied again.
        let reply = feeder.events(vec![deactivate.clone()]).await;
        assert_eq!(reply.body["result"], "duplicate");
        assert_eq!(reply.body["current_state_version"], "1");

        // Same sequence and event ID, different flag: conflicting duplicate.
        let mut altered = deactivate.clone();
        altered["active"] = json!(true);
        let reply = feeder.events(vec![altered]).await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.body["code"], "conflicting_duplicate");
    }
}

#[tokio::test]
async fn removed_topology_is_not_resurrected_by_retries_and_errors_keep_422() {
    for engine in ENGINES {
        let mut feeder = Feeder::start(engine, true).await;
        feeder
            .import(
                json!(["1", "2", "3"]),
                json!([{"source":"1","target":"2"},{"source":"2","target":"3"}]),
            )
            .await;
        let disable = feeder.event(
            "e1",
            json!({"op":"set_edge_enabled","source":"2","target":"1","enabled":false}),
        );
        let remove = feeder.event("e2", json!({"op":"remove_node","node":"2"}));
        let re_add = feeder.event("e3", json!({"op":"add_node","node":"2"}));
        let reply = feeder.events(vec![disable.clone(), remove, re_add]).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw);
        assert_eq!(reply.body["events_applied"], 3);
        assert_eq!(reply.body["state_version"], "3");
        // Retrying the older lifecycle event is a duplicate, not a re-application.
        let reply = feeder.events(vec![disable]).await;
        assert_eq!(reply.body["result"], "duplicate");
        assert_eq!(
            feeder
                .query(json!({"op":"edge_state","source":"1","target":"2"}))
                .await
                .body["registered"],
            false
        );
        assert_eq!(
            feeder.query(connected("1", "2")).await.body["connected"],
            false
        );
        let stats = get(&feeder.app, "/v1/stats").await;
        assert_eq!(
            (stats["node_count"].clone(), stats["edge_count"].clone()),
            (json!(3), json!(0))
        );

        // Single-event domain error: 422 with the domain code, frontier unchanged.
        let unknown = feeder.event(
            "e4",
            json!({"op":"set_edge_enabled","source":"1","target":"3","enabled":true}),
        );
        let reply = feeder.events(vec![unknown]).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.raw
        );
        assert_eq!(reply.body["code"], "unknown_edge");
        // Multi-event: partial failure with event_failed, prefix applied.
        feeder.sequence -= 1;
        feeder.position -= 1;
        let ok = feeder.event("e4", json!({"op":"remove_node","node":"9"}));
        let bad = feeder.event(
            "e5",
            json!({"op":"set_node_active","node":"9","active":true}),
        );
        let reply = feeder.events(vec![ok, bad]).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(reply.body["result"], "partial_failure");
        assert_eq!(reply.body["code"], "event_failed");
        assert_eq!(reply.body["error"]["code"], "unknown_node");
        assert_eq!(reply.body["applied_delivery_sequence"], 4);
        assert_eq!(
            reply.body["state_version"], "3",
            "no-op removal does not advance"
        );
    }
}

#[tokio::test]
async fn source_managed_query_only_batches_accept_state_queries() {
    let mut feeder = Feeder::start(EngineConfig::CompactBfs, true).await;
    feeder.import(json!(["1"]), json!([])).await;
    let reply = batch(
        &feeder.app,
        json!([{"op":"node_state","node":"1"},{"op":"edge_state","source":"1","target":"2"},connected("1","1")]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.raw);
    assert_eq!(reply.body["results"][0]["registered"], true);
}

#[tokio::test]
async fn null_flags_are_rejected_as_malformed_in_every_configuration() {
    for lifecycle in [false, true] {
        let mut feeder = Feeder::start(EngineConfig::CompactBfs, lifecycle).await;
        let reply = feeder
            .import(
                json!(["1", "2"]),
                json!([{"source":"1","target":"2","enabled":null}]),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.raw
        );
        assert_eq!(reply.body["code"], "invalid_request");
        let reply = feeder
            .import(json!([{"node":"1","active":null}]), json!([]))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.raw
        );
        assert_eq!(
            feeder.import(json!(["1"]), json!([])).await.status,
            StatusCode::CREATED
        );
        let event = feeder.event("e1", json!({"op":"add_node","node":"3","active":null}));
        let reply = feeder.events(vec![event]).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.raw
        );
        assert_eq!(get(&feeder.app, "/v1/stats").await["state_version"], "0");
    }
    let app = router_with_config(config(
        EngineConfig::CompactBfs,
        true,
        ServerMode::Standalone,
    ));
    let reply = op(
        &app,
        json!({"op":"set_node_active","node":"1","active":null}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn feeder_edge_setter_checks_endpoints_in_supplied_order() {
    let mut feeder = Feeder::start(EngineConfig::CompactBfs, true).await;
    feeder.import(json!(["1"]), json!([])).await;
    let event = feeder.event(
        "e1",
        json!({"op":"set_edge_enabled","source":"9","target":"3","enabled":false}),
    );
    let reply = feeder.events(vec![event]).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw
    );
    assert_eq!(reply.body["code"], "unknown_node");
    assert_eq!(reply.body["message"], "unknown node: 9");
}

#[tokio::test]
async fn replacement_import_publishes_new_flags_under_if_match() {
    for engine in ENGINES {
        let mut feeder = Feeder::start(engine, true).await;
        feeder
            .import(
                json!(["1", "2", "3"]),
                json!([{"source":"1","target":"2"},{"source":"2","target":"3"}]),
            )
            .await;
        let event = feeder.event(
            "e1",
            json!({"op":"set_node_active","node":"2","active":false}),
        );
        feeder.events(vec![event]).await;
        let etag = send(&feeder.app, Method::GET, "/v1/stats", None, "")
            .await
            .etag
            .expect("stats ETag");
        let body = json!({
            "mode":"replace","instance_id":feeder.instance,"graph_id":"default",
            "source_id":"src","source_epoch":"e2","source_position":"q1",
            "feeder_session_id":"s","feeder_epoch":1,
            "nodes":["1", {"node":"2","active":true}, "3"],
            "edges":[{"source":"1","target":"2","enabled":false},{"source":"2","target":"3"}],
        });
        let response = feeder
            .app
            .clone()
            .oneshot(
                Request::post("/v1/feeder/import")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-knotrel-instance-id", &feeder.instance)
                    .header(header::IF_MATCH, &etag)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{engine:?}");
        let stats = get(&feeder.app, "/v1/stats").await;
        assert_eq!(stats["generation"], 2);
        assert_eq!(stats["state_version"], "0");
        assert_eq!(stats["active_node_count"], 3);
        assert_eq!(stats["enabled_edge_count"], 1);
        assert_eq!(
            feeder.query(connected("1", "3")).await.body["connected"],
            false
        );
        assert_eq!(
            feeder.query(connected("2", "3")).await.body["connected"],
            true
        );
    }
}

#[tokio::test]
async fn compaction_under_http_churn_keeps_answers_and_counts() {
    for engine in ENGINES {
        let app = router_with_config(config(engine, true, ServerMode::Standalone));
        op(&app, json!({"op":"link","source":"1","target":"2"})).await;
        op(
            &app,
            json!({"op":"set_edge_enabled","source":"1","target":"2","enabled":false}),
        )
        .await;
        // 1,500 distinct removed IDs exceed the 1,024 retired-slot threshold.
        for chunk in 0..3_u64 {
            let mut operations = Vec::new();
            for i in 0..500_u64 {
                let id = (1_000 + chunk * 500 + i).to_string();
                operations.push(json!({"op":"link","source":id,"target":"1"}));
                operations.push(json!({"op":"remove_node","node":id}));
            }
            let reply = batch(&app, json!(operations)).await;
            assert_eq!(reply.status, StatusCode::OK);
        }
        let stats = get(&app, "/v1/stats").await;
        assert_eq!(
            (stats["node_count"].clone(), stats["edge_count"].clone()),
            (json!(2), json!(1))
        );
        assert_eq!(stats["effective_edge_count"], 0);
        assert_eq!(op(&app, connected("1", "2")).await.body["connected"], false);
        op(
            &app,
            json!({"op":"set_edge_enabled","source":"2","target":"1","enabled":true}),
        )
        .await;
        assert_eq!(op(&app, connected("1", "2")).await.body["connected"], true);
    }
}

/// Lifecycle support cannot be enabled for `ett` or `hdt` through the embedded
/// API, so no router with such a configuration can serve a request; with
/// lifecycle disabled those engines keep their v0.4.0 behavior: legacy
/// operations work, the reported engine is unchanged (no fallback) and
/// lifecycle requests are rejected as before.
#[tokio::test]
async fn deferred_engines_reject_lifecycle_and_keep_non_lifecycle_behavior() {
    for engine in [EngineConfig::EulerTour, EngineConfig::Hdt] {
        for mode in [ServerMode::Standalone, ServerMode::SourceManaged] {
            let error = ServerConfig::new("127.0.0.1:0".parse().unwrap(), engine, 8)
                .unwrap()
                .with_mode(mode)
                .with_topology_lifecycle(true)
                .unwrap_err();
            assert!(error.to_string().contains(engine.as_str()), "{error}");
        }
        let app = router_with_config(config(engine, false, ServerMode::Standalone));
        let info = get(&app, "/v1/info").await;
        assert_eq!(info["engine"], engine.as_str());
        assert!(info.get("topology_lifecycle").is_none(), "{info}");
        assert_eq!(
            op(&app, json!({"op":"link","source":"1","target":"2"}))
                .await
                .status,
            StatusCode::OK
        );
        let reply = op(&app, json!({"op":"connected","source":"1","target":"2"})).await;
        assert_eq!(reply.body["connected"], true);
        let reply = op(
            &app,
            json!({"op":"set_node_active","node":"1","active":false}),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        assert_eq!(reply.body["code"], "topology_lifecycle_disabled");
        let reply = op(&app, json!({"op":"connected","source":"1","target":"2"})).await;
        assert_eq!(
            reply.body["connected"], true,
            "the rejected request changed nothing"
        );
    }
}

/// Malformed lifecycle events are refused with 422 and explicit messages:
/// an unknown operation name lists the lifecycle operations, and each setter
/// requires exactly its own flag. A self-loop in a flagged import is refused.
#[tokio::test]
async fn malformed_lifecycle_events_and_self_loop_imports_are_refused() {
    for (event, message) in [
        (
            json!({"op":"frobnicate","node":"1"}),
            "set_node_active, set_edge_enabled, or remove_node",
        ),
        (
            json!({"op":"set_node_active","node":"1","enabled":false}),
            "set_node_active operation requires active, and must not have enabled",
        ),
        (
            json!({"op":"set_edge_enabled","source":"1","target":"2","active":false}),
            "set_edge_enabled operation requires enabled, and must not have active",
        ),
    ] {
        let mut feeder = Feeder::start(EngineConfig::CompactBfs, true).await;
        assert_eq!(
            feeder.import(json!(["1", "2"]), json!([])).await.status,
            StatusCode::CREATED
        );
        let event = feeder.event("e1", event);
        let reply = feeder.events(vec![event]).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.raw
        );
        assert!(reply.raw.contains(message), "{}", reply.raw);
    }

    let mut fresh = Feeder::start(EngineConfig::CompactBfs, true).await;
    let reply = fresh
        .import(
            json!(["1"]),
            json!([{"source":"3","target":"3","enabled":false}]),
        )
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.raw
    );
    assert_eq!(reply.body["code"], "self_loop");
}
