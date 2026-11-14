//! Process lifecycle phase shared by the accept loop and the HTTP router.

use crate::{
    api::{self, ErrorCode, Route},
    metrics::{self, Rejection, ServiceTelemetry},
    service::Service,
};
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
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

/// Externally visible server phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Phase {
    /// The server exists but is not yet accepting connections.
    Starting = 0,
    /// The server is accepting connections and serving requests.
    Ready = 1,
    /// Shutdown has begun: admitted work finishes, everything else is rejected.
    Draining = 2,
}

impl Phase {
    const fn from_code(code: u8) -> Self {
        match code {
            code if code == Self::Starting as u8 => Self::Starting,
            code if code == Self::Ready as u8 => Self::Ready,
            _ => Self::Draining,
        }
    }
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
    lock_request_hook: parking_lot::Mutex<Option<WorkerHook>>,
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
            phase: AtomicU8::new(Phase::Starting as u8),
            jobs_tx,
            jobs_rx,
            worker_hook: parking_lot::Mutex::new(None),
            worker_post_lock_hook: parking_lot::Mutex::new(None),
            lock_request_hook: parking_lot::Mutex::new(None),
            drain_loop_hook: parking_lot::Mutex::new(None),
            gate_hook: parking_lot::Mutex::new(None),
            ready_probe_hook: parking_lot::Mutex::new(None),
        };
        #[cfg(not(feature = "test-support"))]
        let inner = LifecycleInner {
            phase: AtomicU8::new(Phase::Starting as u8),
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
            phase: AtomicU8::new(Phase::Ready as u8),
            jobs_tx,
            jobs_rx,
            worker_hook: parking_lot::Mutex::new(None),
            worker_post_lock_hook: parking_lot::Mutex::new(None),
            lock_request_hook: parking_lot::Mutex::new(None),
            drain_loop_hook: parking_lot::Mutex::new(None),
            gate_hook: parking_lot::Mutex::new(None),
            ready_probe_hook: parking_lot::Mutex::new(None),
        };
        #[cfg(not(feature = "test-support"))]
        let inner = LifecycleInner {
            phase: AtomicU8::new(Phase::Ready as u8),
            jobs_tx,
            jobs_rx,
        };
        Self(Arc::new(inner))
    }

    /// Returns the current phase.
    pub fn phase(&self) -> Phase {
        Phase::from_code(self.0.phase.load(Ordering::Acquire))
    }

    /// Moves `Starting` to `Ready`; has no effect once draining has begun.
    pub fn mark_ready(&self) {
        let _ = self.0.phase.compare_exchange(
            Phase::Starting as u8,
            Phase::Ready as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Moves any phase to `Draining`; this is irreversible.
    pub fn begin_draining(&self) {
        self.0.phase.store(Phase::Draining as u8, Ordering::Release);
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

    /// Installs a pause point for the next worker job immediately before it
    /// requests the graph lock, after its lock-wait timing has started.
    ///
    /// Only available when the `test-support` feature is enabled.
    #[cfg(feature = "test-support")]
    pub fn pause_next_lock_request(&self) -> WorkerPauseHandle {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut hook = self.0.lock_request_hook.lock();
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
    pub(crate) fn check_lock_request_hook(&self) {
        let hook = {
            let mut guard = self.0.lock_request_hook.lock();
            guard.take()
        };
        if let Some(hook) = hook {
            let _ = hook.started_tx.send(());
            if let Ok(WorkerAction::Panic) = hook.release_rx.recv() {
                panic!("worker paused by lock-request test hook instructed to panic");
            }
        }
    }

    #[cfg(not(feature = "test-support"))]
    #[inline(always)]
    pub(crate) fn check_lock_request_hook(&self) {}

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

/// State reported by readiness probes and their Prometheus mirror.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReadinessState {
    Unavailable,
    Draining,
    Starting,
    Ready,
    Uninitialized,
    Loading,
    CatchingUp,
    Stale,
    Desynchronized,
}

impl ReadinessState {
    pub(crate) const STANDALONE: [Self; 4] = [
        Self::Unavailable,
        Self::Draining,
        Self::Starting,
        Self::Ready,
    ];

    pub(crate) const SOURCE_MANAGED: [Self; 9] = [
        Self::Unavailable,
        Self::Draining,
        Self::Starting,
        Self::Ready,
        Self::Uninitialized,
        Self::Loading,
        Self::CatchingUp,
        Self::Stale,
        Self::Desynchronized,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Draining => "draining",
            Self::Starting => "starting",
            Self::Ready => crate::service::SyncStatus::Ready.as_str(),
            Self::Uninitialized => crate::service::SyncStatus::Uninitialized.as_str(),
            Self::Loading => crate::service::SyncStatus::Loading.as_str(),
            Self::CatchingUp => crate::service::SyncStatus::CatchingUp.as_str(),
            Self::Stale => crate::service::SyncStatus::Stale.as_str(),
            Self::Desynchronized => crate::service::SyncStatus::Desynchronized.as_str(),
        }
    }

    const fn from_sync_status(status: crate::service::SyncStatus) -> Self {
        match status {
            crate::service::SyncStatus::Uninitialized => Self::Uninitialized,
            crate::service::SyncStatus::Loading => Self::Loading,
            crate::service::SyncStatus::CatchingUp => Self::CatchingUp,
            crate::service::SyncStatus::Ready => Self::Ready,
            crate::service::SyncStatus::Stale => Self::Stale,
            crate::service::SyncStatus::Desynchronized => Self::Desynchronized,
        }
    }
}

#[derive(serde::Serialize)]
struct Status {
    status: ReadinessState,
}

/// Reports whether the server can currently accept graph work.
///
/// A failed graph worker wins over every other state, then draining, then
/// starting. It never takes the graph lock or a worker permit.
pub(crate) async fn readiness(lifecycle: &Lifecycle, service: &Service) -> Response {
    let (status, state) = if service.is_unavailable() {
        (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Unavailable)
    } else {
        match lifecycle.phase() {
            Phase::Draining => (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Draining),
            Phase::Starting => (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Starting),
            Phase::Ready => {
                if service.mode() == crate::ServerMode::Standalone {
                    (StatusCode::OK, ReadinessState::Ready)
                } else {
                    let word = service.published_readiness_word();
                    let published = crate::service::PublishedReadiness::unpack(word);
                    let is_expired = published.deadline_millis != 0
                        && crate::service::now_to_millis() > published.deadline_millis;

                    match published.status {
                        crate::service::SyncStatus::Uninitialized => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            ReadinessState::Uninitialized,
                        ),
                        crate::service::SyncStatus::Loading => {
                            (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Loading)
                        }
                        crate::service::SyncStatus::Desynchronized => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            ReadinessState::Desynchronized,
                        ),
                        crate::service::SyncStatus::Stale => {
                            (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Stale)
                        }
                        crate::service::SyncStatus::CatchingUp => {
                            if is_expired {
                                (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Stale)
                            } else {
                                (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::CatchingUp)
                            }
                        }
                        crate::service::SyncStatus::Ready => {
                            if is_expired {
                                lifecycle.check_ready_probe_hook().await;

                                if service.try_mark_stale_from_expired_lease(word) {
                                    (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Stale)
                                } else {
                                    let fresh = service.published_readiness();
                                    let fresh_expired = fresh.deadline_millis != 0
                                        && crate::service::now_to_millis() > fresh.deadline_millis;
                                    if fresh.status == crate::service::SyncStatus::Ready
                                        && !fresh_expired
                                    {
                                        (StatusCode::OK, ReadinessState::Ready)
                                    } else if fresh.status == crate::service::SyncStatus::Ready
                                        && fresh_expired
                                    {
                                        (StatusCode::SERVICE_UNAVAILABLE, ReadinessState::Stale)
                                    } else {
                                        (
                                            StatusCode::SERVICE_UNAVAILABLE,
                                            ReadinessState::from_sync_status(fresh.status),
                                        )
                                    }
                                }
                            } else {
                                (StatusCode::OK, ReadinessState::Ready)
                            }
                        }
                    }
                }
            }
        }
    };
    let mut response = (status, Json(Status { status: state })).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Returns the label `/ready` would report, without changing any state.
///
/// Precedence matches [`readiness`]: unavailable, draining, starting, then
/// (in source-managed mode) the published synchronization status. An
/// expired lease on `ready` or `catching_up` is reported as `stale`, but the
/// published word is never modified: lease expiry stays lazily recorded by
/// `/ready` and feeder calls, so telemetry observation cannot change what
/// other callers see. Lock-free; reads two atomics and the clock.
pub(crate) fn readiness_state(lifecycle: &Lifecycle, service: &Service) -> ReadinessState {
    use crate::service::{PublishedReadiness, SyncStatus, now_to_millis};

    if service.is_unavailable() {
        return ReadinessState::Unavailable;
    }
    match lifecycle.phase() {
        Phase::Draining => ReadinessState::Draining,
        Phase::Starting => ReadinessState::Starting,
        Phase::Ready if service.mode() == crate::ServerMode::Standalone => ReadinessState::Ready,
        Phase::Ready => {
            let published = PublishedReadiness::unpack(service.published_readiness_word());
            let expired =
                published.deadline_millis != 0 && now_to_millis() > published.deadline_millis;
            match published.status {
                SyncStatus::Ready | SyncStatus::CatchingUp if expired => ReadinessState::Stale,
                status => ReadinessState::from_sync_status(status),
            }
        }
    }
}

fn is_probe(path: &str) -> bool {
    path == Route::Health.as_str() || path == Route::Ready.as_str()
}

/// True only for the registered `/metrics` route; when metrics are disabled
/// the path is unmatched and draining rejects it like any other request.
pub(crate) fn is_metrics_scrape(request: &Request) -> bool {
    request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .is_some_and(|matched| matched.as_str() == Route::Metrics.as_str())
}

fn close_connection(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
}

/// State of [`shutdown_gate`]: the lifecycle and, when metrics are enabled,
/// the service's rejection counters.
#[derive(Clone)]
pub(crate) struct GateState {
    pub(crate) lifecycle: Lifecycle,
    pub(crate) telemetry: Option<Arc<ServiceTelemetry>>,
}

/// Rejects new work while draining and asks clients to close the connection.
///
/// Probes, and `/metrics` when enabled, stay answerable so orchestrators
/// can observe the drain. A request
/// that passed this gate before draining began runs to completion.
pub(crate) async fn shutdown_gate(
    State(GateState {
        lifecycle,
        telemetry,
    }): State<GateState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_draining = lifecycle.phase() == Phase::Draining;
    let probe = is_probe(path) || is_metrics_scrape(&request);

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
        if let Some(telemetry) = &telemetry {
            telemetry.reject(Rejection::ShuttingDown);
        }
        api::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ShuttingDown,
            "server is shutting down; retry against another instance".into(),
        )
    };
    close_connection(&mut response);
    response
}

