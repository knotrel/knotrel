//! Feeder admission refusals over HTTP (decision 4 of the v1.0 contract
//! freeze review): an exhausted feeder admission lane answers `503 busy` on
//! every route of that lane, and `503 import_in_progress` is reserved for a
//! concurrent import. Each test tells the two conditions apart.

#![cfg(feature = "test-support")]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use knotrel_core::EngineConfig;
use knotrel_server::{Lifecycle, ServerConfig, ServerMode, router_with_lifecycle};
use std::time::Duration;
use tower::ServiceExt;

const STEP: Duration = Duration::from_secs(5);

/// A source-managed router with one feeder data permit (the control lane
/// always has two).
fn app() -> (Router, Lifecycle) {
    let config = ServerConfig::new("127.0.0.1:0".parse().unwrap(), EngineConfig::CompactBfs, 4)
        .unwrap()
        .with_mode(ServerMode::SourceManaged)
        .with_feeder_reserved_jobs(1)
        .unwrap();
    let lifecycle = Lifecycle::new();
    lifecycle.mark_ready();
    (router_with_lifecycle(config, lifecycle.clone()), lifecycle)
}

async fn instance_id(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    info["instance_id"].as_str().unwrap().to_owned()
}

/// Sends a feeder request; returns the status and the decoded error body
/// (`Value::Null` when the body is not JSON).
async fn feeder(
    app: &Router,
    instance: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-knotrel-instance-id", instance)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn session(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "lease_duration_ms": 10_000,
    })
}

fn heartbeat(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
    })
}

fn gap(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance,
        "graph_id": "default",
        "generation": 1,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "failed_delivery_sequence": 1,
    })
}

fn barrier(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance,
        "graph_id": "default",
        "generation": 1,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "barrier_delivery_sequence": 1,
    })
}

fn events(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance,
        "graph_id": "default",
        "generation": 1,
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "source_id": "pg-1",
        "source_epoch": "ep-1",
        "events": [{
            "event_id": "e-1",
            "delivery_sequence": 1,
            "prev_delivery_sequence": 0,
            "source_position": "pos-1",
            "prev_source_position": "pos-0",
            "op": "progress",
        }],
    })
}

fn import(instance: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": "create_only",
        "instance_id": instance,
        "graph_id": "default",
        "feeder_session_id": "s",
        "feeder_epoch": 1,
        "source_id": "pg-1",
        "source_epoch": "ep-1",
        "source_position": "pos-0",
        "nodes": [],
        "edges": [],
    })
}

fn assert_busy(status: StatusCode, body: &serde_json::Value, lane: &str) {
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "busy", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains(lane), "{message}");
    assert!(!message.contains("import"), "{message}");
}

fn assert_not_refused(status: StatusCode, body: &serde_json::Value) {
    assert!(
        !(status == StatusCode::SERVICE_UNAVAILABLE
            && (body["code"] == "busy" || body["code"] == "import_in_progress")),
        "{status} {body}"
    );
}

