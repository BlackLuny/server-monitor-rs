//! `/api/updates/*` — admin-only.
//!
//! The orchestrator lives in [`crate::updates`]; this module is just the
//! HTTP surface. Every handler is admin-gated; CSRF middleware already
//! protects the mutating verbs at the router level.

#![allow(clippy::result_large_err)]

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::{
    auth::AdminUser,
    state::AppState,
    updates::{
        abort_rollout, create_rollout, dispatch, get_rollout, list_recent_releases, list_rollouts,
        pause_rollout, poller::LatestRelease, resume_rollout, rollout::RolloutError,
        CreateRolloutInput, RolloutSummary, RolloutView,
    },
};

pub async fn latest(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, Response> {
    let value =
        sqlx::query_scalar::<_, Value>("SELECT value FROM settings WHERE key = 'latest_release'")
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
    Ok(Json(value.unwrap_or(Value::Null)))
}

pub async fn recent(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<LatestRelease>>, Response> {
    list_recent_releases(&state.pool)
        .await
        .map(Json)
        .map_err(translate)
}

pub async fn list(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<RolloutSummary>>, Response> {
    list_rollouts(&state.pool)
        .await
        .map(Json)
        .map_err(translate)
}

pub async fn detail(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RolloutView>, Response> {
    get_rollout(&state.pool, id)
        .await
        .map(Json)
        .map_err(translate)
}

pub async fn create(
    AdminUser(session): AdminUser,
    State(state): State<AppState>,
    Json(input): Json<CreateRolloutInput>,
) -> Result<(StatusCode, Json<RolloutView>), Response> {
    let id = create_rollout(&state.pool, input, Some(session.user_id))
        .await
        .map_err(translate)?;
    // Push UpdateAgent to anyone online right now. Offline agents pick it
    // up on Register; that path is wired in agent_service.rs.
    if let Err(err) = dispatch::dispatch_pending_for_rollout(&state.pool, &state.hub, id).await {
        tracing::warn!(%err, rollout_id = id, "dispatch failed — assignments stay pending");
    }
    let view = get_rollout(&state.pool, id).await.map_err(translate)?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn pause(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Response> {
    pause_rollout(&state.pool, id).await.map_err(translate)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resume(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Response> {
    resume_rollout(&state.pool, id).await.map_err(translate)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn abort(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Response> {
    abort_rollout(&state.pool, id).await.map_err(translate)?;
    if let Err(err) =
        dispatch::dispatch_aborts_for_rollout(&state.pool, &state.hub, id, "rollout aborted").await
    {
        tracing::warn!(%err, rollout_id = id, "abort dispatch failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

fn translate(err: RolloutError) -> Response {
    use RolloutError::*;
    let (status, code, msg) = match &err {
        NotFound { .. } => (StatusCode::NOT_FOUND, "not_found", err.to_string()),
        VersionMismatch { .. } => (StatusCode::CONFLICT, "version_mismatch", err.to_string()),
        VersionUnknown { .. } => (StatusCode::NOT_FOUND, "version_unknown", err.to_string()),
        NoCachedRelease => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no_cached_release",
            err.to_string(),
        ),
        NoEligibleAgents => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "no_eligible_agents",
            err.to_string(),
        ),
        PercentOutOfRange(_) => (StatusCode::BAD_REQUEST, "bad_percent", err.to_string()),
        BadTransition { .. } => (StatusCode::CONFLICT, "bad_transition", err.to_string()),
        Db(_) | Settings(_) => {
            tracing::error!(%err, "rollout internal error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal".into(),
            )
        }
    };
    let body = serde_json::json!({"code": code, "message": msg});
    (status, Json(body)).into_response()
}

fn internal(err: sqlx::Error) -> Response {
    tracing::error!(%err, "updates db error");
    (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
}
