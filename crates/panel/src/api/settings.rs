//! Global KV settings + audit-log read-model.
//!
//! The `settings` table backs the admin-configurable options: `site_name`,
//! `guest_enabled`, `agent_endpoint`, `ssh_recording_enabled`. Values are
//! JSONB so future options can grow richer structures without a migration.
//!
//! A list view of `audit_log` lives here too because it's admin-only and
//! shares a conceptual lane with settings (things only admins inspect).

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{
    auth::{self, audit, AdminUser},
    state::AppState,
};

// ---------------------------------------------------------------------------
// GET /api/settings
// ---------------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
pub struct SettingRow {
    pub key: String,
    pub value: Value,
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<SettingRow>>, StatusCode> {
    sqlx::query_as::<_, SettingRow>("SELECT key, value FROM settings ORDER BY key")
        .fetch_all(&state.pool)
        .await
        .map(Json)
        .map_err(|err| {
            tracing::error!(%err, "settings: list");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// ---------------------------------------------------------------------------
// PUT /api/settings/:key
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PutBody {
    pub value: Value,
}

pub async fn put_one(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<PutBody>,
) -> Result<Json<SettingRow>, axum::response::Response> {
    // Whitelist the known keys — unknown keys are a bug somewhere, not an
    // opportunity for silent storage.
    if !is_allowed_key(&key) {
        return Err(bad("unknown_setting", "unknown setting key"));
    }
    if let Err(err) = validate(&key, &body.value) {
        return Err(bad("invalid_value", err));
    }

    let row: SettingRow = sqlx::query_as(
        r#"INSERT INTO settings (key, value) VALUES ($1, $2)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value
           RETURNING key, value"#,
    )
    .bind(&key)
    .bind(&body.value)
    .fetch_one(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, key, "settings: put");
        internal()
    })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "settings.updated",
        Some(&row.key),
        &meta,
    )
    .await;
    Ok(Json(row))
}

fn is_allowed_key(k: &str) -> bool {
    matches!(
        k,
        "site_name" | "guest_enabled" | "agent_endpoint" | "ssh_recording_enabled"
    )
}

fn validate(key: &str, value: &Value) -> Result<(), &'static str> {
    match key {
        "site_name" | "agent_endpoint" => {
            if value.is_string() {
                Ok(())
            } else {
                Err("expected a string")
            }
        }
        "guest_enabled" | "ssh_recording_enabled" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err("expected a boolean")
            }
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// GET /api/audit
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct AuditRow {
    pub id: i64,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
}

pub async fn list_audit(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<AuditRow>>, StatusCode> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    sqlx::query_as::<_, AuditRow>(
        r#"SELECT a.id, a.user_id, u.username, a.action, a.target, a.ip, a.user_agent, a.ts
             FROM audit_log a
             LEFT JOIN users u ON u.id = a.user_id
            ORDER BY a.ts DESC
            LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|err| {
        tracing::error!(%err, "audit: list");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

fn bad(code: &'static str, message: &'static str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { code, message })).into_response()
}

fn internal() -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            code: "internal_error",
            message: "an internal error occurred",
        }),
    )
        .into_response()
}