/// State of [`request_timeout`].
#[derive(Clone, Copy)]
pub(crate) struct TimeoutPolicy {
    pub(crate) limit: Duration,
    /// Set when metrics are enabled, so an abandoned graph job can tell a
    /// timeout from any other cancellation.
    pub(crate) mark_timeouts: bool,
}

/// Bounds body receipt plus handling of one request.
///
/// Dropping the handler future cancels body reads and queued admission, but a
/// graph job already handed to a worker still completes: after a timeout the
/// client cannot tell whether its mutation was applied.
pub(crate) async fn request_timeout(
    State(policy): State<TimeoutPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let completed = if policy.mark_timeouts {
        let flag = Arc::new(AtomicBool::new(false));
        let mut handler = Box::pin(metrics::mark_timeouts(flag.clone(), next.run(request)));
        let result = tokio::time::timeout(policy.limit, handler.as_mut()).await;
        if result.is_err() {
            // Mark before dropping, so the handler's drop guards see it.
            flag.store(true, Ordering::Release);
            drop(handler);
        }
        result
    } else {
        tokio::time::timeout(policy.limit, next.run(request)).await
    };
    match completed {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                limit_ms = policy.limit.as_millis(),
                "request exceeded its time limit"
            );
            let mut response = api::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::RequestTimeout,
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
    fn readiness_domains_match_the_http_and_metrics_contract() {
        assert_eq!(
            ReadinessState::STANDALONE.map(ReadinessState::as_str),
            ["unavailable", "draining", "starting", "ready"]
        );
        assert_eq!(
            ReadinessState::SOURCE_MANAGED.map(ReadinessState::as_str),
            [
                "unavailable",
                "draining",
                "starting",
                "ready",
                "uninitialized",
                "loading",
                "catching_up",
                "stale",
                "desynchronized",
            ]
        );
        assert_eq!(crate::ServerMode::Standalone.as_str(), "standalone");
        assert_eq!(crate::ServerMode::SourceManaged.as_str(), "source_managed");
    }

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

    #[test]
    fn test_lifecycle_default_and_debug_impls() {
        let lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.phase(), Phase::Starting);
        assert!(!is_probe("/v1/stats"));
        assert!(is_probe("/health"));
        assert!(is_probe("/ready"));

        #[cfg(feature = "test-support")]
        {
            let (started_tx, _) = tokio::sync::oneshot::channel();
            let (_release_tx, release_rx) = std::sync::mpsc::channel();
            let worker_hook = WorkerHook {
                started_tx,
                release_rx,
            };
            assert!(format!("{worker_hook:?}").contains("WorkerHook"));

            let drain_hook = DrainLoopHook(Box::new(|_, _, _| {}));
            assert!(format!("{drain_hook:?}").contains("DrainLoopHook"));

            let gate_hook = GateHook(Box::new(|_, _| {}));
            assert!(format!("{gate_hook:?}").contains("GateHook"));

            let (p_tx, _) = tokio::sync::oneshot::channel();
            let (_, p_rx) = tokio::sync::oneshot::channel();
            let probe_hook = ReadyProbeHook {
                started_tx: p_tx,
                release_rx: p_rx,
            };
            assert!(format!("{probe_hook:?}").contains("ReadyProbeHook"));

            assert_eq!(format!("{:?}", GateDecision::Admitted), "Admitted");
            assert_eq!(
                format!("{:?}", GateDecision::RejectedShuttingDown),
                "RejectedShuttingDown"
            );
            assert_eq!(format!("{:?}", GateDecision::Probe), "Probe");
        }
    }

    #[tokio::test]
    async fn test_readiness_branches() {
        use crate::service::{Service, SyncStatus};
        use knotrel_core::ConnectivityGraph;

        let _clock_guard = crate::service::lock_clock_for_test().await;

        let lifecycle = Lifecycle::ready();
        let config = crate::ServerConfig::default().with_mode(crate::ServerMode::SourceManaged);
        let service =
            Service::with_config(ConnectivityGraph::default(), 2, lifecycle.clone(), &config);

        // 1. Uninitialized
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 2. Loading
        service.set_sync_status_for_test(SyncStatus::Loading);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 3. Desynchronized
        service.set_sync_status_for_test(SyncStatus::Desynchronized);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 4. Stale
        service.set_sync_status_for_test(SyncStatus::Stale);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 5. CatchingUp (non-expired deadline)
        let far_future = crate::service::now_to_millis() + 60_000;
        service.set_published_readiness_for_test(SyncStatus::CatchingUp, far_future);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Advance clock to ensure expiration
        crate::service::advance_clock(std::time::Duration::from_secs(10));

        // 6. CatchingUp (expired deadline -> stale)
        service.set_published_readiness_for_test(SyncStatus::CatchingUp, 1);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // 7. Ready (non-expired deadline -> OK 200)
        let far_future2 = crate::service::now_to_millis() + 60_000;
        service.set_published_readiness_for_test(SyncStatus::Ready, far_future2);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::OK);

        // 8. Ready (expired deadline -> try_mark_stale succeeds -> 503 stale)
        service.set_published_readiness_for_test(SyncStatus::Ready, 1);
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        crate::service::reset_clock();

        // 9. When service is unavailable
        service.mark_unavailable_for_test();
        let resp = readiness(&lifecycle, &service).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    async fn probe_label(lifecycle: &Lifecycle, service: &Service) -> String {
        let body = readiness(lifecycle, service).await.into_body();
        let bytes = axum::body::to_bytes(body, 1024).await.unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        text.trim_start_matches(r#"{"status":""#)
            .trim_end_matches(r#""}"#)
            .to_owned()
    }

    /// Telemetry must report what `/ready` would report, by the same
    /// precedence, without itself recording an expired lease as `stale`.
    #[tokio::test]
    async fn observed_readiness_matches_probe_without_transitioning() {
        use crate::service::{Service, SyncStatus};
        use knotrel_core::ConnectivityGraph;

        let _clock_guard = crate::service::lock_clock_for_test().await;
        crate::service::advance_clock(std::time::Duration::from_secs(10));
        let future = || crate::service::now_to_millis() + 60_000;

        let standalone = crate::ServerConfig::default();
        let managed = standalone
            .clone()
            .with_mode(crate::ServerMode::SourceManaged);
        let draining = Lifecycle::ready();
        draining.begin_draining();
        let phases = [
            ("starting", Lifecycle::new()),
            ("ready", Lifecycle::ready()),
            ("draining", draining),
        ];
        let statuses: [(SyncStatus, bool, &str); 8] = [
            (SyncStatus::Uninitialized, false, "uninitialized"),
            (SyncStatus::Loading, false, "loading"),
            (SyncStatus::Desynchronized, false, "desynchronized"),
            (SyncStatus::Stale, false, "stale"),
            (SyncStatus::CatchingUp, false, "catching_up"),
            (SyncStatus::CatchingUp, true, "stale"),
            (SyncStatus::Ready, false, "ready"),
            (SyncStatus::Ready, true, "stale"),
        ];

        for (phase, lifecycle) in phases {
            let service = Service::with_config(
                ConnectivityGraph::default(),
                2,
                lifecycle.clone(),
                &standalone,
            );
            assert_eq!(readiness_state(&lifecycle, &service).as_str(), phase);
            assert_eq!(probe_label(&lifecycle, &service).await, phase);

            for (status, expired, label) in statuses {
                let service = Service::with_config(
                    ConnectivityGraph::default(),
                    2,
                    lifecycle.clone(),
                    &managed,
                );
                let deadline = if expired { 1 } else { future() };
                service.set_published_readiness_for_test(status, deadline);
                let expected = if phase == "ready" { label } else { phase };
                let word = service.published_readiness_word();
                for _ in 0..2 {
                    assert_eq!(readiness_state(&lifecycle, &service).as_str(), expected);
                    assert_eq!(service.published_readiness_word(), word, "{status:?}");
                }
                assert_eq!(probe_label(&lifecycle, &service).await, expected);
                assert_eq!(readiness_state(&lifecycle, &service).as_str(), expected);

                service.mark_unavailable_for_test();
                assert_eq!(
                    readiness_state(&lifecycle, &service).as_str(),
                    "unavailable"
                );
                assert_eq!(probe_label(&lifecycle, &service).await, "unavailable");
            }
        }
        crate::service::reset_clock();
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn test_readiness_probe_race_branches() {
        use crate::service::{Service, SyncStatus};
        use knotrel_core::ConnectivityGraph;

        let _clock_guard = crate::service::lock_clock_for_test().await;
        crate::service::advance_clock(std::time::Duration::from_secs(10));

        let lifecycle = Lifecycle::ready();
        let config = crate::ServerConfig::default().with_mode(crate::ServerMode::SourceManaged);
        let service =
            Service::with_config(ConnectivityGraph::default(), 2, lifecycle.clone(), &config);

        // Branch 1: Probe pauses after observing expired lease; concurrent update sets Ready with fresh future deadline
        service.set_published_readiness_for_test(SyncStatus::Ready, 1);
        let mut handle = lifecycle.pause_next_ready_probe();
        let l_clone = lifecycle.clone();
        let s_clone = service.clone();
        let task = tokio::spawn(async move { readiness(&l_clone, &s_clone).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait_started())
            .await
            .expect("probe pause wait_started timed out");
        let far_future = crate::service::now_to_millis() + 60_000;
        service.set_published_readiness_for_test(SyncStatus::Ready, far_future);
        handle.release();
        let resp = task.await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Branch 2: Probe pauses after observing expired lease; concurrent update changes status to Desynchronized
        service.set_published_readiness_for_test(SyncStatus::Ready, 1);
        let mut handle = lifecycle.pause_next_ready_probe();
        let l_clone = lifecycle.clone();
        let s_clone = service.clone();
        let task = tokio::spawn(async move { readiness(&l_clone, &s_clone).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait_started())
            .await
            .expect("probe pause wait_started timed out");
        service.set_published_readiness_for_test(SyncStatus::Desynchronized, 0);
        handle.release();
        let resp = task.await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        // Branch 3: Probe pauses after observing expired lease; concurrent update sets Ready with still expired deadline
        service.set_published_readiness_for_test(SyncStatus::Ready, 1);
        let mut handle = lifecycle.pause_next_ready_probe();
        let l_clone = lifecycle.clone();
        let s_clone = service.clone();
        let task = tokio::spawn(async move { readiness(&l_clone, &s_clone).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait_started())
            .await
            .expect("probe pause wait_started timed out");
        service.set_published_readiness_for_test(SyncStatus::Ready, 1);
        handle.release();
        let resp = task.await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        crate::service::reset_clock();
    }
}
