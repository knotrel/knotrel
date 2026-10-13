//! Startup-only configuration loading and validation.

use knotrel_core::{EngineConfig, GraphLimits};
use std::{collections::BTreeMap, error::Error, ffi::OsString, fmt, net::SocketAddr};

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
        }
    }
}

impl ServerConfig {
    /// Default ceiling on registered vertices for configured servers.
    pub const DEFAULT_MAX_NODES: usize = 100_000;
    /// Default ceiling on live edges for configured servers.
    pub const DEFAULT_MAX_EDGES: usize = 500_000;
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
        })
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
        Ok(
            Self::build(address, allow_non_loopback, engine, max_pending_jobs)?
                .with_graph_limits(limits),
        )
    }
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
}
