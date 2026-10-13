//! Standalone in-memory Knotrel HTTP server.

use std::error::Error;
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json_format = std::env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json_format {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .try_init()?;
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .try_init()?;
    }
    let config = knotrel_server::ServerConfig::from_env()?;
    let address = config.address();
    let engine = config.engine();
    let max_pending_jobs = config.max_pending_jobs();
    let graph_limits = config.graph_limits();
    let exposed = config.is_exposed();
    let lifecycle = knotrel_server::Lifecycle::new();
    let app = knotrel_server::router_with_lifecycle(config.clone(), lifecycle.clone());
    // Installed before binding so a signal during startup is never fatal-by-default.
    let signals = Signals::install()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    let (stop_sender, stop_receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(forward_signals(signals, stop_sender));
    if exposed {
        tracing::warn!(
            "listening on a non-loopback address; v0.1.0 has no authentication, \
             authorization or TLS and is unsupported for untrusted clients"
        );
    }
    tracing::info!(
        address = %listener.local_addr()?, engine = engine.as_str(),
        experimental = engine.is_experimental(), max_pending_jobs,
        max_nodes = ?graph_limits.max_nodes, max_edges = ?graph_limits.max_edges,
        max_connections = config.max_connections(),
        header_read_timeout_ms = config.header_read_timeout().as_millis(),
        request_timeout_ms = config.request_timeout().as_millis(),
        shutdown_timeout_ms = config.shutdown_timeout().as_millis(),
        "Knotrel listening; graph state is in memory"
    );
    let outcome = knotrel_server::serve(listener, app, &config, &lifecycle, async {
        // A dropped sender means signal handling is gone; keep serving.
        if stop_receiver.await.is_err() {
            std::future::pending::<()>().await;
        }
    })
    .await;
    match outcome {
        knotrel_server::ShutdownOutcome::Drained => {
            tracing::info!("shutdown complete");
            Ok(())
        }
        knotrel_server::ShutdownOutcome::DeadlineExceeded { abandoned } => {
            tracing::error!(abandoned, "shutdown deadline exceeded; exiting");
            // Worker threads may still be running a graph job; do not wait for them.
            std::process::exit(EXIT_DEADLINE_EXCEEDED);
        }
    }
}

/// Exit status when the drain deadline passes with connections still open.
const EXIT_DEADLINE_EXCEEDED: i32 = 2;
/// Exit status when a second signal forces an immediate stop (128 + SIGINT).
const EXIT_FORCED: i32 = 130;

/// Reports the first SIGINT/SIGTERM through `stop`; a second one exits at once.
async fn forward_signals(mut signals: Signals, stop: tokio::sync::oneshot::Sender<()>) {
    signals.next().await;
    tracing::info!("shutdown requested");
    // The receiver only disappears once serving has ended.
    let _ = stop.send(());
    signals.next().await;
    tracing::warn!("second shutdown signal; exiting immediately");
    std::process::exit(EXIT_FORCED);
}

#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn next(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
    }
}

#[cfg(not(unix))]
struct Signals;

#[cfg(not(unix))]
impl Signals {
    fn install() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn next(&mut self) {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
