//! Startup-only configuration loading and validation.

use knotrel_core::{EngineConfig, GraphLimits};
use std::{
    collections::BTreeMap, error::Error, ffi::OsString, fmt, net::SocketAddr, time::Duration,
};

/// Operating mode of the Knotrel server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerMode {
    /// Standalone in-memory mode without external feeder synchronization.
    #[default]
    Standalone,
    /// Integration mode where an authoritative external feeder coordinates synchronization.
    SourceManaged,
}

/// Validated standalone server settings.
///
/// The environment is read only by [`Self::from_env`]. Embedded callers can use
/// [`Self::new`]. Settings are fixed when the router is constructed; the address
/// is used by the standalone executable, not by the router itself.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    address: SocketAddr,
    engine: EngineConfig,
    max_pending_jobs: usize,
    graph_limits: GraphLimits,
    max_connections: usize,
    header_read_timeout: Duration,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    mode: ServerMode,
    max_import_body_bytes: usize,
    dedup_window_size: usize,
}

/// A malformed or unsupported startup setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConfigError {}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            engine: EngineConfig::default(),
            max_pending_jobs: 32,
            graph_limits: Self::DEFAULT_GRAPH_LIMITS,
            max_connections: Self::DEFAULT_MAX_CONNECTIONS,
            header_read_timeout: Self::DEFAULT_HEADER_READ_TIMEOUT,
            request_timeout: Self::DEFAULT_REQUEST_TIMEOUT,
            shutdown_timeout: Self::DEFAULT_SHUTDOWN_TIMEOUT,
            mode: ServerMode::default(),
            max_import_body_bytes: Self::DEFAULT_MAX_IMPORT_BODY_BYTES,
            dedup_window_size: Self::DEFAULT_DEDUP_WINDOW_SIZE,
        }
    }
}