async fn wait_jobs(lifecycle: &Lifecycle, jobs: usize) {
    tokio::time::timeout(STEP, async {
        while lifecycle.outstanding_jobs() != jobs {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("outstanding jobs never reached the expected count");
}

/// A full data lane refuses events, barrier and gap with `busy`; the control
/// lane and the import permit stay available, so the refusal cannot be a
/// concurrent import.
#[tokio::test]
async fn exhausted_data_lane_answers_busy_on_events_barrier_and_gap() {
    let (app, lifecycle) = app();
    let instance = instance_id(&app).await;
    let mut pause = lifecycle.pause_next_worker();
    let held = tokio::spawn({
        let (app, instance) = (app.clone(), instance.clone());
        async move { feeder(&app, &instance, "/v1/feeder/gap", gap(&instance)).await }
    });
    tokio::time::timeout(STEP, pause.wait_started())
        .await
        .expect("held data job never started");

    for (path, body) in [
        ("/v1/feeder/events", events(&instance)),
        ("/v1/feeder/barrier", barrier(&instance)),
        ("/v1/feeder/gap", gap(&instance)),
    ] {
        let (status, reply) = feeder(&app, &instance, path, body).await;
        assert_busy(status, &reply, "feeder data");
    }
    // The control lane and the import permit are unaffected.
    let (status, reply) = feeder(&app, &instance, "/v1/feeder/session", session(&instance)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = feeder(
        &app,
        &instance,
        "/v1/feeder/heartbeat",
        heartbeat(&instance),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = feeder(&app, &instance, "/v1/feeder/import", import(&instance)).await;
    assert_not_refused(status, &reply);

    pause.release();
    let _ = held.await.unwrap();
    wait_jobs(&lifecycle, 0).await;
}

/// A full control lane (both permits held) refuses session and heartbeat
/// with `busy`; the data lane and the import permit stay available.
#[tokio::test]
async fn exhausted_control_lane_answers_busy_on_session_and_heartbeat() {
    let (app, lifecycle) = app();
    let instance = instance_id(&app).await;
    // First control job: paused before it requests the graph lock.
    let mut first = lifecycle.pause_next_worker();
    let held_first = tokio::spawn({
        let (app, instance) = (app.clone(), instance.clone());
        async move { feeder(&app, &instance, "/v1/feeder/session", session(&instance)).await }
    });
    tokio::time::timeout(STEP, first.wait_started())
        .await
        .expect("first control job never started");
    // Second control job: paused at its lock request, also without the lock.
    let mut second = lifecycle.pause_next_lock_request();
    let held_second = tokio::spawn({
        let (app, instance) = (app.clone(), instance.clone());
        async move {
            feeder(
                &app,
                &instance,
                "/v1/feeder/heartbeat",
                heartbeat(&instance),
            )
            .await
        }
    });
    tokio::time::timeout(STEP, second.wait_started())
        .await
        .expect("second control job never started");

    for (path, body) in [
        ("/v1/feeder/session", session(&instance)),
        ("/v1/feeder/heartbeat", heartbeat(&instance)),
    ] {
        let (status, reply) = feeder(&app, &instance, path, body).await;
        assert_busy(status, &reply, "feeder control");
    }
    // The data lane and the import permit are unaffected (the graph lock is
    // free: neither paused job holds it).
    let (status, reply) = feeder(&app, &instance, "/v1/feeder/gap", gap(&instance)).await;
    assert_not_refused(status, &reply);
    let (status, reply) = feeder(&app, &instance, "/v1/feeder/import", import(&instance)).await;
    assert_not_refused(status, &reply);

    first.release();
    second.release();
    let _ = held_first.await.unwrap();
    let _ = held_second.await.unwrap();
    wait_jobs(&lifecycle, 0).await;
}

/// A concurrent import answers `import_in_progress` to the second import
/// only; neither feeder lane is refused meanwhile.
#[tokio::test]
async fn concurrent_import_is_import_in_progress_and_refuses_no_lane() {
    let (app, lifecycle) = app();
    let instance = instance_id(&app).await;
    let mut pause = lifecycle.pause_next_worker();
    let held = tokio::spawn({
        let (app, instance) = (app.clone(), instance.clone());
        async move { feeder(&app, &instance, "/v1/feeder/import", import(&instance)).await }
    });
    tokio::time::timeout(STEP, pause.wait_started())
        .await
        .expect("held import never reached its worker");

    let (status, reply) = feeder(&app, &instance, "/v1/feeder/import", import(&instance)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{reply}");
    assert_eq!(reply["code"], "import_in_progress", "{reply}");

    // The paused import holds no lane permit and not the graph lock (it is
    // paused before parsing), so feeder calls on both lanes are admitted
    // and run to their own outcome meanwhile.
    for (path, body) in [
        ("/v1/feeder/session", session(&instance)),
        ("/v1/feeder/heartbeat", heartbeat(&instance)),
        ("/v1/feeder/gap", gap(&instance)),
        ("/v1/feeder/barrier", barrier(&instance)),
        ("/v1/feeder/events", events(&instance)),
    ] {
        let (status, reply) = feeder(&app, &instance, path, body).await;
        assert_not_refused(status, &reply);
    }
    pause.release();
    let _ = held.await.unwrap();
    wait_jobs(&lifecycle, 0).await;
}
