//! Bounded CPU work and exclusive graph ownership for each request.

use crate::api::{Operation, Outcome};
use knotrel_core::Graph;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Shared graph state plus a strict limit on outstanding worker jobs.
#[derive(Clone)]
pub(crate) struct Service {
    graph: Arc<Mutex<Graph>>,
    permits: Arc<Semaphore>,
}

/// Admission failure or a worker that cannot safely use the graph.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServiceError {
    Busy,
    Unavailable,
}

impl Service {
    /// Creates the service; zero capacity intentionally rejects every request.
    pub(crate) fn new(graph: Graph, capacity: usize) -> Self {
        Self {
            graph: Arc::new(Mutex::new(graph)),
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    /// Executes a request on a blocking worker, preserving batch isolation.
    ///
    /// Admission is nonblocking: rejected work never enters Tokio's blocking
    /// queue. Moving the permit into the closure bounds queued plus running
    /// jobs even if the caller drops its future. The mutex is acquired only
    /// inside that closure, keeping traversal and lock contention off async
    /// executor threads. One guard spans the entire batch, so no other request
    /// can observe its intermediate states. Per-operation errors do not roll
    /// back earlier updates. A poisoned mutex fails closed.
    pub(crate) async fn execute(
        &self,
        operations: Vec<Operation>,
    ) -> Result<Vec<Outcome>, ServiceError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ServiceError::Busy)?;
        let graph = self.graph.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut graph = graph.lock().map_err(|_| ServiceError::Unavailable)?;
            Ok(operations
                .into_iter()
                .map(|operation| operation.apply(&mut graph))
                .collect())
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "graph worker failed");
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
        let service = Service::new(Graph::new(), 1);
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
        let service = Service::new(Graph::new(), 1);
        let graph = service.graph.clone();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        // Hold the graph from a dedicated thread so the worker stays pending
        // without blocking the async test executor or depending on a large graph.
        let holder = std::thread::spawn(move || {
            let _guard = graph.lock().unwrap();
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
        assert_eq!(service.graph.lock().unwrap().connected(7, 7), Ok(true));
    }
}
