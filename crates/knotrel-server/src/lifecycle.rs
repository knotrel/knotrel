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
use tokio::sync::watch;

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

#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerAction {
    Proceed,
    Panic,
}

#[cfg(feature = "test-support")]
struct WorkerHook {
    started_tx: tokio::sync::oneshot::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<WorkerAction>,
}

#[cfg(feature = "test-support")]
impl std::fmt::Debug for WorkerHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHook").finish_non_exhaustive()
    }
}

/// Handle allowing test callers to pause or panic the next worker deterministically.
///
/// Only available when the `test-support` feature is enabled. Not supported for production use.
#[cfg(feature = "test-support")]
pub struct WorkerPauseHandle {
    started_rx: tokio::sync::oneshot::Receiver<()>,
    release_tx: std::sync::mpsc::Sender<WorkerAction>,
}

#[cfg(feature = "test-support")]
impl WorkerPauseHandle {
    /// Awaits until the worker has reached the controlled blocking point.
    pub async fn wait_started(&mut self) {
        let _ = (&mut self.started_rx).await;
    }

    /// Releases the worker so it proceeds normally.
    pub fn release(self) {
        let _ = self.release_tx.send(WorkerAction::Proceed);
    }

    /// Releases the worker and instructs it to panic immediately.
    pub fn panic_on_release(self) {
        let _ = self.release_tx.send(WorkerAction::Panic);
    }
}

#[cfg(feature = "test-support")]
type DrainLoopCallback = Box<dyn FnMut(bool, bool, usize) + Send + 'static>;

#[cfg(feature = "test-support")]
struct DrainLoopHook(DrainLoopCallback);

#[cfg(feature = "test-support")]
impl std::fmt::Debug for DrainLoopHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrainLoopHook").finish_non_exhaustive()
    }
}

/// Decision made by `shutdown_gate` for an incoming request.
///
/// Only available when `test-support` feature is enabled.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateDecision {
    /// Request was admitted before draining began.
    Admitted,
    /// Request reached the gate while draining and was rejected with 503 `shutting_down`.
    RejectedShuttingDown,
    /// Request is a probe route (/health, /ready) allowed during drain.
    Probe,
}

#[cfg(feature = "test-support")]
type GateCallback = Box<dyn FnMut(&str, GateDecision) + Send + 'static>;

#[cfg(feature = "test-support")]
struct GateHook(GateCallback);

#[cfg(feature = "test-support")]
impl std::fmt::Debug for GateHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateHook").finish_non_exhaustive()
    }
}

#[cfg(feature = "test-support")]
struct ReadyProbeHook {
    started_tx: tokio::sync::oneshot::Sender<()>,
    release_rx: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(feature = "test-support")]
impl std::fmt::Debug for ReadyProbeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadyProbeHook").finish_non_exhaustive()
    }
}

/// Handle allowing test callers to pause the next `/ready` probe after lease expiry observation.
///
/// Only available when the `test-support` feature is enabled. Not supported for production use.
#[cfg(feature = "test-support")]
pub struct ProbePauseHandle {
    started_rx: tokio::sync::oneshot::Receiver<()>,
    release_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

#[cfg(feature = "test-support")]
impl ProbePauseHandle {
    /// Awaits until the readiness probe has reached the controlled pause point.
    pub async fn wait_started(&mut self) {
        let _ = (&mut self.started_rx).await;
    }

    /// Releases the readiness probe so it proceeds with publishing.
    pub fn release(mut self) {
        if let Some(tx) = self.release_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug)]
struct LifecycleInner {
    phase: AtomicU8,
    jobs_tx: watch::Sender<usize>,
    jobs_rx: watch::Receiver<usize>,
    #[cfg(feature = "test-support")]
    worker_hook: parking_lot::Mutex<Option<WorkerHook>>,
    #[cfg(feature = "test-support")]
    worker_post_lock_hook: parking_lot::Mutex<Option<WorkerHook>>,
    #[cfg(feature = "test-support")]
    drain_loop_hook: parking_lot::Mutex<Option<DrainLoopHook>>,
    #[cfg(feature = "test-support")]
    gate_hook: parking_lot::Mutex<Option<GateHook>>,
    #[cfg(feature = "test-support")]
    ready_probe_hook: parking_lot::Mutex<Option<ReadyProbeHook>>,
}

/// Token tracking an active graph worker job.
///
/// Dropping this token decrements the active worker count on every path,
/// including normal return, early error, or unwinding from a panic.
pub(crate) struct JobGuard {
    jobs_tx: watch::Sender<usize>,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.jobs_tx.send_modify(|count| {
            *count = count.saturating_sub(1);
        });
    }
}

/// Shared handle for the lifecycle phase and worker tracking.
///
/// Phases only move forward: `Starting`, then `Ready`, then `Draining`.
/// Cloned handles observe the same phase and job count.
#[derive(Clone, Debug)]
pub struct Lifecycle(Arc<LifecycleInner>);

