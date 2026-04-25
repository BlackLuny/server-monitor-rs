//! Probe + per-agent override + result query endpoints.
//!
//! Admin-guarded throughout. Mutating handlers nudge the assignment bus so
//! the scheduler reconciles every connected agent within one tick.

// `axum::response::Response` is genuinely large (~128 bytes) but it's the
// idiomatic error type for handlers that may emit different status codes.
// All siblings in this api module already opt out of this lint for the same
// reason — keeping the surface uniform.
#![allow(clippy::result_large_err)]

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    auth::{self, audit, AdminUser},
    state::AppState,
};

const ALLOWED_KINDS: &[&str] = &["icmp", "tcp", "http"];

// ---------------------------------------------------------------------------
// GET /api/probes
// ---------------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
pub struct ProbeRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub port: Option<i32>,
    pub interval_s: i32,
    pub timeout_ms: i32,
    pub http_method: Option<String>,
    pub http_expect_code: Option<i32>,
    pub http_expect_body: Option<String>,
    pub default_enabled: bool,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<ProbeRow>>, StatusCode> {
    sqlx::query_as::<_, ProbeRow>(
        "SELECT id, name, kind, target, port, interval_s, timeout_ms, \
                http_method, http_expect_code, http_expect_body, \
                default_enabled, enabled, created_at, updated_at \
           FROM probes ORDER BY name",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|err| {
        tracing::error!(%err, "probes: list");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// ---------------------------------------------------------------------------
// POST /api/probes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateProbe {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub port: Option<i32>,
    pub interval_s: Option<i32>,
    pub timeout_ms: Option<i32>,
    pub http_method: Option<String>,
    pub http_expect_code: Option<i32>,
    pub http_expect_body: Option<String>,
    pub default_enabled: Option<bool>,
    pub enabled: Option<bool>,
}

pub async fn create(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    headers: HeaderMap,
    Json(body): Json<CreateProbe>,
) -> Result<(StatusCode, Json<ProbeRow>), axum::response::Response> {
    validate_create(&body)?;

    let row: ProbeRow = sqlx::query_as(
        r#"INSERT INTO probes
              (name, kind, target, port, interval_s, timeout_ms,
               http_method, http_expect_code, http_expect_body,
               default_enabled, enabled, created_by)
           VALUES
              ($1, $2, $3, $4, COALESCE($5, 60), COALESCE($6, 3000),
               $7, $8, $9, COALESCE($10, TRUE), COALESCE($11, TRUE), $12)
           RETURNING id, name, kind, target, port, interval_s, timeout_ms,
                     http_method, http_expect_code, http_expect_body,
                     default_enabled, enabled, created_at, updated_at"#,
    )
    .bind(body.name.trim())
    .bind(&body.kind)
    .bind(body.target.trim())
    .bind(body.port)
    .bind(body.interval_s)
    .bind(body.timeout_ms)
    .bind(body.http_method.as_deref())
    .bind(body.http_expect_code)
    .bind(body.http_expect_body.as_deref())
    .bind(body.default_enabled)
    .bind(body.enabled)
    .bind(session.user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "probes: insert");
        internal()
    })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "probe.created",
        Some(&row.name),
        &meta,
    )
    .await;

    state.assignment_bus.publish();
    Ok((StatusCode::CREATED, Json(row)))
}

// ---------------------------------------------------------------------------
// PATCH /api/probes/:id
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UpdateProbe {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub target: Option<String>,
    pub port: Option<Option<i32>>,
    pub interval_s: Option<i32>,
    pub timeout_ms: Option<i32>,
    pub http_method: Option<Option<String>>,
    pub http_expect_code: Option<Option<i32>>,
    pub http_expect_body: Option<Option<String>>,
    pub default_enabled: Option<bool>,
    pub enabled: Option<bool>,
}

