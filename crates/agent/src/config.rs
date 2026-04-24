//! Persistent agent configuration.
//!
//! Stores the panel endpoint and the tokens used to authenticate there. The
//! file lives at a platform-appropriate default location but `--config` /
//! `$MONITOR_AGENT_CONFIG` always takes precedence.
//!
//! State transitions through life:
//! - after `configure`: {endpoint, join_token}
//! - after first Register: {endpoint, agent_id, server_token} (join_token cleared)

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("i/o error reading/writing config at {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("malformed config file: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("missing required field: {0}")]
    Missing(&'static str),
    #[error("invalid endpoint: {0}")]
    Endpoint(#[from] monitor_common::Error),
}

/// Persistent configuration file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Panel URL, e.g. `https://panel.example.com/grpc`.
    pub endpoint: String,

    /// Single-use token used only for the initial Register call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_token: Option<String>,

    /// Panel-assigned stable agent identifier (UUID). Filled by Register.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Long-lived token used for the Stream RPC. Filled by Register.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_token: Option<String>,

    /// How often heartbeats are emitted on the Stream, in seconds.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_s: u64,
}

fn default_heartbeat_interval() -> u64 {
    10
}

impl AgentConfig {
    /// Validate that the endpoint parses and at least one credential is set.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.endpoint.trim().is_empty() {
            return Err(ConfigError::Missing("endpoint"));
        }
        monitor_common::AgentEndpoint::parse(&self.endpoint)?;
        if self.server_token.is_none() && self.join_token.is_none() {
            return Err(ConfigError::Missing("join_token or server_token"));
        }
        Ok(())
    }

    /// True when the agent has not yet completed first-time Register.
    #[must_use]
    pub fn needs_register(&self) -> bool {
        self.server_token.is_none() && self.join_token.is_some()
    }

    /// Parsed endpoint URL (convenience).
    pub fn parsed_endpoint(&self) -> Result<monitor_common::AgentEndpoint, monitor_common::Error> {
        monitor_common::AgentEndpoint::parse(&self.endpoint)
    }

    /// Load from a specific path.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Self = serde_yaml::from_str(&raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Atomically save to a specific path. Writes to `<path>.tmp` then renames
    /// so a crash mid-write cannot corrupt an existing good config.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let serialized = serde_yaml::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        fs::write(&tmp, serialized).map_err(|source| ConfigError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

/// Platform-default path for the persistent config file.
// Explicit `return` in cfg-gated arms makes it obvious that exactly one is
// reachable per platform; clippy would otherwise flag them as needless.
#[allow(clippy::needless_return)]
#[must_use]
pub fn default_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        return PathBuf::from("/etc/monitor-agent/config.yaml");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/Library/Application Support/monitor-agent/config.yaml");
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
        return base.join("monitor-agent").join("config.yaml");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("./monitor-agent.yaml")
    }
}

/// Resolve the config path with the usual precedence: explicit flag → env var → default.
pub fn resolve_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("MONITOR_AGENT_CONFIG") {
        return PathBuf::from(p);
    }
    default_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_save_and_load() {
        let dir = tempdir();
        let path = dir.join("config.yaml");
        let cfg = AgentConfig {
            endpoint: "https://panel.example.com/grpc".into(),
            join_token: Some("pending-token".into()),
            agent_id: None,
            server_token: None,
            heartbeat_interval_s: 10,
        };
        cfg.save(&path).unwrap();
        let loaded = AgentConfig::load(&path).unwrap();
        assert_eq!(loaded.endpoint, cfg.endpoint);
        assert_eq!(loaded.join_token, cfg.join_token);
        assert!(loaded.needs_register());
    }

    #[test]
    fn invalid_endpoint_fails_validation() {
        let cfg = AgentConfig {
            endpoint: "not a url".into(),
            join_token: Some("x".into()),
            agent_id: None,
            server_token: None,
            heartbeat_interval_s: 10,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn missing_credentials_fail() {
        let cfg = AgentConfig {
            endpoint: "http://panel:9090".into(),
            join_token: None,
            agent_id: None,
            server_token: None,
            heartbeat_interval_s: 10,
        };
        assert!(cfg.validate().is_err());
    }

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join(format!("monitor-agent-test-{}", std::process::id()))
            .join(format!("{}", rand_id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn rand_id() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}
