//! User administration endpoints.
//!
//! Admins manage other admins through this surface; every user in the panel
//! today is implicitly `role='admin'` because that's the only value the
//! users.role CHECK allows. The extra role column is intentional — future
//! tiers ("viewer" etc.) will land without a schema change.
//!
//! Password changes are split in two:
//!   - `PUT /api/users/:id/password` — admin resets anyone's password
//!   - `PUT /api/auth/password` — any authenticated user changes their own
//!     (password-gated, and revokes all other sessions on success).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    auth::{self, audit, password, session, AdminUser, AuthUser},
    state::AppState,
};

const MIN_PASSWORD_LEN: usize = 8;

// ---------------------------------------------------------------------------
// GET /api/users
// ---------------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub totp_enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<UserRow>>, StatusCode> {
    sqlx::query_as::<_, UserRow>(
        "SELECT id, username, role, totp_enabled, created_at FROM users ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|err| {
        tracing::error!(%err, "users: list");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

// ---------------------------------------------------------------------------
// POST /api/users
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
}

pub async fn create(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    headers: HeaderMap,
    Json(body): Json<CreateUser>,
) -> Result<(StatusCode, Json<UserRow>), axum::response::Response> {
    let username = body.username.trim().to_owned();
    if username.is_empty() {
        return Err(bad("username_required", "username must not be empty"));
    }
    if body.password.len() < MIN_PASSWORD_LEN {
        return Err(bad("password_too_short", "password is too short"));
    }

    let hash = password::hash(&body.password).map_err(|err| {
        tracing::error!(%err, "users: hash");
        internal()
    })?;

    let row: Result<UserRow, sqlx::Error> = sqlx::query_as(
        r#"INSERT INTO users (username, password_hash, role)
           VALUES ($1, $2, 'admin')
           RETURNING id, username, role, totp_enabled, created_at"#,
    )
    .bind(&username)
    .bind(&hash)
    .fetch_one(&state.pool)
    .await;

    let row = match row {
        Ok(r) => r,
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(bad("username_taken", "username is already in use"))
        }
        Err(err) => {
            tracing::error!(%err, "users: insert");
            return Err(internal());
        }
    };

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "user.created",
        Some(&row.username),
        &meta,
    )
    .await;

    Ok((StatusCode::CREATED, Json(row)))
}

// ---------------------------------------------------------------------------
// DELETE /api/users/:id
// ---------------------------------------------------------------------------

pub async fn delete_one(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<StatusCode, axum::response::Response> {
    if id == session.user_id {
        return Err(bad("self_delete_forbidden", "you can't delete yourself"));
    }
    // Never leave the panel admin-less. If this would drop the last admin,
    // refuse and tell the caller to appoint a replacement first.
    let admin_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "users: count");
            internal()
        })?;
    if admin_count <= 1 {
        return Err(bad(
            "last_admin",
            "can't delete the last remaining administrator",
        ));
    }

    let removed: Option<(String,)> =
        sqlx::query_as("DELETE FROM users WHERE id = $1 RETURNING username")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "users: delete");
                internal()
            })?;
    let (username,) = removed.ok_or_else(not_found)?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "user.deleted",
        Some(&username),
        &meta,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// PUT /api/users/:id/password   (admin reset)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ResetPasswordBody {
    pub password: String,
}

pub async fn reset_password(
    State(state): State<AppState>,
    AdminUser(session): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
    Json(body): Json<ResetPasswordBody>,
) -> Result<StatusCode, axum::response::Response> {
    if body.password.len() < MIN_PASSWORD_LEN {
        return Err(bad("password_too_short", "password is too short"));
    }
    let hash = password::hash(&body.password).map_err(|err| {
        tracing::error!(%err, "users: hash reset");
        internal()
    })?;

    let updated: Option<(String,)> =
        sqlx::query_as("UPDATE users SET password_hash = $1 WHERE id = $2 RETURNING username")
            .bind(&hash)
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "users: reset pw");
                internal()
            })?;
    let (username,) = updated.ok_or_else(not_found)?;

    // Any active sessions belonging to the target account are now risky —
    // blow them away so the old password can't keep anyone signed in.
    if let Err(err) = session::revoke_all_for_user(&state.pool, id).await {
        tracing::warn!(%err, "users: revoke on reset");
    }

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "user.password_reset",
        Some(&username),
        &meta,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// PUT /api/auth/password   (self-serve change)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ChangeOwnPasswordBody {
    pub current_password: String,
    pub new_password: String,
}

pub async fn change_own_password(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    headers: HeaderMap,
    Json(body): Json<ChangeOwnPasswordBody>,
) -> Result<StatusCode, axum::response::Response> {
    if body.new_password.len() < MIN_PASSWORD_LEN {
        return Err(bad("password_too_short", "password is too short"));
    }
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(session.user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "pw change: load");
            internal()
        })?;
    match password::verify(&body.current_password, &hash) {
        Ok(true) => {}
        Ok(false) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    code: "invalid_password",
                    message: "current password is incorrect",
                }),
            )
                .into_response());
        }
        Err(err) => {
            tracing::error!(%err, "pw change: verify");
            return Err(internal());
        }
    }

    let new_hash = password::hash(&body.new_password).map_err(|err| {
        tracing::error!(%err, "pw change: hash");
        internal()
    })?;
    sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
        .bind(&new_hash)
        .bind(session.user_id)
        .execute(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "pw change: persist");
            internal()
        })?;

    // Rotate *other* sessions (we'll keep the current one alive via the
    // cookie we set back, but for self-serve pw change the simplest thing
    // is to revoke-all and let the caller re-auth next request. We revoke
    // everything for safety.)
    if let Err(err) = session::revoke_all_for_user(&state.pool, session.user_id).await {
        tracing::warn!(%err, "pw change: revoke all");
    }

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "user.password_changed",
        Some(&session.username),
        &meta,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
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

fn not_found() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            code: "not_found",
            message: "user not found",
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