pub async fn update(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<UpdateProbe>,
) -> Result<Json<ProbeRow>, axum::response::Response> {
    if let Some(k) = body.kind.as_deref() {
        if !ALLOWED_KINDS.contains(&k) {
            return Err(bad("invalid_kind", "kind must be icmp/tcp/http"));
        }
    }
    if let Some(name) = body.name.as_deref() {
        if name.trim().is_empty() {
            return Err(bad("name_required", "name must not be empty"));
        }
    }
    if let Some(t) = body.target.as_deref() {
        if t.trim().is_empty() {
            return Err(bad("target_required", "target must not be empty"));
        }
    }

    // Same Option<Option<T>> trick we use in servers.rs to distinguish
    // "leave alone" from "set to NULL".
    let row: Option<ProbeRow> = sqlx::query_as(
        r#"UPDATE probes SET
              name             = COALESCE($2, name),
              kind             = COALESCE($3, kind),
              target           = COALESCE($4, target),
              interval_s       = COALESCE($5, interval_s),
              timeout_ms       = COALESCE($6, timeout_ms),
              default_enabled  = COALESCE($7, default_enabled),
              enabled          = COALESCE($8, enabled),
              port             = CASE WHEN $9  THEN $10 ELSE port             END,
              http_method      = CASE WHEN $11 THEN $12 ELSE http_method      END,
              http_expect_code = CASE WHEN $13 THEN $14 ELSE http_expect_code END,
              http_expect_body = CASE WHEN $15 THEN $16 ELSE http_expect_body END,
              updated_at       = NOW()
            WHERE id = $1
            RETURNING id, name, kind, target, port, interval_s, timeout_ms,
                      http_method, http_expect_code, http_expect_body,
                      default_enabled, enabled, created_at, updated_at"#,
    )
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.kind.as_deref())
    .bind(body.target.as_deref().map(str::trim))
    .bind(body.interval_s)
    .bind(body.timeout_ms)
    .bind(body.default_enabled)
    .bind(body.enabled)
    .bind(body.port.is_some())
    .bind(body.port.flatten())
    .bind(body.http_method.is_some())
    .bind(body.http_method.as_ref().and_then(|o| o.as_deref()))
    .bind(body.http_expect_code.is_some())
    .bind(body.http_expect_code.flatten())
    .bind(body.http_expect_body.is_some())
    .bind(body.http_expect_body.as_ref().and_then(|o| o.as_deref()))
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "probes: update");
        internal()
    })?;
    let row = row.ok_or_else(not_found)?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "probe.updated",
        Some(&row.name),
        &meta,
    )
    .await;

    state.assignment_bus.publish();
    Ok(Json(row))
}

// ---------------------------------------------------------------------------
// DELETE /api/probes/:id
// ---------------------------------------------------------------------------

pub async fn delete_one(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, axum::response::Response> {
    let removed: Option<(String,)> =
        sqlx::query_as("DELETE FROM probes WHERE id = $1 RETURNING name")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "probes: delete");
                internal()
            })?;
    let (name,) = removed.ok_or_else(not_found)?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "probe.deleted",
        Some(&name),
        &meta,
    )
    .await;

    state.assignment_bus.publish();
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// GET /api/probes/:id/agents — effective state matrix per agent
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AgentEffectiveRow {
    pub agent_id: String,
    pub display_name: String,
    pub default_enabled: bool,
    /// `Some(b)` if explicit override exists, `None` if inheriting default.
    pub override_enabled: Option<bool>,
    pub effective_enabled: bool,
}

pub async fn list_agents_for_probe(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<AgentEffectiveRow>>, axum::response::Response> {
    let default_enabled: bool =
        sqlx::query_scalar("SELECT default_enabled FROM probes WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "probes: load default");
                internal()
            })?
            .ok_or_else(not_found)?;

    let rows: Vec<(Uuid, String, Option<bool>)> = sqlx::query_as(
        r#"SELECT s.agent_id, s.display_name, o.enabled
             FROM servers s
             LEFT JOIN probe_agent_overrides o
               ON o.agent_id = s.agent_id AND o.probe_id = $1
            ORDER BY s.display_name"#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "probes: list agents");
        internal()
    })?;

    let out = rows
        .into_iter()
        .map(
            |(agent_id, display_name, override_enabled)| AgentEffectiveRow {
                agent_id: agent_id.to_string(),
                display_name,
                default_enabled,
                override_enabled,
                effective_enabled: override_enabled.unwrap_or(default_enabled),
            },
        )
        .collect();
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// PUT /api/probes/:id/agents/:agent_id  — set or clear an override
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SetOverride {
    /// `null` clears the override (revert to probe default).
    pub enabled: Option<bool>,
}

pub async fn set_override(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path((probe_id, agent_id)): Path<(i64, String)>,
    headers: HeaderMap,
    Json(body): Json<SetOverride>,
) -> Result<StatusCode, axum::response::Response> {
    let agent_uuid: Uuid = agent_id
        .parse()
        .map_err(|_| bad("invalid_agent_id", "agent_id must be a UUID"))?;

    // Refuse if the probe doesn't exist — early 404 so callers don't think
    // their override "took" silently.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM probes WHERE id = $1)")
        .bind(probe_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "override: probe lookup");
            internal()
        })?;
    if !exists {
        return Err(not_found());
    }

    // Only persist rows that actually deviate from the default, so the
    // override table stays sparse. If `enabled` matches the probe default,
    // delete any existing row instead.
    let default_enabled: bool =
        sqlx::query_scalar("SELECT default_enabled FROM probes WHERE id = $1")
            .bind(probe_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "override: default fetch");
                internal()
            })?;

    let action;
    match body.enabled {
        Some(want) if want == default_enabled => {
            // Same as default → just clear the override row.
            sqlx::query("DELETE FROM probe_agent_overrides WHERE probe_id = $1 AND agent_id = $2")
                .bind(probe_id)
                .bind(agent_uuid)
                .execute(&state.pool)
                .await
                .map_err(|err| {
                    tracing::error!(%err, "override: delete");
                    internal()
                })?;
            action = "probe.override_cleared";
        }
        Some(want) => {
            sqlx::query(
                r#"INSERT INTO probe_agent_overrides (probe_id, agent_id, enabled)
                   VALUES ($1, $2, $3)
                   ON CONFLICT (probe_id, agent_id) DO UPDATE SET
                     enabled = EXCLUDED.enabled, updated_at = NOW()"#,
            )
            .bind(probe_id)
            .bind(agent_uuid)
            .bind(want)
            .execute(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "override: upsert");
                internal()
            })?;
            action = "probe.override_set";
        }
        None => {
            sqlx::query("DELETE FROM probe_agent_overrides WHERE probe_id = $1 AND agent_id = $2")
                .bind(probe_id)
                .bind(agent_uuid)
                .execute(&state.pool)
                .await
                .map_err(|err| {
                    tracing::error!(%err, "override: delete on null");
                    internal()
                })?;
            action = "probe.override_cleared";
        }
    }

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        action,
        Some(&format!("probe={probe_id} agent={agent_uuid}")),
        &meta,
    )
    .await;

    state.assignment_bus.publish();
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// GET /api/probes/:id/results  — time-series for the chart
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ResultsQuery {
    pub range: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Serialize)]
