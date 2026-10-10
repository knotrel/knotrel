//! Bounded CPU work and exclusive graph ownership for each request.

use crate::api::{Operation, Outcome};
use knotrel_core::ConnectivityGraph;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Shared graph state plus a strict limit on outstanding worker jobs.
#[derive(Clone)]
pub(crate) struct Service {
    graph: Arc<Mutex<GraphState>>,
    permits: Arc<Semaphore>,
    info: crate::ServerInfo,
}

/// Graph and revision share one mutex: snapshots cannot split a batch or observe
/// a mutation without its revision increment. Version starts at zero even when
/// embedding a pre-populated graph; it counts this service instance's changes.
struct GraphState {
    graph: ConnectivityGraph,
    // parking_lot does not poison on unwind. Mark each job before execution;
    // only normal completion clears this bit, while still holding the lock.
    // Queued jobs therefore cannot observe state left by a panicked worker.
    failed: bool,
    // Little-endian decimal digits avoid wraparound or saturation. Increment is
    // amortized O(1), worst-case O(log M), after M effective mutations. Storage
    // and snapshot conversion are O(log M); this adds no core-engine state.
    version: Vec<u8>,
}

impl GraphState {
    fn apply(&mut self, operation: Operation) -> Outcome {
        let outcome = operation.apply(&mut self.graph);
        if matches!(outcome, Outcome::Changed { changed: true }) {
            self.increment_version();
        }
        outcome
    }

    fn increment_version(&mut self) {
        for digit in &mut self.version {
            if *digit < 9 {
                *digit += 1;
                return;
            }
            *digit = 0;
        }
        self.version.push(1);
    }

    fn snapshot(&self) -> GraphSnapshot {
        let node_count = self.graph.node_count();
        let edge_count = self.graph.edge_count();
        let limits = self.graph.limits();
        GraphSnapshot {
            node_count,
            edge_count,
            max_nodes: limits.max_nodes,
            max_edges: limits.max_edges,
            remaining_nodes: limits.max_nodes.map(|limit| limit - node_count),
            remaining_edges: limits.max_edges.map(|limit| limit - edge_count),
            state_version: self
                .version
                .iter()
                .rev()
                .map(|digit| char::from(b'0' + digit))
                .collect(),
        }
    }
}

/// Owned coherent state, serialized after releasing the graph lock.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GraphSnapshot {
    node_count: usize,
    edge_count: usize,
    max_nodes: Option<usize>,
    max_edges: Option<usize>,
    remaining_nodes: Option<usize>,
    remaining_edges: Option<usize>,
    state_version: String,
}

/// Admission failure or a worker that cannot safely use the graph.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServiceError {
    Busy,
    Unavailable,
}