impl ServerConfig {
    /// Default ceiling on registered vertices for configured servers.
    pub const DEFAULT_MAX_NODES: usize = 100_000;
    /// Default ceiling on live edges for configured servers.
    pub const DEFAULT_MAX_EDGES: usize = 500_000;
    /// Default ceiling on simultaneously open client connections.
    pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;
    /// Largest accepted connection ceiling.
    pub const MAX_CONNECTIONS_LIMIT: usize = 1_000_000;
    /// Default maximum payload size for graph import (64 MiB).
    pub const DEFAULT_MAX_IMPORT_BODY_BYTES: usize = 64 * 1024 * 1024;
    /// Largest accepted import payload ceiling (512 MiB).
    pub const MAX_IMPORT_BODY_LIMIT: usize = 512 * 1024 * 1024;
    /// Default capacity of bounded deduplication window.
    pub const DEFAULT_DEDUP_WINDOW_SIZE: usize = 10_000;
    /// Largest accepted deduplication window capacity.
    pub const MAX_DEDUP_WINDOW_SIZE: usize = 1_000_000;
    /// Default time a client may take to send request headers or stay idle between requests.
    pub const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// Default time allowed to receive a request body and produce its response.
    pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
    /// Default time admitted requests may take to finish after shutdown begins.
    pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
    /// Largest accepted timeout; timeouts must be positive so every wait is bounded.
    pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);
    /// Graph limits applied by [`Self::default`], [`Self::new`] and [`Self::from_env`].
    pub const DEFAULT_GRAPH_LIMITS: GraphLimits = GraphLimits {
        max_nodes: Some(Self::DEFAULT_MAX_NODES),
        max_edges: Some(Self::DEFAULT_MAX_EDGES),
    };

    /// Validates explicit settings without reading the environment.
    ///
    /// Returns [`ConfigError`] if `address` is not a loopback address (use
    /// [`Self::new_allowing_non_loopback`] for deliberate exposure), or if
    /// `max_pending_jobs` is zero or exceeds
    /// [`tokio::sync::Semaphore::MAX_PERMITS`]. The limit covers queued plus
    /// running graph jobs, not parallel access to the graph. Graph limits start
    /// at [`Self::DEFAULT_GRAPH_LIMITS`].
    pub fn new(
        address: SocketAddr,
        engine: EngineConfig,
        max_pending_jobs: usize,
    ) -> Result<Self, ConfigError> {
        Self::build(address, false, engine, max_pending_jobs)
    }

    /// Like [`Self::new`], but explicitly accepts a non-loopback listener address.
    ///
    /// Knotrel v0.1.0 has no authentication, authorization or TLS; see
    /// `docs/http-api.md` before using this.
    pub fn new_allowing_non_loopback(
        address: SocketAddr,
        engine: EngineConfig,
        max_pending_jobs: usize,
    ) -> Result<Self, ConfigError> {
        Self::build(address, true, engine, max_pending_jobs)
    }

    /// Reports whether the listener address is outside the loopback interface.
    pub fn is_exposed(&self) -> bool {
        !self.address.ip().is_loopback()
    }

    fn build(
        address: SocketAddr,
        allow_non_loopback: bool,
        engine: EngineConfig,
        max_pending_jobs: usize,
    ) -> Result<Self, ConfigError> {
        if !allow_non_loopback && !address.ip().is_loopback() {
            return Err(ConfigError(
                "KNOTREL_ADDR must be a loopback address unless KNOTREL_ALLOW_NON_LOOPBACK=true"
                    .into(),
            ));
        }
        if max_pending_jobs == 0 || max_pending_jobs > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(ConfigError(format!(
                "KNOTREL_MAX_PENDING_JOBS must be between 1 and {}",
                tokio::sync::Semaphore::MAX_PERMITS
            )));
        }
        Ok(Self {
            address,
            engine,
            max_pending_jobs,
            graph_limits: Self::DEFAULT_GRAPH_LIMITS,
            max_connections: Self::DEFAULT_MAX_CONNECTIONS,
            header_read_timeout: Self::DEFAULT_HEADER_READ_TIMEOUT,
            request_timeout: Self::DEFAULT_REQUEST_TIMEOUT,
            shutdown_timeout: Self::DEFAULT_SHUTDOWN_TIMEOUT,
            mode: ServerMode::default(),
            max_import_body_bytes: Self::DEFAULT_MAX_IMPORT_BODY_BYTES,
            dedup_window_size: Self::DEFAULT_DEDUP_WINDOW_SIZE,
        })
    }

    /// Sets the operational mode (standalone or source-managed).
    pub fn with_mode(mut self, mode: ServerMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the maximum payload size accepted for graph import.
    pub fn with_max_import_body_bytes(mut self, bytes: usize) -> Result<Self, ConfigError> {
        self.max_import_body_bytes = check_import_body_bytes(bytes)?;
        Ok(self)
    }

    /// Sets the bounded deduplication window size.
    pub fn with_dedup_window_size(mut self, size: usize) -> Result<Self, ConfigError> {
        self.dedup_window_size = check_dedup_window_size(size)?;
        Ok(self)
    }

    /// Returns the operating mode of the server.
    pub fn mode(&self) -> ServerMode {
        self.mode
    }

    /// Returns the maximum allowed size in bytes for a graph import payload.
    pub fn max_import_body_bytes(&self) -> usize {
        self.max_import_body_bytes
    }

    /// Returns the capacity of the bounded deduplication window.
    pub fn dedup_window_size(&self) -> usize {
        self.dedup_window_size
    }

    /// Replaces the open-connection ceiling.
    ///
    /// Returns [`ConfigError`] unless `limit` is in `1..=`[`Self::MAX_CONNECTIONS_LIMIT`].
    pub fn with_max_connections(mut self, limit: usize) -> Result<Self, ConfigError> {
        self.max_connections = check_connections(limit)?;
        Ok(self)
    }

    /// Replaces the header-read timeout, which also bounds keep-alive idle time.
    ///
    /// Returns [`ConfigError`] unless the value is positive and at most [`Self::MAX_TIMEOUT`].
    pub fn with_header_read_timeout(mut self, timeout: Duration) -> Result<Self, ConfigError> {
        self.header_read_timeout = check_timeout("KNOTREL_HEADER_READ_TIMEOUT_MS", timeout)?;
        Ok(self)
    }

    /// Replaces the per-request timeout covering body receipt and handling.
    ///
    /// Returns [`ConfigError`] unless the value is positive and at most [`Self::MAX_TIMEOUT`].
    pub fn with_request_timeout(mut self, timeout: Duration) -> Result<Self, ConfigError> {
        self.request_timeout = check_timeout("KNOTREL_REQUEST_TIMEOUT_MS", timeout)?;
        Ok(self)
    }

    /// Replaces the bounded drain period used after a shutdown request.
    ///
    /// Returns [`ConfigError`] unless the value is positive and at most [`Self::MAX_TIMEOUT`].
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Result<Self, ConfigError> {
        self.shutdown_timeout = check_timeout("KNOTREL_SHUTDOWN_TIMEOUT_MS", timeout)?;
        Ok(self)
    }

    /// Returns the maximum number of simultaneously served connections.
    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    /// Returns how long a client may take to send headers or remain idle.
    pub fn header_read_timeout(&self) -> Duration {
        self.header_read_timeout
    }

    /// Returns the limit on receiving a request body and producing its response.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Returns the drain deadline applied after a shutdown request.
    pub fn shutdown_timeout(&self) -> Duration {
        self.shutdown_timeout
    }

    /// Replaces the immutable graph cardinality limits; zero is valid, None is unlimited.
    pub fn with_graph_limits(mut self, limits: GraphLimits) -> Self {
        self.graph_limits = limits;
        self
    }

    /// Returns the cardinality ceilings for this server's graph instance.
    pub fn graph_limits(&self) -> GraphLimits {
        self.graph_limits
    }

    /// Loads and validates a snapshot of the process environment.
    ///
    /// Recognized variables are `KNOTREL_ADDR` (default `127.0.0.1:8080`),
    /// `KNOTREL_ENGINE` (`compact-bfs` by default, or experimental `ett` / `hdt`) and
    /// `KNOTREL_MAX_PENDING_JOBS` (default `32`). `KNOTREL_MAX_NODES` and
    /// `KNOTREL_MAX_EDGES` are nonnegative decimal integers or `unlimited`; absent
    /// means [`Self::DEFAULT_MAX_NODES`] / [`Self::DEFAULT_MAX_EDGES`], zero
    /// forbids new entries. A non-loopback `KNOTREL_ADDR` is rejected unless
    /// `KNOTREL_ALLOW_NON_LOOPBACK` is exactly `true` (`false` is also accepted).
    ///
    /// `KNOTREL_MAX_CONNECTIONS` (default 1024) is a positive integer.
    /// `KNOTREL_HEADER_READ_TIMEOUT_MS` (5000), `KNOTREL_REQUEST_TIMEOUT_MS` (30000) and
    /// `KNOTREL_SHUTDOWN_TIMEOUT_MS` (10000) are positive millisecond counts of at most
    /// one hour.
    ///
    /// Returns [`ConfigError`] for unknown `KNOTREL_` keys, non-Unicode names
    /// or values in that namespace, unsupported engines and malformed values.
    /// Unrelated environment variables are ignored. There is no hot reload.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_variables(std::env::vars_os())
    }

    /// Returns the socket address requested for the standalone listener.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the implementation selected for the empty graph at startup.
    pub fn engine(&self) -> EngineConfig {
        self.engine
    }

    /// Returns the maximum number of admitted queued plus running graph jobs.
    pub fn max_pending_jobs(&self) -> usize {
        self.max_pending_jobs
    }

    // Snapshot first: validation never depends on repeated environment reads.
    fn from_variables(
        variables: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, ConfigError> {
        let mut settings = BTreeMap::new();
        for (name, value) in variables {
            if !name.to_string_lossy().starts_with("KNOTREL_") {
                continue;
            }
            let name = name
                .into_string()
                .map_err(|_| ConfigError("KNOTREL_ setting name must be valid Unicode".into()))?;
            if !matches!(
                name.as_str(),
                "KNOTREL_ADDR"
                    | "KNOTREL_ENGINE"
                    | "KNOTREL_MAX_PENDING_JOBS"
                    | "KNOTREL_MAX_NODES"
                    | "KNOTREL_MAX_EDGES"
                    | "KNOTREL_ALLOW_NON_LOOPBACK"
                    | "KNOTREL_MAX_CONNECTIONS"
                    | "KNOTREL_HEADER_READ_TIMEOUT_MS"
                    | "KNOTREL_REQUEST_TIMEOUT_MS"
                    | "KNOTREL_SHUTDOWN_TIMEOUT_MS"
                    | "KNOTREL_MODE"
                    | "KNOTREL_MAX_IMPORT_BODY_BYTES"
                    | "KNOTREL_DEDUP_WINDOW_SIZE"
            ) {
                return Err(ConfigError(format!("unknown setting: {name}")));
            }
            let value = value
                .into_string()
                .map_err(|_| ConfigError(format!("{name} must be valid Unicode")))?;
            settings.insert(name, value);
        }
        let defaults = Self::default();
        let address = match settings.get("KNOTREL_ADDR") {
            Some(value) => value.parse().map_err(|_| {
                ConfigError(
                    "KNOTREL_ADDR must be an IP socket address such as 127.0.0.1:8080".into(),
                )
            })?,
            None => defaults.address,
        };
        let allow_non_loopback = match settings
            .get("KNOTREL_ALLOW_NON_LOOPBACK")
            .map(String::as_str)
        {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => {
                return Err(ConfigError(
                    "KNOTREL_ALLOW_NON_LOOPBACK must be true or false".into(),
                ));
            }
        };
        let engine = match settings.get("KNOTREL_ENGINE").map(String::as_str) {
            None | Some("compact-bfs") => EngineConfig::CompactBfs,
            Some("ett") => EngineConfig::EulerTour,
            Some("hdt") => EngineConfig::Hdt,
            Some(_) => {
                return Err(ConfigError(
                    "KNOTREL_ENGINE must be compact-bfs, ett or hdt".into(),
                ));
            }
        };
        let mode = match settings.get("KNOTREL_MODE").map(String::as_str) {
            None | Some("standalone") => ServerMode::Standalone,
            Some("source_managed") | Some("source-managed") => ServerMode::SourceManaged,
            Some(_) => {
                return Err(ConfigError(
                    "KNOTREL_MODE must be standalone or source_managed".into(),
                ));
            }
        };
        let max_import_body_bytes = match settings.get("KNOTREL_MAX_IMPORT_BODY_BYTES") {
            Some(value) => parse_decimal("KNOTREL_MAX_IMPORT_BODY_BYTES", value)?,
            None => defaults.max_import_body_bytes,
        };
        let dedup_window_size = match settings.get("KNOTREL_DEDUP_WINDOW_SIZE") {
            Some(value) => parse_decimal("KNOTREL_DEDUP_WINDOW_SIZE", value)?,
            None => defaults.dedup_window_size,
        };
        let max_pending_jobs = match settings.get("KNOTREL_MAX_PENDING_JOBS") {
            Some(value) => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(ConfigError(
                        "KNOTREL_MAX_PENDING_JOBS must be a positive decimal integer".into(),
                    ));
                }
                value
                    .parse()
                    .map_err(|_| ConfigError("KNOTREL_MAX_PENDING_JOBS is out of range".into()))?
            }
            None => defaults.max_pending_jobs,
        };
        let parse_limit =
            |key: &str, default: Option<usize>| -> Result<Option<usize>, ConfigError> {
                settings.get(key).map_or(Ok(default), |value| {
                    if value == "unlimited" {
                        return Ok(None);
                    }
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(ConfigError(format!(
                            "{key} must be a nonnegative decimal integer or unlimited"
                        )));
                    }
                    value
                        .parse()
                        .map(Some)
                        .map_err(|_| ConfigError(format!("{key} is out of range")))
                })
            };
        let limits = GraphLimits {
            max_nodes: parse_limit("KNOTREL_MAX_NODES", Self::DEFAULT_GRAPH_LIMITS.max_nodes)?,
            max_edges: parse_limit("KNOTREL_MAX_EDGES", Self::DEFAULT_GRAPH_LIMITS.max_edges)?,
        };
        let max_connections = match settings.get("KNOTREL_MAX_CONNECTIONS") {
            Some(value) => parse_decimal("KNOTREL_MAX_CONNECTIONS", value)?,
            None => defaults.max_connections,
        };
        let timeout = |key: &str, default: Duration| -> Result<Duration, ConfigError> {
            match settings.get(key) {
                Some(value) => Ok(Duration::from_millis(
                    parse_decimal(key, value)?
                        .try_into()
                        .map_err(|_| ConfigError(format!("{key} is out of range")))?,
                )),
                None => Ok(default),
            }
        };
        Self::build(address, allow_non_loopback, engine, max_pending_jobs)?
            .with_mode(mode)
            .with_max_import_body_bytes(max_import_body_bytes)?
            .with_dedup_window_size(dedup_window_size)?
            .with_graph_limits(limits)
            .with_max_connections(max_connections)?
            .with_header_read_timeout(timeout(
                "KNOTREL_HEADER_READ_TIMEOUT_MS",
                defaults.header_read_timeout,
            )?)?
            .with_request_timeout(timeout(
                "KNOTREL_REQUEST_TIMEOUT_MS",
                defaults.request_timeout,
            )?)?
            .with_shutdown_timeout(timeout(
                "KNOTREL_SHUTDOWN_TIMEOUT_MS",
                defaults.shutdown_timeout,
            )?)
    }
}

