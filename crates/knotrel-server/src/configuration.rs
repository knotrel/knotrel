//! Startup-only configuration loading and validation.

use knotrel_core::EngineConfig;
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
        }
    }
}

impl ServerConfig {
    /// Validates explicit settings without reading the environment.
    ///
    /// Returns [`ConfigError`] if `max_pending_jobs` is zero or exceeds
    /// [`tokio::sync::Semaphore::MAX_PERMITS`]. The limit covers queued plus
    /// running graph jobs, not parallel access to the graph.
    pub fn new(
        address: SocketAddr,
        engine: EngineConfig,
        max_pending_jobs: usize,
    ) -> Result<Self, ConfigError> {
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
        })
    }

    /// Loads and validates a snapshot of the process environment.
    ///
    /// Recognized variables are `KNOTREL_ADDR` (default `127.0.0.1:8080`),
    /// `KNOTREL_ENGINE` (`compact-bfs` by default, or experimental `ett` / `hdt`) and
    /// `KNOTREL_MAX_PENDING_JOBS` (default `32`).
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
                "KNOTREL_ADDR" | "KNOTREL_ENGINE" | "KNOTREL_MAX_PENDING_JOBS"
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
        Self::new(address, engine, max_pending_jobs)
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