impl Lifecycle {
    /// Creates a handle in [`Phase::Starting`].
    pub fn new() -> Self {
        let (jobs_tx, jobs_rx) = watch::channel(0);
        #[cfg(feature = "test-support")]
        let inner = LifecycleInner {
            phase: AtomicU8::new(STARTING),
            jobs_tx,
            jobs_rx,
            worker_hook: parking_lot::Mutex::new(None),
            worker_post_lock_hook: parking_lot::Mutex::new(None),
            drain_loop_hook: parking_lot::Mutex::new(None),
            gate_hook: parking_lot::Mutex::new(None),
            ready_probe_hook: parking_lot::Mutex::new(None),
        };
        #[cfg(not(feature = "test-support"))]
        let inner = LifecycleInner {
            phase: AtomicU8::new(STARTING),
            jobs_tx,
            jobs_rx,
        };
        Self(Arc::new(inner))
    }

    /// Creates a handle that already reports [`Phase::Ready`].
    pub(crate) fn ready() -> Self {
        let (jobs_tx, jobs_rx) = watch::channel(0);
        #[cfg(feature = "test-support")]
        let inner = LifecycleInner {
            phase: AtomicU8::new(READY),
            jobs_tx,
            jobs_rx,
            worker_hook: parking_lot::Mutex::new(None),
            worker_post_lock_hook: parking_lot::Mutex::new(None),
            drain_loop_hook: parking_lot::Mutex::new(None),
            gate_hook: parking_lot::Mutex::new(None),
            ready_probe_hook: parking_lot::Mutex::new(None),
        };
        #[cfg(not(feature = "test-support"))]
        let inner = LifecycleInner {
            phase: AtomicU8::new(READY),
            jobs_tx,
            jobs_rx,
        };
        Self(Arc::new(inner))
    }

    /// Returns the current phase.
    pub fn phase(&self) -> Phase {
        match self.0.phase.load(Ordering::Acquire) {
            STARTING => Phase::Starting,
            READY => Phase::Ready,
            _ => Phase::Draining,
        }
    }