impl Service {
    /// Creates the service; zero capacity intentionally rejects every request.
    pub(crate) fn new(graph: ConnectivityGraph, capacity: usize) -> Self {
        let engine = graph.engine();
        Self {
            info: crate::ServerInfo {
                engine: engine.as_str(),
                experimental: engine.is_experimental(),
                max_pending_jobs: capacity,
                max_body_bytes: crate::MAX_BODY_BYTES,
                max_batch_operations: crate::MAX_BATCH_OPERATIONS,
                max_nodes: graph.limits().max_nodes,
                max_edges: graph.limits().max_edges,
            },
            graph: Arc::new(Mutex::new(GraphState {
                graph,
                failed: false,
                version: vec![0],
            })),
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    /// Reads effective immutable settings without waiting for graph work.
    pub(crate) fn info(&self) -> crate::ServerInfo {
        self.info.clone()
    }

    /// Executes a request on a blocking worker, preserving batch isolation.
    ///
    /// Admission is nonblocking: rejected work never enters Tokio's blocking
    /// queue. Moving the permit into the closure bounds queued plus running
    /// jobs even if the caller drops its future. The mutex is acquired only
    /// inside that closure, keeping traversal and lock contention off async
    /// executor threads. One guard spans the entire batch, so no other request
    /// can observe its intermediate states. Per-operation errors do not roll
    /// back earlier updates. A panic permanently marks the graph unavailable,
    /// including to queued jobs, until this service instance is replaced.
    pub(crate) async fn execute(
        &self,
        operations: Vec<Operation>,
    ) -> Result<Vec<Outcome>, ServiceError> {
        self.run(move |state| {
            operations
                .into_iter()
                .map(|operation| state.apply(operation))
                .collect()
        })
        .await
    }

    /// Takes a coherent snapshot through the same bounded worker path as writes.
    pub(crate) async fn stats(&self) -> Result<GraphSnapshot, ServiceError> {
        self.run(|state| state.snapshot()).await
    }

    // One admission/cancellation/poisoning policy for reads and writes. The
    // owned permit stays with the blocking closure even if its caller is gone.
    async fn run<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut GraphState) -> R + Send + 'static,
    ) -> Result<R, ServiceError> {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(
                capacity = self.info.max_pending_jobs,
                "request capacity exhausted; rejecting with 503 busy"
            );
            ServiceError::Busy
        })?;
        let graph = self.graph.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut graph = graph.lock();
            if graph.failed {
                tracing::warn!("graph worker state marked as failed; rejecting work");
                return Err(ServiceError::Unavailable);
            }
            graph.failed = true;
            let result = work(&mut graph);
            graph.failed = false;
            Ok(result)
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "graph worker thread panicked or aborted");
            ServiceError::Unavailable
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::WireNode;
    use std::time::Duration;

    fn add(node: u64) -> Operation {
        Operation::AddNode {
            node: WireNode(node),
        }
    }

    #[tokio::test]
    async fn saturated_service_rejects_without_mutating() {
        let service = Service::new(ConnectivityGraph::default(), 1);
        let permit = service.permits.clone().acquire_owned().await.unwrap();
        assert!(matches!(
            service.execute(vec![add(9)]).await,
            Err(ServiceError::Busy)
        ));
        drop(permit);
        assert!(matches!(
            service.execute(vec![add(9)]).await.unwrap()[0],
            Outcome::Changed { changed: true }
        ));
    }

    #[tokio::test]
    async fn dropped_caller_does_not_release_a_running_workers_permit() {
        let service = Service::new(ConnectivityGraph::default(), 1);
        let graph = service.graph.clone();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        // Hold the graph from a dedicated thread so the worker stays pending
        // without blocking the async test executor or depending on a large graph.
        let holder = std::thread::spawn(move || {
            let _guard = graph.lock();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        locked_rx.await.unwrap();
        let caller_service = service.clone();
        let caller = tokio::spawn(async move { caller_service.execute(vec![add(7)]).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        let _ = caller.await;
        assert_eq!(service.permits.available_permits(), 0);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(service.graph.lock().graph.connected(7, 7), Ok(true));
    }
    #[test]
    fn decimal_version_carries_without_wrapping_or_losing_precision() {
        for (before, after) in [
            ("9", "10"),
            ("99", "100"),
            ("9007199254740992", "9007199254740993"),
            ("18446744073709551615", "18446744073709551616"),
            (
                "9999999999999999999999999999999999999999",
                "10000000000000000000000000000000000000000",
            ),
        ] {
            let mut state = GraphState {
                graph: ConnectivityGraph::default(),
                failed: false,
                version: before.bytes().rev().map(|b| b - b'0').collect(),
            };
            state.apply(add(1));
            assert_eq!(state.snapshot().state_version, after);
            state.apply(add(1));
            assert_eq!(state.snapshot().state_version, after);
        }
    }

    #[tokio::test]
    async fn stats_admission_does_not_block_info_and_worker_failure_fails_closed() {
        use axum::{
            Router,
            body::{Body, to_bytes},
            http::{Request, StatusCode},
            routing::get,
        };
        use tower::ServiceExt;
        let service = Service::new(ConnectivityGraph::default(), 1);
        let app = Router::new()
            .route("/v1/stats", get(crate::api::stats))
            .route("/v1/info", get(crate::api::info))
            .with_state(service.clone());
        let permit = service.permits.clone().acquire_owned().await.unwrap();
        let response = app
            .clone()
            .oneshot(Request::get("/v1/stats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "busy");
        let info = app
            .clone()
            .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(info.status(), StatusCode::OK);
        drop(permit);
        assert_eq!(service.stats().await.unwrap().state_version, "0");
        assert!(
            service
                .run(|_| {
                    panic!("test worker panic");
                })
                .await
                .is_err()
        );
        let response = app
            .clone()
            .oneshot(Request::get("/v1/stats").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "unavailable");
        let response = app
            .oneshot(Request::get("/v1/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(service.permits.available_permits(), 1);
        assert_eq!(service.info().engine, "compact-bfs");
    }

    #[tokio::test]
    async fn panicked_writer_rejects_queued_and_future_work() {
        let service = Service::new(ConnectivityGraph::default(), 2);
        let writer = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            writer
                .run(move |state| {
                    state.apply(add(1));
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("panic after partial mutation");
                })
                .await
        });
        started_rx.await.unwrap();
        let reader = service.clone();
        let queued = tokio::spawn(async move { reader.stats().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Dropping the requester must not disable the worker's failure tracking.
        job.abort();
        let _ = job.await;
        release_tx.send(()).unwrap();
        assert!(matches!(
            queued.await.unwrap(),
            Err(ServiceError::Unavailable)
        ));
        assert!(matches!(
            service.execute(vec![add(2)]).await,
            Err(ServiceError::Unavailable)
        ));
        assert!(matches!(
            service.stats().await,
            Err(ServiceError::Unavailable)
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(service.graph.lock().graph.node_count(), 1);
        assert_eq!(service.info().engine, "compact-bfs");
    }

    #[tokio::test]
    async fn admitted_stats_wait_for_the_entire_locked_job_and_retain_permit_on_cancel() {
        let service = Service::new(ConnectivityGraph::default(), 2);
        let writer = service.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            writer
                .run(move |state| {
                    state.apply(add(1));
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    state.apply(add(2));
                })
                .await
        });
        started_rx.await.unwrap();
        let reader = service.clone();
        let read = tokio::spawn(async move { reader.stats().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!read.is_finished()); // The snapshot cannot observe the first change alone.
        read.abort();
        let _ = read.await;
        assert_eq!(service.permits.available_permits(), 0);
        release_tx.send(()).unwrap();
        job.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while service.permits.available_permits() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = service.stats().await.unwrap();
        assert_eq!(snapshot.node_count, 2);
        assert_eq!(snapshot.state_version, "2");
    }
}