// Strict ASCII decimal: no sign, whitespace, separators or empty strings.
fn parse_decimal(key: &str, value: &str) -> Result<usize, ConfigError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ConfigError(format!(
            "{key} must be a positive decimal integer"
        )));
    }
    value
        .parse()
        .map_err(|_| ConfigError(format!("{key} is out of range")))
}

fn check_connections(limit: usize) -> Result<usize, ConfigError> {
    if limit == 0 || limit > ServerConfig::MAX_CONNECTIONS_LIMIT {
        return Err(ConfigError(format!(
            "KNOTREL_MAX_CONNECTIONS must be between 1 and {}",
            ServerConfig::MAX_CONNECTIONS_LIMIT
        )));
    }
    Ok(limit)
}

fn check_import_body_bytes(bytes: usize) -> Result<usize, ConfigError> {
    if bytes == 0 || bytes > ServerConfig::MAX_IMPORT_BODY_LIMIT {
        return Err(ConfigError(format!(
            "KNOTREL_MAX_IMPORT_BODY_BYTES must be between 1 and {}",
            ServerConfig::MAX_IMPORT_BODY_LIMIT
        )));
    }
    Ok(bytes)
}

fn check_dedup_window_size(size: usize) -> Result<usize, ConfigError> {
    if size == 0 || size > ServerConfig::MAX_DEDUP_WINDOW_SIZE {
        return Err(ConfigError(format!(
            "KNOTREL_DEDUP_WINDOW_SIZE must be between 1 and {}",
            ServerConfig::MAX_DEDUP_WINDOW_SIZE
        )));
    }
    Ok(size)
}

