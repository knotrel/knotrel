//! Accept loop with bounded connections, timeouts and bounded graceful drain.

use crate::{Lifecycle, ServerConfig};
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
};

/// How a call to [`serve`] ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownOutcome {
    /// Every admitted connection and graph worker job finished within the shutdown timeout.
    Drained,
    /// The shutdown timeout elapsed before all admitted work finished.
    ///
    /// [`serve`] reports that the drain deadline has expired. It aborts open network
    /// connection tasks, but it does **not** forcibly terminate running `spawn_blocking`
    /// graph workers, which run on Tokio's blocking threadpool until completion.
    ///
    /// The standalone executable handles this outcome by calling `std::process::exit(2)`,
    /// which terminates the process. Embedded callers of [`serve`] must handle this
    /// outcome explicitly and understand that outstanding graph workers may still be
    /// running on the threadpool.
    DeadlineExceeded {
        /// Number of connections still open plus graph jobs still running when the deadline passed.
        abandoned: usize,
    },
}

// Static so rejecting an over-limit connection allocates nothing.
const REJECT_CONNECTION: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
content-type: application/json\r\n\
connection: close\r\n\
content-length: 85\r\n\
\r\n\
{\"result\":\"error\",\"code\":\"too_many_connections\",\"message\":\"connection limit reached\"}";

const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// Serves `app` on `listener` until `shutdown` completes, then drains.
///
/// The lifecycle is marked [`crate::Phase::Ready`] before the first accept.
/// At most [`ServerConfig::max_connections`] connections are served; extra
/// connections receive a best-effort `503` and are closed. Header reads (also
/// the keep-alive idle wait) are bounded by
/// [`ServerConfig::header_read_timeout`].
///
/// When `shutdown` completes the lifecycle moves to [`crate::Phase::Draining`].
/// Idle connections are closed, connections with an admitted request finish
/// that request with `Connection: close`, and connections opened during the
/// drain are answered once (probes normally, other routes with `503`
/// `shutting_down`). The call returns [`ShutdownOutcome::Drained`] as soon as
/// no connection remains and all outstanding graph worker jobs have finished.
/// If [`ServerConfig::shutdown_timeout`] elapses first, it returns
/// [`ShutdownOutcome::DeadlineExceeded`] reporting the number of open connections
/// and unfinished graph jobs. Network connection tasks are aborted, but running
/// `spawn_blocking` graph workers cannot be cancelled by `serve()` and run until
/// completion. The standalone binary calls `std::process::exit(2)` upon
/// `DeadlineExceeded`; embedded callers must handle this outcome explicitly.
pub async fn serve(
    listener: TcpListener,
    app: axum::Router,
    config: &ServerConfig,
    lifecycle: &Lifecycle,
    shutdown: impl Future<Output = ()>,
) -> ShutdownOutcome {
    let permits = Arc::new(Semaphore::new(config.max_connections()));
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut tasks: JoinSet<()> = JoinSet::new();
    let header_read_timeout = config.header_read_timeout();
    let mut deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let mut jobs_rx = lifecycle.jobs_receiver();
    let mut jobs_closed = false;
    tokio::pin!(shutdown);
    lifecycle.mark_ready();
    let mut draining = false;
    loop {
        let active_jobs = *jobs_rx.borrow_and_update();
        if draining && tasks.is_empty() && active_jobs == 0 {
            return ShutdownOutcome::Drained;
        }

        #[cfg(feature = "test-support")]
        lifecycle.check_drain_loop_hook(draining, tasks.is_empty(), active_jobs);

        tokio::select! {
            biased;
            () = &mut shutdown, if !draining => {
                draining = true;
                lifecycle.begin_draining();
                // A send error only means no connection is listening.
                let _ = drain_tx.send(true);
                deadline = Some(Box::pin(tokio::time::sleep(config.shutdown_timeout())));
                tracing::info!(
                    open_connections = tasks.len(),
                    outstanding_jobs = *jobs_rx.borrow(),
                    timeout_ms = config.shutdown_timeout().as_millis(),
                    "draining"
                );
            }
            Some(finished) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = finished {
                    tracing::error!(panicked = error.is_panic(), "connection task failed");
                }
            }
            jobs_changed = jobs_rx.changed(), if draining && !jobs_closed => {
                if jobs_changed.is_err() {
                    jobs_closed = true;
                }
            }
            () = async {
                match deadline.as_mut() {
                    Some(sleep) => sleep.await,
                    None => std::future::pending().await,
                }
            } => {
                let abandoned = tasks.len() + *jobs_rx.borrow_and_update();
                if abandoned == 0 {
                    return ShutdownOutcome::Drained;
                }
                tasks.abort_all();
                return ShutdownOutcome::DeadlineExceeded { abandoned };
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => {
                    admit(
                        stream,
                        &permits,
                        &mut tasks,
                        &app,
                        header_read_timeout,
                        (!draining).then(|| drain_rx.clone()),
                    );
                }
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "accept failed; backing off");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                }
            },
        }
    }
}

fn admit(
    stream: TcpStream,
    permits: &Arc<Semaphore>,
    tasks: &mut JoinSet<()>,
    app: &axum::Router,
    header_read_timeout: Duration,
    drain: Option<watch::Receiver<bool>>,
) {
    let Ok(permit) = permits.clone().try_acquire_owned() else {
        tracing::warn!("connection limit reached; rejecting connection");
        // Best effort on the raw socket: a fresh socket has an empty send buffer, so the
        // write neither blocks nor needs a readiness event from the reactor.
        if let Ok(mut raw) = stream.into_std() {
            let _ = std::io::Write::write(&mut raw, REJECT_CONNECTION);
        }
        return;
    };
    let app = app.clone();
    tasks.spawn(async move {
        let _permit = permit;
        let connection = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(header_read_timeout)
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app));
        tokio::pin!(connection);
        let result = match drain {
            Some(mut drain) => {
                tokio::select! {
                    result = connection.as_mut() => result,
                    () = async { let _ = drain.wait_for(|draining| *draining).await; } => {
                        connection.as_mut().graceful_shutdown();
                        connection.await
                    }
                }
            }
            None => connection.await,
        };
        if let Err(error) = result {
            // Client-side aborts and timeouts are routine; keep the detail at debug.
            tracing::debug!(%error, "connection ended with an error");
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejection_body_length_matches_content_length() {
        let text = std::str::from_utf8(super::REJECT_CONNECTION).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("content-length: {}", body.len())));
    }
}
