//! `knotrel-pg-feeder`: the reference PostgreSQL feeder.
//!
//! ```text
//! knotrel-pg-feeder [run]      synchronize Knotrel with PostgreSQL (default)
//! knotrel-pg-feeder check      run the preflight checks and exit
//! knotrel-pg-feeder drop-slots drop this feeder's inactive replication slots
//! ```
//!
//! Configuration is read from environment variables; see the README.

use knotrel_pg_feeder::config::Config;
use knotrel_pg_feeder::feeder::{Fatal, Feeder, preflight};
use knotrel_pg_feeder::postgres::Control;
use tokio::sync::watch;

const USAGE: &str = "usage: knotrel-pg-feeder [run | check | drop-slots]
configuration: environment variables (see integrations/postgres-feeder/README.md)";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    let result = runtime.block_on(async move {
        let cfg = Config::from_env().map_err(Fatal::Config)?;
        match command.as_str() {
            "run" => {
                let (tx, rx) = watch::channel(false);
                tokio::spawn(async move {
                    wait_for_signal().await;
                    let _ = tx.send(true);
                });
                Feeder::new(cfg).await?.run(rx).await
            }
            "check" => {
                preflight(&cfg).await?;
                println!("preflight passed");
                Ok(())
            }
            "drop-slots" => {
                let control = Control::connect(&cfg.pg)
                    .await
                    .map_err(|e| Fatal::Config(e.to_string()))?;
                let dropped = control
                    .drop_own_slots(&cfg.slot_prefix, None)
                    .await
                    .map_err(|e| Fatal::Other(e.to_string()))?;
                println!("dropped {} slot(s): {}", dropped.len(), dropped.join(", "));
                Ok(())
            }
            other => Err(Fatal::Config(format!("unknown command {other:?}\n{USAGE}"))),
        }
    });
    if let Err(fatal) = result {
        tracing::error!(exit_code = fatal.exit_code(), "{fatal}");
        std::process::exit(fatal.exit_code());
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
