//! Typed accessors over the `settings` KV table.
//!
//! The table stores JSONB values keyed by string. These helpers give the rest
//! of the panel a typed view without scattering raw queries around.

use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;

/// Errors surfaced by settings helpers.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("setting `{key}` has an unexpected shape: {err}")]
    Shape {
        key: &'static str,
        err: serde_json::Error,
    },
}

/// Fetch a setting as an arbitrary typed value, returning `None` when absent.
pub async fn get<T: DeserializeOwned>(
    pool: &PgPool,
    key: &'static str,
) -> Result<Option<T>, SettingsError> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    match row {
        None => Ok(None),
        Some((v,)) => serde_json::from_value(v)
            .map(Some)
            .map_err(|err| SettingsError::Shape { key, err }),
    }
}

/// Fetch the configured agent endpoint (trimmed). Returns `None` when unset or
/// when the value is empty — callers should treat both as "not configured".
pub async fn agent_endpoint(pool: &PgPool) -> Result<Option<String>, SettingsError> {
    trimmed_string(pool, "agent_endpoint").await
}

/// Public HTTP base for the panel — used to build URLs that browsers and
/// `curl` need to fetch from (notably `install-agent.sh`). Distinct from
/// `agent_endpoint`, which is the gRPC dial URL agents use. Trailing slashes
/// are stripped so callers can concatenate `/install-agent.sh` directly.
pub async fn panel_public_url(pool: &PgPool) -> Result<Option<String>, SettingsError> {
    Ok(trimmed_string(pool, "panel_public_url")
        .await?
        .map(|s| s.trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty()))
}

async fn trimmed_string(
    pool: &PgPool,
    key: &'static str,
) -> Result<Option<String>, SettingsError> {
    let val: Option<String> = get(pool, key).await?;
    Ok(val.and_then(|s| {
        let t = s.trim().to_owned();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    }))
}

/// Whether guests (unauthenticated callers) may inspect the dashboard.
/// Defaults to `true` when unset.
pub async fn guest_enabled(pool: &PgPool) -> Result<bool, SettingsError> {
    Ok(get::<bool>(pool, "guest_enabled").await?.unwrap_or(true))
}

/// Global default for whether SSH/web-terminal sessions get recorded. The
/// per-server `ssh_recording` column overrides this when set to `'on'` /
/// `'off'`; `'default'` defers to this value. Off when unset.
pub async fn ssh_recording_default(pool: &PgPool) -> Result<bool, SettingsError> {
    Ok(get::<bool>(pool, "ssh_recording_enabled")
        .await?
        .unwrap_or(false))
}