    /// Moves `Starting` to `Ready`; has no effect once draining has begun.
    pub fn mark_ready(&self) {
        let _ = self
            .0
            .phase
            .compare_exchange(STARTING, READY, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Moves any phase to `Draining`; this is irreversible.
    pub fn begin_draining(&self) {
        self.0.phase.store(DRAINING, Ordering::Release);
    }

    /// Tracks an outstanding graph worker job.
    pub(crate) fn track_job(&self) -> JobGuard {
        self.0.jobs_tx.send_modify(|count| *count += 1);
        JobGuard {
            jobs_tx: self.0.jobs_tx.clone(),
        }
    }

    /// Number of outstanding graph jobs currently queued or running.
    pub fn outstanding_jobs(&self) -> usize {
        *self.0.jobs_rx.borrow()
    }

    /// Receiver observing changes to the outstanding graph job count.
    pub(crate) fn jobs_receiver(&self) -> watch::Receiver<usize> {
        self.0.jobs_rx.clone()
    }

    /// Installs a pause point for the next worker job to facilitate deterministic testing.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn pause_next_worker(&self) -> WorkerPauseHandle {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut hook = self.0.worker_hook.lock();
        *hook = Some(WorkerHook {
            started_tx,
            release_rx,
        });
        WorkerPauseHandle {
            started_rx,
            release_tx,
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn check_worker_hook(&self) {
        let hook = {
            let mut guard = self.0.worker_hook.lock();
            guard.take()
        };
        if let Some(hook) = hook {
            let _ = hook.started_tx.send(());
            if let Ok(WorkerAction::Panic) = hook.release_rx.recv() {
                panic!("worker paused by test hook instructed to panic");
            }
        }
    }

    #[cfg(not(feature = "test-support"))]
    #[inline(always)]
    pub(crate) fn check_worker_hook(&self) {}

    /// Installs a pause point for the next worker job immediately after releasing the graph lock.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn pause_next_worker_after_lock(&self) -> WorkerPauseHandle {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut hook = self.0.worker_post_lock_hook.lock();
        *hook = Some(WorkerHook {
            started_tx,
            release_rx,
        });
        WorkerPauseHandle {
            started_rx,
            release_tx,
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn check_worker_post_lock_hook(&self) {
        let hook = {
            let mut guard = self.0.worker_post_lock_hook.lock();
            guard.take()
        };
        if let Some(hook) = hook {
            let _ = hook.started_tx.send(());
            if let Ok(WorkerAction::Panic) = hook.release_rx.recv() {
                panic!("worker paused by post-lock test hook instructed to panic");
            }
        }
    }

    #[cfg(not(feature = "test-support"))]
    #[inline(always)]
    pub(crate) fn check_worker_post_lock_hook(&self) {}

    /// Installs a callback hook triggered at the drain observation boundary.
    ///
    /// Parameters passed to the callback:
    /// - `draining`: whether `Phase::Draining` has begun.
    /// - `tasks_empty`: whether `tasks.is_empty()` in `serve()`.
    /// - `active_jobs`: the outstanding job count read by `borrow_and_update()`.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn on_drain_loop<F>(&self, f: F)
    where
        F: FnMut(bool, bool, usize) + Send + 'static,
    {
        *self.0.drain_loop_hook.lock() = Some(DrainLoopHook(Box::new(f)));
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn check_drain_loop_hook(
        &self,
        draining: bool,
        tasks_empty: bool,
        active_jobs: usize,
    ) {
        let mut hook = self.0.drain_loop_hook.lock();
        if let Some(hook) = hook.as_mut() {
            (hook.0)(draining, tasks_empty, active_jobs);
        }
    }

    /// Installs a callback hook triggered at the shutdown gate admission boundary.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn on_gate<F>(&self, f: F)
    where
        F: FnMut(&str, GateDecision) + Send + 'static,
    {
        *self.0.gate_hook.lock() = Some(GateHook(Box::new(f)));
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn record_gate_decision(&self, path: &str, decision: GateDecision) {
        let mut hook = self.0.gate_hook.lock();
        if let Some(hook) = hook.as_mut() {
            (hook.0)(path, decision);
        }
    }

    /// Installs a pause point for the next readiness probe after observing lease expiry.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn pause_next_ready_probe(&self) -> ProbePauseHandle {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let mut hook = self.0.ready_probe_hook.lock();
        *hook = Some(ReadyProbeHook {
            started_tx,
            release_rx,
        });
        ProbePauseHandle {
            started_rx,
            release_tx: Some(release_tx),
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) async fn check_ready_probe_hook(&self) {
        let hook = {
            let mut guard = self.0.ready_probe_hook.lock();
            guard.take()
        };
        if let Some(hook) = hook {
            let _ = hook.started_tx.send(());
            let _ = hook.release_rx.await;
        }
    }

    #[cfg(not(feature = "test-support"))]
    #[inline(always)]
    pub(crate) async fn check_ready_probe_hook(&self) {}
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
/// A failed graph worker wins over every other state, then draining, then
/// starting. It never takes the graph lock or a worker permit.
pub(crate) async fn readiness(lifecycle: &Lifecycle, service: &Service) -> Response {
    let (status, label) = if service.is_unavailable() {
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    } else {
        match lifecycle.phase() {
            Phase::Draining => (StatusCode::SERVICE_UNAVAILABLE, "draining"),
            Phase::Starting => (StatusCode::SERVICE_UNAVAILABLE, "starting"),
            Phase::Ready => {
                if service.mode() == crate::ServerMode::Standalone {
                    (StatusCode::OK, "ready")
                } else {
                    let word = service.published_readiness_word();
                    let published = crate::service::PublishedReadiness::unpack(word);
                    let is_expired = published.deadline_millis != 0
                        && crate::service::now_to_millis() > published.deadline_millis;

                    match published.status {
                        crate::service::SyncStatus::Uninitialized => {
                            (StatusCode::SERVICE_UNAVAILABLE, "uninitialized")
                        }
                        crate::service::SyncStatus::Loading => {
                            (StatusCode::SERVICE_UNAVAILABLE, "loading")
                        }
                        crate::service::SyncStatus::Desynchronized => {
                            (StatusCode::SERVICE_UNAVAILABLE, "desynchronized")
                        }
                        crate::service::SyncStatus::Stale => {
                            (StatusCode::SERVICE_UNAVAILABLE, "stale")
                        }
                        crate::service::SyncStatus::CatchingUp => {
                            if is_expired {
                                (StatusCode::SERVICE_UNAVAILABLE, "stale")
                            } else {
                                (StatusCode::SERVICE_UNAVAILABLE, "catching_up")
                            }
                        }
                        crate::service::SyncStatus::Ready => {
                            if is_expired {
                                lifecycle.check_ready_probe_hook().await;

                                if service.try_mark_stale_from_expired_lease(word) {
                                    (StatusCode::SERVICE_UNAVAILABLE, "stale")
                                } else {
                                    let fresh = service.published_readiness();
                                    let fresh_expired = fresh.deadline_millis != 0
                                        && crate::service::now_to_millis() > fresh.deadline_millis;
                                    if fresh.status == crate::service::SyncStatus::Ready
                                        && !fresh_expired
                                    {
                                        (StatusCode::OK, "ready")
                                    } else if fresh.status == crate::service::SyncStatus::Ready
                                        && fresh_expired
                                    {
                                        (StatusCode::SERVICE_UNAVAILABLE, "stale")
                                    } else {
                                        (StatusCode::SERVICE_UNAVAILABLE, fresh.status.as_str())
                                    }
                                }
                            } else {
                                (StatusCode::OK, "ready")
                            }
                        }
                    }
                }
            }
        }
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
    let path = request.uri().path();
    let is_draining = lifecycle.phase() == Phase::Draining;
    let probe = is_probe(path);

    #[cfg(feature = "test-support")]
    {
        let decision = if !is_draining {
            GateDecision::Admitted
        } else if probe {
            GateDecision::Probe
        } else {
            GateDecision::RejectedShuttingDown
        };
        lifecycle.record_gate_decision(path, decision);
    }

    if !is_draining {
        return next.run(request).await;
    }
    let mut response = if probe {
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
