//! Login, logout, and `whoami` endpoints.
//!
//! Login is two-stage *conditional on enrollment*. If the account has
//! `totp_enabled = false`, password alone is enough. If it's true, the first
//! request with a valid password comes back as `totp_required` so the client
//! can prompt for the code; the next request carries it. Backup codes are
//! accepted in the same field and consumed on match.
//!
//! Every terminal state writes an audit row via `auth::audit::record`.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    auth::{self, audit, backup_codes, cookie, password, session, totp, AuthUser},
    state::AppState,
};

// ---------------------------------------------------------------------------
// POST /api/auth/login
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// Either a six-digit TOTP code or a backup code. Only consulted when the
    /// account has `totp_enabled = true`; the field is ignored otherwise.
    #[serde(default)]
    pub totp_code: Option<String>,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub totp_enabled: bool,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

struct UserRow {
    id: i64,
    username: String,
    role: String,
    password_hash: String,
    totp_enabled: bool,
    totp_secret: Option<String>,
    backup_codes: Value,
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    let meta = auth::session_meta(&headers);

    // Look up without early-returning so the failure path stays timing-
    // equivalent for unknown-user and known-user-wrong-password.
    let row: Option<UserRow> =
        match sqlx::query_as::<_, (i64, String, String, String, bool, Option<String>, Value)>(
            r#"SELECT id, username, role, password_hash, totp_enabled, totp_secret, backup_codes
             FROM users
            WHERE username = $1"#,
        )
        .bind(body.username.trim())
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(t)) => Some(UserRow {
                id: t.0,
                username: t.1,
                role: t.2,
                password_hash: t.3,
                totp_enabled: t.4,
                totp_secret: t.5,
                backup_codes: t.6,
            }),
            Ok(None) => None,
            Err(err) => {
                tracing::error!(%err, "login: user lookup failed");
                return internal_error();
            }
        };

    let (user_id, username, role, pw_ok, totp_enabled, totp_secret, backup_codes_json) = match row {
        Some(u) => (
            Some(u.id),
            u.username,
            u.role,
            match password::verify(&body.password, &u.password_hash) {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!(%err, "login: password verify failed");
                    return internal_error();
                }
            },
            u.totp_enabled,
            u.totp_secret,
            u.backup_codes,
        ),
        None => {
            // Burn comparable CPU so the caller can't distinguish outcomes
            // by latency.
            let _ = password::verify(&body.password, DUMMY_HASH);
            (
                None,
                body.username.trim().to_owned(),
                String::new(),
                false,
                false,
                None,
                Value::Array(vec![]),
            )
        }
    };

    if !pw_ok {
        audit::record(
            &state.pool,
            user_id,
            "auth.login.failure",
            Some(&username),
            &meta,
        )
        .await;
        return invalid_credentials();
    }

    // pw_ok=true implies a real row.
    let Some(user_id) = user_id else {
        return invalid_credentials();
    };

    if totp_enabled {
        let code = body.totp_code.as_deref().unwrap_or("").trim();
        if code.is_empty() {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    code: "totp_required",
                    message: "enter the six-digit code from your authenticator app",
                }),
            )
                .into_response();
        }

        // Prefer the generator; fall back to single-use backup codes. A
        // single successful backup code must be burned from the stored list.
        let totp_ok = totp_secret
            .as_deref()
            .map(|s| totp::verify(s, code))
            .unwrap_or(false);

        let used_backup = if totp_ok {
            None
        } else {
            backup_codes::consume(&backup_codes_json, code)
        };

        if !totp_ok && used_backup.is_none() {
            audit::record(
                &state.pool,
                Some(user_id),
                "auth.login.failure",
                Some(&username),
                &meta,
            )
            .await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    code: "invalid_totp",
                    message: "that code is not valid",
                }),
            )
                .into_response();
        }

        if let Some(updated) = used_backup {
            if let Err(err) = sqlx::query("UPDATE users SET backup_codes = $1 WHERE id = $2")
                .bind(&updated)
                .bind(user_id)
                .execute(&state.pool)
                .await
            {
                tracing::error!(%err, "login: backup code consume failed");
                return internal_error();
            }
            audit::record(
                &state.pool,
                Some(user_id),
                "auth.backup_code_used",
                Some(&username),
                &meta,
            )
            .await;
        }
    }

    let sid = match session::create(&state.pool, user_id, &meta).await {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(%err, "login: session creation failed");
            return internal_error();
        }
    };

    audit::record(
        &state.pool,
        Some(user_id),
        "auth.login.success",
        Some(&username),
        &meta,
    )
    .await;

    let jar = jar.add(cookie::build(sid, state.cookies_secure));
    (
        StatusCode::OK,
        jar,
        Json(MeResponse {
            user_id,
            username,
            role,
            totp_enabled,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /api/auth/logout
// ---------------------------------------------------------------------------

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> impl IntoResponse {
    if let Some(c) = jar.get(session::COOKIE_NAME) {
        let sid = c.value().to_owned();
        if let Err(err) = session::revoke(&state.pool, &sid).await {
            tracing::error!(%err, "logout: revoke failed");
        }
    }
    let meta = auth::session_meta(&headers);
    audit::record(&state.pool, None, "auth.logout", None, &meta).await;
    let jar = jar.add(cookie::clear(state.cookies_secure));
    (StatusCode::NO_CONTENT, jar).into_response()
}

// ---------------------------------------------------------------------------
// GET /api/auth/me
// ---------------------------------------------------------------------------

pub async fn me(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Result<Json<MeResponse>, StatusCode> {
    let row: (bool,) = sqlx::query_as("SELECT totp_enabled FROM users WHERE id = $1")
        .bind(session.user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "me: user fetch failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(MeResponse {
        user_id: session.user_id,
        username: session.username,
        role: session.role,
        totp_enabled: row.0,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Pre-hashed argon2id output so the unknown-user branch spends comparable
/// CPU to the real verify path.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MTIzNDU2Nzg5MGFiY2RlZg$CQvkAKEs0pHUaLL+7pOBKNW8Ic5GoNsMW4bX/KSWNAc";

fn invalid_credentials() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorBody {
            code: "invalid_credentials",
            message: "username or password is incorrect",
        }),
    )
        .into_response()
}

fn internal_error() -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            code: "internal_error",
            message: "an internal error occurred",
        }),
    )
        .into_response()
}
