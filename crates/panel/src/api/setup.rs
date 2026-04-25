//! First-run setup wizard.
//!
//! Two endpoints, both unauthenticated — they have to be, since there is no
//! admin yet when the panel boots fresh:
//!
//! - `GET /api/setup/status` — lets the SvelteKit shell decide whether to
//!   redirect to `/setup` or `/login` on a cold load.
//! - `POST /api/setup`      — creates the very first admin. Idempotent: once
//!   any user exists, the endpoint always returns 403.
//!
//! The write is racey-by-construction (two tabs hitting the button at once),
//! so we use `INSERT ... WHERE NOT EXISTS` instead of a read-then-write check.
//! Postgres turns that into a single atomic statement — one writer wins, the
//! other gets zero rows back and sees `already_initialized`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};

use crate::{
    auth::{self, cookie, password, session},
    state::AppState,
};

/// Minimum password length enforced on the server. The UI mirrors this rule
/// so users get feedback before the round trip.
const MIN_PASSWORD_LEN: usize = 8;

// ---------------------------------------------------------------------------
// GET /api/setup/status
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SetupStatus {
    pub initialized: bool,
}

pub async fn status(State(state): State<AppState>) -> Result<Json<SetupStatus>, StatusCode> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "setup: status query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(SetupStatus {
        initialized: exists,
    }))
}

// ---------------------------------------------------------------------------
// POST /api/setup
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct SetupResponse {
    pub user_id: i64,
    pub username: String,
    pub role: String,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<SetupRequest>,
) -> impl IntoResponse {
    let username = body.username.trim().to_owned();
    if username.is_empty() {
        return bad_request("username_required", "username must not be empty");
    }
    if body.password.len() < MIN_PASSWORD_LEN {
        return bad_request(
            "password_too_short",
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        );
    }

    let password_hash = match password::hash(&body.password) {
        Ok(h) => h,
        Err(err) => {
            tracing::error!(%err, "setup: password hashing failed");
            return internal_error();
        }
    };

    // Atomic first-writer-wins: the row inserts only when `users` is empty.
    // A lost race returns zero rows, so `fetch_optional` → None.
    let created: Option<(i64, String, String)> = match sqlx::query_as(
        r#"INSERT INTO users (username, password_hash, role)
           SELECT $1, $2, 'admin'
            WHERE NOT EXISTS (SELECT 1 FROM users)
           RETURNING id, username, role"#,
    )
    .bind(&username)
    .bind(&password_hash)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(%err, "setup: insert failed");
            return internal_error();
        }
    };

    let Some((user_id, username, role)) = created else {
        return (
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                code: "already_initialized",
                message: "setup has already been completed".into(),
            }),
        )
            .into_response();
    };

    // Auto-login: the wizard shouldn't send users to a login page right after
    // they picked a password. Start a session and set the cookie.
    let meta = auth::session_meta(&headers);
    let sid = match session::create(&state.pool, user_id, &meta).await {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(%err, "setup: session creation failed");
            return internal_error();
        }
    };

    // Audit trail — task 11 will formalize this behind a helper.
    if let Err(err) = sqlx::query(
        r#"INSERT INTO audit_log (user_id, action, target, ip, user_agent)
           VALUES ($1, 'setup.admin_created', $2, $3, $4)"#,
    )
    .bind(user_id)
    .bind(&username)
    .bind(meta.ip.as_deref())
    .bind(meta.user_agent.as_deref())
    .execute(&state.pool)
    .await
    {
        // Non-fatal: setup already succeeded, the session is live.
        tracing::warn!(%err, "setup: audit write failed");
    }

    let jar = jar.add(cookie::build(sid, state.cookies_secure));
    (
        StatusCode::CREATED,
        jar,
        Json(SetupResponse {
            user_id,
            username,
            role,
        }),
    )
        .into_response()
}

fn bad_request(code: &'static str, message: impl Into<String>) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            code,
            message: message.into(),
        }),
    )
        .into_response()
}

fn internal_error() -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            code: "internal_error",
            message: "an internal error occurred".into(),
        }),
    )
        .into_response()
}