fn check_timeout(key: &str, timeout: Duration) -> Result<Duration, ConfigError> {
    if timeout.is_zero() || timeout > ServerConfig::MAX_TIMEOUT {
        return Err(ConfigError(format!(
            "{key} must be between 1 and {} milliseconds",
            ServerConfig::MAX_TIMEOUT.as_millis()
        )));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_defaults_and_explicit_values_do_not_depend_on_process_environment() {
        let default = ServerConfig::from_variables([(
            OsString::from("OTHER_APP"),
            OsString::from("ignored"),
        )])
        .unwrap();
        assert_eq!(default.address(), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(default.engine(), EngineConfig::CompactBfs);
        assert_eq!(default.max_pending_jobs(), 32);
        let selected = ServerConfig::from_variables([
            ("KNOTREL_ADDR".into(), "[::1]:0".into()),
            ("KNOTREL_ENGINE".into(), "ett".into()),
            ("KNOTREL_MAX_PENDING_JOBS".into(), "7".into()),
        ])
        .unwrap();
        assert_eq!(selected.address(), "[::1]:0".parse().unwrap());
        assert_eq!(selected.engine(), EngineConfig::EulerTour);
        assert_eq!(selected.max_pending_jobs(), 7);
    }

    #[test]
    fn graph_limits_load_independently_and_accept_zero() {
        let defaults = ServerConfig::from_variables([]).unwrap();
        assert_eq!(defaults.graph_limits(), ServerConfig::DEFAULT_GRAPH_LIMITS);
        let limits = ServerConfig::from_variables([
            ("KNOTREL_MAX_NODES".into(), "0".into()),
            ("KNOTREL_MAX_EDGES".into(), "12".into()),
        ])
        .unwrap()
        .graph_limits();
        assert_eq!(
            limits,
            GraphLimits {
                max_nodes: Some(0),
                max_edges: Some(12)
            }
        );
        let only_edges =
            ServerConfig::from_variables([("KNOTREL_MAX_EDGES".into(), "0".into())]).unwrap();
        assert_eq!(
            only_edges.graph_limits(),
            GraphLimits {
                max_nodes: ServerConfig::DEFAULT_GRAPH_LIMITS.max_nodes,
                max_edges: Some(0)
            }
        );
        let unlimited = ServerConfig::from_variables([
            ("KNOTREL_MAX_NODES".into(), "unlimited".into()),
            ("KNOTREL_MAX_EDGES".into(), "unlimited".into()),
        ])
        .unwrap();
        assert_eq!(unlimited.graph_limits(), GraphLimits::default());
        for value in ["Unlimited", "none", "-1", ""] {
            let error = ServerConfig::from_variables([("KNOTREL_MAX_NODES".into(), value.into())])
                .unwrap_err();
            assert!(error.to_string().starts_with("KNOTREL_MAX_NODES"));
        }
    }

    #[test]
    fn non_loopback_addresses_need_explicit_opt_in() {
        for address in [
            "0.0.0.0:8080",
            "[::]:8080",
            "192.168.1.10:8080",
            "[::ffff:127.0.0.1]:1",
        ] {
            let error = ServerConfig::from_variables([("KNOTREL_ADDR".into(), address.into())])
                .unwrap_err();
            assert!(error.to_string().contains("KNOTREL_ALLOW_NON_LOOPBACK"));
            assert!(!error.to_string().contains(address), "echoed {address}");
            let allowed = ServerConfig::from_variables([
                ("KNOTREL_ADDR".into(), address.into()),
                ("KNOTREL_ALLOW_NON_LOOPBACK".into(), "true".into()),
            ])
            .unwrap();
            assert!(allowed.is_exposed());
            assert!(
                ServerConfig::from_variables([
                    ("KNOTREL_ADDR".into(), address.into()),
                    ("KNOTREL_ALLOW_NON_LOOPBACK".into(), "false".into()),
                ])
                .is_err()
            );
        }
        for address in ["127.0.0.1:0", "127.9.9.9:80", "[::1]:80"] {
            let config =
                ServerConfig::from_variables([("KNOTREL_ADDR".into(), address.into())]).unwrap();
            assert!(!config.is_exposed());
        }
        for value in ["", "TRUE", "1", "yes", " true"] {
            let error = ServerConfig::from_variables([
                ("KNOTREL_ADDR".into(), "0.0.0.0:1".into()),
                ("KNOTREL_ALLOW_NON_LOOPBACK".into(), value.into()),
            ])
            .unwrap_err();
            assert!(error.to_string().starts_with("KNOTREL_ALLOW_NON_LOOPBACK"));
        }
        let any: SocketAddr = "0.0.0.0:1".parse().unwrap();
        assert!(ServerConfig::new(any, EngineConfig::Hdt, 1).is_err());
        assert!(ServerConfig::new_allowing_non_loopback(any, EngineConfig::Hdt, 1).is_ok());
    }

    #[test]
    fn lifecycle_settings_default_load_and_reject_unbounded_values() {
        let defaults = ServerConfig::from_variables([]).unwrap();
        assert_eq!(defaults.max_connections(), 1024);
        assert_eq!(defaults.header_read_timeout(), Duration::from_secs(5));
        assert_eq!(defaults.request_timeout(), Duration::from_secs(30));
        assert_eq!(defaults.shutdown_timeout(), Duration::from_secs(10));
        let loaded = ServerConfig::from_variables([
            ("KNOTREL_MAX_CONNECTIONS".into(), "3".into()),
            ("KNOTREL_HEADER_READ_TIMEOUT_MS".into(), "1".into()),
            ("KNOTREL_REQUEST_TIMEOUT_MS".into(), "250".into()),
            ("KNOTREL_SHUTDOWN_TIMEOUT_MS".into(), "3600000".into()),
        ])
        .unwrap();
        assert_eq!(loaded.max_connections(), 3);
        assert_eq!(loaded.header_read_timeout(), Duration::from_millis(1));
        assert_eq!(loaded.request_timeout(), Duration::from_millis(250));
        assert_eq!(loaded.shutdown_timeout(), Duration::from_secs(3600));
        let common = [
            "",
            "0",
            "-1",
            "+1",
            " 1",
            "1.5",
            "1s",
            "unlimited",
            "18446744073709551616",
        ];
        let cases = [
            ("KNOTREL_MAX_CONNECTIONS", "1000001"),
            ("KNOTREL_HEADER_READ_TIMEOUT_MS", "3600001"),
            ("KNOTREL_REQUEST_TIMEOUT_MS", "3600001"),
            ("KNOTREL_SHUTDOWN_TIMEOUT_MS", "3600001"),
        ];
        for (key, over_limit) in cases {
            for value in common.into_iter().chain([over_limit]) {
                let error = ServerConfig::from_variables([(key.into(), value.into())])
                    .map(|_| ())
                    .unwrap_err();
                assert!(error.to_string().starts_with(key), "{key}={value}");
            }
        }
        assert!(defaults.clone().with_max_connections(0).is_err());
        assert!(
            defaults
                .clone()
                .with_request_timeout(Duration::ZERO)
                .is_err()
        );
        assert!(
            defaults
                .with_shutdown_timeout(Duration::from_secs(3601))
                .is_err()
        );
    }

    #[test]
    fn dedup_window_size_configuration_and_limits() {
        let defaults = ServerConfig::from_variables([]).unwrap();
        assert_eq!(
            defaults.dedup_window_size(),
            ServerConfig::DEFAULT_DEDUP_WINDOW_SIZE
        );
        let custom =
            ServerConfig::from_variables([("KNOTREL_DEDUP_WINDOW_SIZE".into(), "5000".into())])
                .unwrap();
        assert_eq!(custom.dedup_window_size(), 5000);

        assert!(defaults.clone().with_dedup_window_size(0).is_err());
        assert!(defaults.clone().with_dedup_window_size(1_000_001).is_err());
        assert!(defaults.with_dedup_window_size(10).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_configuration_is_rejected_but_unrelated_environment_is_ignored() {
        use std::os::unix::ffi::OsStringExt;
        assert!(
            ServerConfig::from_variables([(
                "KNOTREL_ENGINE".into(),
                OsString::from_vec(vec![0xff])
            )])
            .is_err()
        );
        assert!(
            ServerConfig::from_variables([(
                OsString::from_vec(b"KNOTREL_\xff".to_vec()),
                "ett".into()
            )])
            .is_err()
        );
        assert!(
            ServerConfig::from_variables([(
                OsString::from_vec(vec![0xff]),
                OsString::from_vec(vec![0xff])
            )])
            .is_ok()
        );
    }

    #[test]
    fn additional_configuration_validation_and_getters() {
        // Unknown KNOTREL_ setting
        let err =
            ServerConfig::from_variables([("KNOTREL_FOOBAR".into(), "1".into())]).unwrap_err();
        assert!(err.to_string().contains("unknown setting: KNOTREL_FOOBAR"));

        // Malformed KNOTREL_ADDR
        let err = ServerConfig::from_variables([("KNOTREL_ADDR".into(), "not_an_ip".into())])
            .unwrap_err();
        assert!(err.to_string().contains("must be an IP socket address"));

        // KNOTREL_ENGINE=hdt and invalid
        let cfg = ServerConfig::from_variables([("KNOTREL_ENGINE".into(), "hdt".into())]).unwrap();
        assert_eq!(cfg.engine(), EngineConfig::Hdt);
        let err =
            ServerConfig::from_variables([("KNOTREL_ENGINE".into(), "unknown_engine".into())])
                .unwrap_err();
        assert!(err.to_string().contains("KNOTREL_ENGINE must be"));

        // KNOTREL_MODE
        let cfg = ServerConfig::from_variables([("KNOTREL_MODE".into(), "source_managed".into())])
            .unwrap();
        assert_eq!(cfg.mode(), ServerMode::SourceManaged);
        let cfg2 = ServerConfig::from_variables([("KNOTREL_MODE".into(), "source-managed".into())])
            .unwrap();
        assert_eq!(cfg2.mode(), ServerMode::SourceManaged);
        let err = ServerConfig::from_variables([("KNOTREL_MODE".into(), "invalid_mode".into())])
            .unwrap_err();
        assert!(err.to_string().contains("KNOTREL_MODE must be"));

        // KNOTREL_MAX_PENDING_JOBS non-numeric
        let err = ServerConfig::from_variables([("KNOTREL_MAX_PENDING_JOBS".into(), "abc".into())])
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be a positive decimal integer")
        );
        let err = ServerConfig::from_variables([("KNOTREL_MAX_PENDING_JOBS".into(), "".into())])
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be a positive decimal integer")
        );

        // KNOTREL_MAX_IMPORT_BODY_BYTES
        let cfg =
            ServerConfig::from_variables([("KNOTREL_MAX_IMPORT_BODY_BYTES".into(), "1024".into())])
                .unwrap();
        assert_eq!(cfg.max_import_body_bytes(), 1024);

        // with_max_import_body_bytes bounds
        let defaults = ServerConfig::default();
        assert!(defaults.clone().with_max_import_body_bytes(0).is_err());
        assert!(
            defaults
                .clone()
                .with_max_import_body_bytes(ServerConfig::MAX_IMPORT_BODY_LIMIT + 1)
                .is_err()
        );
        assert!(defaults.clone().with_max_import_body_bytes(1024).is_ok());

        // with_dedup_window_size bounds
        assert!(defaults.clone().with_dedup_window_size(0).is_err());
        assert!(
            defaults
                .clone()
                .with_dedup_window_size(ServerConfig::MAX_DEDUP_WINDOW_SIZE + 1)
                .is_err()
        );
        assert!(defaults.clone().with_dedup_window_size(100).is_ok());

        // KNOTREL_DEDUP_WINDOW_SIZE env
        let cfg = ServerConfig::from_variables([("KNOTREL_DEDUP_WINDOW_SIZE".into(), "50".into())])
            .unwrap();
        assert_eq!(cfg.dedup_window_size(), 50);

        // with_graph_limits and graph_limits
        let limits = knotrel_core::GraphLimits {
            max_nodes: Some(10),
            max_edges: Some(20),
        };
        let cfg = defaults.clone().with_graph_limits(limits);
        assert_eq!(cfg.graph_limits(), limits);

        // Getters
        assert_eq!(
            defaults.header_read_timeout(),
            ServerConfig::DEFAULT_HEADER_READ_TIMEOUT
        );
        assert_eq!(defaults.mode(), ServerMode::Standalone);
    }
}
