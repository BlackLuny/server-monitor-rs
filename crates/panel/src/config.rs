//! Panel configuration loading.
//!
//! Precedence (highest first):
//! 1. Environment variables prefixed `MONITOR_` (double-underscore for nesting,
//!    e.g. `MONITOR_DATABASE__URL`).
//! 2. YAML file at the path from `--config` / `$MONITOR_CONFIG`, or the default
//!    search paths if neither is set.
//! 3. Built-in defaults.
//!
//! Default YAML search paths (first existing wins):
//!   - `./config.yaml`
//!   - `/etc/monitor-panel/config.yaml`

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::{
    providers::{Env, Format, Serialized, Yaml},
    Figment,
};
use serde::{Deserialize, Serialize};

/// Root panel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub grpc: GrpcConfig,
    pub database: DatabaseConfig,
    pub jwt: JwtConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub github: GitHubConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    /// Set to `true` only for plain-HTTP deployments where the panel sits
    /// behind no TLS terminator. The default emits cookies with the
    /// `Secure` attribute, which modern browsers refuse to store over
    /// http://. Override with `MONITOR_HTTP__INSECURE_COOKIES=true`.
    #[serde(default)]
    pub insecure_cookies: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".parse().expect("valid default"),
            insecure_cookies: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcConfig {
    pub listen: SocketAddr,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9090".parse().expect("valid default"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Postgres connection string, e.g. `postgres://user:pass@host:5432/db`.
    pub url: String,
    #[serde(default = "default_db_pool")]
    pub max_connections: u32,
}

fn default_db_pool() -> u32 {
    16
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    /// Random secret used to sign session JWTs. Must be ≥ 32 bytes.
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// tracing-subscriber EnvFilter directive (e.g. `info,sqlx=warn`).
    #[serde(default = "default_log_filter")]
    pub filter: String,
    #[serde(default)]
    pub format: LogFormat,
}

fn default_log_filter() -> String {
    "info".to_string()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: default_log_filter(),
            format: LogFormat::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// `owner/repo` for release polling. Empty disables update checks.
    #[serde(default)]
    pub repo: String,
    /// Optional PAT for higher API rate limit (5000/h vs 60/h anon).
    #[serde(default)]
    pub token: String,
}

impl Config {
    /// Load configuration using the precedence described in the module docs.
    ///
    /// `explicit_path` overrides the default YAML search paths when provided.
    pub fn load(explicit_path: Option<&Path>) -> anyhow::Result<Self> {
        let yaml_path = explicit_path
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var_os("MONITOR_CONFIG").map(PathBuf::from))
            .or_else(Self::find_default_yaml);

        // Seed with a placeholder so the YAML/env layers have something to merge into.
        // We cannot call `Default` on `Config` (it has no sane defaults for
        // database/jwt) so we use `Serialized::default(...)` on the nested
        // defaults only. `Figment`'s Yaml provider will fail loudly if the
        // user's file omits required keys.
        let mut figment = Figment::new().merge(Serialized::defaults(Defaults::default()));

        if let Some(path) = yaml_path.as_ref() {
            if path.exists() {
                figment = figment.merge(Yaml::file(path));
            }
        }

        figment = figment.merge(Env::prefixed("MONITOR_").split("__"));

        let cfg: Self = figment
            .extract()
            .map_err(|e| anyhow::anyhow!("invalid config: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn find_default_yaml() -> Option<PathBuf> {
        const CANDIDATES: &[&str] = &["./config.yaml", "/etc/monitor-panel/config.yaml"];
        CANDIDATES.iter().map(PathBuf::from).find(|p| p.exists())
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.jwt.secret.len() < 32 {
            anyhow::bail!("jwt.secret must be at least 32 bytes long");
        }
        if self.database.url.trim().is_empty() {
            anyhow::bail!("database.url is required");
        }
        Ok(())
    }
}

/// Defaults struct used only as a serde_json source to populate optional fields.
#[derive(Default, Serialize)]
struct Defaults {
    http: HttpConfig,
    grpc: GrpcConfig,
    log: LogConfig,
    github: GitHubConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_jwt_secret() {
        let yaml = r#"
database:
  url: "postgres://x"
jwt:
  secret: "too-short"
"#;
        let figment = Figment::new()
            .merge(Serialized::defaults(Defaults::default()))
            .merge(Yaml::string(yaml));
        let cfg: Config = figment.extract().unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_valid_yaml() {
        let yaml = r#"
database:
  url: "postgres://user:pass@localhost/monitor"
jwt:
  secret: "0123456789abcdef0123456789abcdef"
"#;
        let figment = Figment::new()
            .merge(Serialized::defaults(Defaults::default()))
            .merge(Yaml::string(yaml));
        let cfg: Config = figment.extract().unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.http.listen.port(), 8080);
        assert_eq!(cfg.grpc.listen.port(), 9090);
        assert_eq!(cfg.database.max_connections, 16);
    }
}