pub struct ResultsSeries {
    pub probe_id: i64,
    pub range: String,
    pub granularity: &'static str,
    pub points: Vec<ResultPoint>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ResultPoint {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub agent_id: Uuid,
    pub ok: bool,
    pub latency_us: i64,
    pub latency_us_p50: Option<i64>,
    pub latency_us_p95: Option<i64>,
    pub success_rate: Option<f64>,
    pub status_code: Option<i32>,
    pub error: Option<String>,
}

pub async fn results(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<i64>,
    Query(q): Query<ResultsQuery>,
) -> Result<Json<ResultsSeries>, StatusCode> {
    let range = q.range.as_deref().unwrap_or("1h");
    let (granularity, interval) = pick_granularity(range);

    let rows: Vec<ResultPoint> = match q.agent_id.as_deref() {
        Some(a) => {
            let agent_uuid: Uuid = a.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
            sqlx::query_as(
                r#"SELECT ts, agent_id, ok, latency_us,
                          latency_us_p50, latency_us_p95, success_rate,
                          status_code, error
                     FROM probe_results
                    WHERE probe_id = $1
                      AND granularity = $2
                      AND agent_id = $3
                      AND ts >= NOW() - ($4::text)::interval
                    ORDER BY ts"#,
            )
            .bind(id)
            .bind(granularity)
            .bind(agent_uuid)
            .bind(interval)
            .fetch_all(&state.pool)
            .await
        }
        None => {
            sqlx::query_as(
                r#"SELECT ts, agent_id, ok, latency_us,
                          latency_us_p50, latency_us_p95, success_rate,
                          status_code, error
                     FROM probe_results
                    WHERE probe_id = $1
                      AND granularity = $2
                      AND ts >= NOW() - ($3::text)::interval
                    ORDER BY ts"#,
            )
            .bind(id)
            .bind(granularity)
            .bind(interval)
            .fetch_all(&state.pool)
            .await
        }
    }
    .map_err(|err| {
        tracing::error!(%err, "probes: results");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ResultsSeries {
        probe_id: id,
        range: range.to_owned(),
        granularity,
        points: rows,
    }))
}

fn pick_granularity(range: &str) -> (&'static str, &'static str) {
    match range {
        "10m" | "30m" | "1h" => ("raw", "1 hour"),
        "6h" => ("m1", "6 hours"),
        "24h" => ("m1", "24 hours"),
        "7d" => ("m5", "7 days"),
        "30d" => ("h1", "30 days"),
        "90d" => ("h1", "90 days"),
        _ => ("raw", "1 hour"),
    }
}

// ---------------------------------------------------------------------------
// validation + errors
// ---------------------------------------------------------------------------

fn validate_create(body: &CreateProbe) -> Result<(), axum::response::Response> {
    if body.name.trim().is_empty() {
        return Err(bad("name_required", "name must not be empty"));
    }
    if !ALLOWED_KINDS.contains(&body.kind.as_str()) {
        return Err(bad("invalid_kind", "kind must be icmp/tcp/http"));
    }
    if body.target.trim().is_empty() {
        return Err(bad("target_required", "target must not be empty"));
    }
    if body.kind == "tcp" && body.port.unwrap_or(0) <= 0 {
        return Err(bad("port_required", "tcp probe needs a port"));
    }
    if body.kind == "http" {
        let t = body.target.trim();
        if !(t.starts_with("http://") || t.starts_with("https://")) {
            return Err(bad("invalid_url", "http target must include scheme"));
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

fn bad(code: &'static str, message: &'static str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { code, message })).into_response()
}

fn not_found() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            code: "not_found",
            message: "probe not found",
        }),
    )
        .into_response()
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

// suppress dead_code on Value import — kept for forward compatibility
const _: Option<Value> = None;
