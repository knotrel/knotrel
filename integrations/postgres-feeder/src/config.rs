//! Configuration from environment variables.
//!
//! Every setting is an environment variable, so the feeder fits containers
//! and secret files. The password comes from `KNOTREL_PG_PASSWORD_FILE`
//! (preferred) or `KNOTREL_PG_PASSWORD`, never from the command line.

use crate::mapping::TableNames;
use crate::position::is_valid_slot_name;
use crate::postgres::PgParams;
use std::time::Duration;

/// Feeder settings.
#[derive(Clone, Debug)]
pub struct Config {
    /// PostgreSQL connection (both connections use it).
    pub pg: PgParams,
    /// Publication of the two graph tables.
    pub publication: String,
    /// Graph table names.
    pub tables: TableNames,
    /// Prefix reserved for this feeder's slots.
    pub slot_prefix: String,
    /// Base URL of the Knotrel server.
    pub knotrel_url: String,
    /// Feeder lease requested from Knotrel.
    pub lease: Duration,
    /// Maximum events per Knotrel request (1 to 1024).
    pub max_batch: usize,
    /// Idle time of the stream after which the feeder considers itself caught
    /// up and declares a barrier.
    pub catch_up_idle: Duration,
    /// Interval of the slot retention check.
    pub slot_check_interval: Duration,
    /// Accept `max_slot_wal_keep_size = -1`.
    pub allow_unbounded_wal: bool,
    /// Timeout of a Knotrel request (import excluded).
    pub request_timeout: Duration,
    /// Timeout of a Knotrel import.
    pub import_timeout: Duration,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn parse<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match var(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name}={v:?} is not a valid value")),
    }
}

fn identifier(name: &str, default: &str) -> Result<String, String> {
    let value = var(name).unwrap_or_else(|| default.to_string());
    if value.is_empty() || value.len() > 63 || value.contains('\0') {
        return Err(format!(
            "{name}={value:?} is not a valid PostgreSQL identifier"
        ));
    }
    Ok(value)
}

impl Config {
    /// Reads the configuration from the environment.
    ///
    /// # Errors
    ///
    /// A message naming the variable that is missing or invalid.
    pub fn from_env() -> Result<Self, String> {
        let password = match (var("KNOTREL_PG_PASSWORD_FILE"), var("KNOTREL_PG_PASSWORD")) {
            (Some(path), _) => std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read KNOTREL_PG_PASSWORD_FILE: {e}"))?
                .trim_end_matches(['\r', '\n'])
                .to_string(),
            (None, Some(p)) => p,
            (None, None) => String::new(),
        };
        let pg = PgParams {
            host: var("KNOTREL_PG_HOST").unwrap_or_else(|| "127.0.0.1".into()),
            port: parse("KNOTREL_PG_PORT", 5432)?,
            user: var("KNOTREL_PG_USER").ok_or("KNOTREL_PG_USER is required")?,
            password,
            database: var("KNOTREL_PG_DATABASE").ok_or("KNOTREL_PG_DATABASE is required")?,
        };
        let slot_prefix = var("KNOTREL_PG_SLOT_PREFIX").unwrap_or_else(|| "knotrel_feeder".into());
        // The slot name is `<prefix>_<12 hex digits>`.
        if !is_valid_slot_name(&slot_prefix) || slot_prefix.len() > 50 {
            return Err(format!(
                "KNOTREL_PG_SLOT_PREFIX={slot_prefix:?}: use 1 to 50 characters from [a-z0-9_]"
            ));
        }
        let max_batch = parse("KNOTREL_FEEDER_MAX_BATCH", 1024_usize)?;
        if !(1..=1024).contains(&max_batch) {
            return Err("KNOTREL_FEEDER_MAX_BATCH must be from 1 to 1024".into());
        }
        let lease_ms = parse("KNOTREL_FEEDER_LEASE_MS", 10_000_u64)?;
        if !(1_000..=60_000).contains(&lease_ms) {
            return Err("KNOTREL_FEEDER_LEASE_MS must be from 1000 to 60000".into());
        }
        Ok(Self {
            pg,
            publication: identifier("KNOTREL_PG_PUBLICATION", "knotrel_graph")?,
            tables: TableNames {
                schema: identifier("KNOTREL_PG_SCHEMA", "knotrel_graph")?,
                nodes: identifier("KNOTREL_PG_NODES_TABLE", "nodes")?,
                edges: identifier("KNOTREL_PG_EDGES_TABLE", "edges")?,
            },
            slot_prefix,
            knotrel_url: var("KNOTREL_URL").unwrap_or_else(|| "http://127.0.0.1:8080".into()),
            lease: Duration::from_millis(lease_ms),
            max_batch,
            catch_up_idle: Duration::from_millis(parse("KNOTREL_FEEDER_CATCH_UP_IDLE_MS", 500)?),
            slot_check_interval: Duration::from_secs(parse("KNOTREL_FEEDER_SLOT_CHECK_SECS", 30)?),
            allow_unbounded_wal: parse("KNOTREL_FEEDER_ALLOW_UNBOUNDED_WAL", false)?,
            request_timeout: Duration::from_millis(parse(
                "KNOTREL_FEEDER_REQUEST_TIMEOUT_MS",
                10_000,
            )?),
            import_timeout: Duration::from_millis(parse(
                "KNOTREL_FEEDER_IMPORT_TIMEOUT_MS",
                300_000,
            )?),
        })
    }
}
