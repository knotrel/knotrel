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
    let app = knotrel_server::router_with_config(config);
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(
        address = %listener.local_addr()?, engine = engine.as_str(),
        experimental = engine.is_experimental(), max_pending_jobs,
        max_nodes = ?graph_limits.max_nodes, max_edges = ?graph_limits.max_edges,
        "Knotrel listening; graph state is in memory"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Stops accepting connections on Ctrl-C or Unix SIGTERM and drains requests.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to listen for Ctrl-C; shutting down");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = terminate => {} }
    tracing::info!("shutdown requested");
}
