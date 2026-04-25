//! Server-group CRUD. Admin-gated.
//!
//! Groups are a UI-only organizational construct — dashboards render servers
//! bucketed under their group's display name, sorted by `order_idx`. The
//! panel never enforces anything about them beyond foreign-key tidiness.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{self, audit, AdminUser},
    state::AppState,
};

#[derive(Serialize, sqlx::FromRow)]
pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub order_idx: i32,
    pub description: Option<String>,
    pub color: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<GroupRow>>, StatusCode> {
    sqlx::query_as::<_, GroupRow>(
        "SELECT id, name, order_idx, description, color FROM server_groups ORDER BY order_idx, id",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|err| {
        tracing::error!(%err, "groups: list");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

#[derive(Deserialize)]
pub struct CreateGroup {
    pub name: String,
    pub order_idx: Option<i32>,
    pub description: Option<String>,
    pub color: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    headers: HeaderMap,
    Json(body): Json<CreateGroup>,
) -> Result<(StatusCode, Json<GroupRow>), axum::response::Response> {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return Err(bad("name_required", "group name must not be empty"));
    }
    let row: GroupRow = sqlx::query_as(
        r#"INSERT INTO server_groups (name, order_idx, description, color)
           VALUES ($1, $2, $3, $4)
           RETURNING id, name, order_idx, description, color"#,
    )
    .bind(&name)
    .bind(body.order_idx.unwrap_or(0))
    .bind(&body.description)
    .bind(&body.color)
    .fetch_one(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "groups: create");
        internal()
    })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "group.created",
        Some(&row.name),
        &meta,
    )
    .await;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct UpdateGroup {
    pub name: Option<String>,
    pub order_idx: Option<i32>,
    pub description: Option<String>,
    pub color: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<UpdateGroup>,
) -> Result<Json<GroupRow>, axum::response::Response> {
    // Single UPDATE with COALESCE — lets callers send a partial object.
    let row: Option<GroupRow> = sqlx::query_as(
        r#"UPDATE server_groups SET
              name        = COALESCE($2, name),
              order_idx   = COALESCE($3, order_idx),
              description = COALESCE($4, description),
              color       = COALESCE($5, color)
            WHERE id = $1
            RETURNING id, name, order_idx, description, color"#,
    )
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.order_idx)
    .bind(body.description.as_deref())
    .bind(body.color.as_deref())
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "groups: update");
        internal()
    })?;

    let row = row.ok_or_else(not_found)?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "group.updated",
        Some(&row.name),
        &meta,
    )
    .await;
    Ok(Json(row))
}

pub async fn delete_one(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, axum::response::Response> {
    let row: Option<(String,)> =
        sqlx::query_as("DELETE FROM server_groups WHERE id = $1 RETURNING name")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "groups: delete");
                internal()
            })?;
    let (name,) = row.ok_or_else(not_found)?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "group.deleted",
        Some(&name),
        &meta,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
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
            message: "group not found",
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
