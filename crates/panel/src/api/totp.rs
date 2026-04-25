//! TOTP enrollment & management for the logged-in user.
//!
//! Flow from the UI:
//!   1. `POST /api/auth/totp/enroll`  — user clicks "set up 2FA"; we generate
//!      a secret, stash it on their row (with `totp_enabled=false`), and
//!      return the otpauth URL + inline SVG QR. If they abandon the flow,
//!      nothing needs cleaning up — the secret just sits there and can be
//!      overwritten by a retry or cleared by `disable`.
//!   2. `POST /api/auth/totp/confirm` — user sends the first six-digit code;
//!      we verify, flip the flag, and hand back the ten backup codes. This
//!      is the only time they see those plaintext.
//!   3. `POST /api/auth/totp/disable` — password-gated tear-down.
//!   4. `POST /api/auth/totp/regenerate-backup` — password-gated reroll of
//!      the ten codes without touching the TOTP secret.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    auth::{self, audit, backup_codes, password, totp, AuthUser},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Response/error shapes
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
pub struct EnrollResponse {
    pub secret: String,
    pub otpauth_url: String,
    pub qr_svg_data_url: String,
}

#[derive(Serialize)]
pub struct ConfirmResponse {
    pub totp_enabled: bool,
    pub backup_codes: Vec<String>,
}

#[derive(Serialize)]
pub struct BackupCodesResponse {
    pub backup_codes: Vec<String>,
}

// ---------------------------------------------------------------------------
// POST /api/auth/totp/enroll
// ---------------------------------------------------------------------------

pub async fn enroll(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
) -> Result<Json<EnrollResponse>, axum::response::Response> {
    let secret = totp::new_secret();
    let url = totp::provisioning_url(&secret, &session.username).map_err(|err| {
        tracing::error!(%err, "enroll: provisioning url failed");
        internal()
    })?;
    let svg = totp::provisioning_qr_svg(&url);
    let qr = totp::qr_data_url(&svg);

    // We overwrite any previous pending secret on purpose — starting over
    // is a reasonable user action and keeping the old one around would
    // allow confirmation from a stale generator.
    sqlx::query(
        r#"UPDATE users
              SET totp_secret  = $1,
                  totp_enabled = FALSE
            WHERE id = $2"#,
    )
    .bind(&secret)
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "enroll: persist failed");
        internal()
    })?;

    Ok(Json(EnrollResponse {
        secret,
        otpauth_url: url,
        qr_svg_data_url: qr,
    }))
}

// ---------------------------------------------------------------------------
// POST /api/auth/totp/confirm
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ConfirmRequest {
    pub code: String,
}

pub async fn confirm(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    headers: HeaderMap,
    Json(body): Json<ConfirmRequest>,
) -> Result<Json<ConfirmResponse>, axum::response::Response> {
    let (secret, already_on): (Option<String>, bool) =
        sqlx::query_as("SELECT totp_secret, totp_enabled FROM users WHERE id = $1")
            .bind(session.user_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(%err, "confirm: load user");
                internal()
            })?;

    if already_on {
        return Err(bad_request(
            "already_enabled",
            "TOTP is already enabled on this account",
        ));
    }
    let Some(secret) = secret else {
        return Err(bad_request(
            "no_pending_enrollment",
            "start enrollment before confirming",
        ));
    };
    if !totp::verify(&secret, body.code.trim()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                code: "invalid_totp",
                message: "that code is not valid",
            }),
        )
            .into_response());
    }

    // Freshly generate backup codes. argon2-hash for storage, show
    // plaintext exactly once in the response.
    let plain = backup_codes::generate_plaintext();
    let hashes = backup_codes::hash_all(&plain).map_err(|err| {
        tracing::error!(%err, "confirm: hash backup codes");
        internal()
    })?;
    let hashes_json = Value::Array(hashes.into_iter().map(Value::String).collect());

    sqlx::query(
        r#"UPDATE users
              SET totp_enabled = TRUE,
                  backup_codes = $1
            WHERE id = $2"#,
    )
    .bind(&hashes_json)
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "confirm: flip flag");
        internal()
    })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "totp.enabled",
        Some(&session.username),
        &meta,
    )
    .await;

    Ok(Json(ConfirmResponse {
        totp_enabled: true,
        backup_codes: plain,
    }))
}

// ---------------------------------------------------------------------------
// POST /api/auth/totp/disable   (password-gated)
// POST /api/auth/totp/regenerate-backup (password-gated)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PasswordGated {
    pub password: String,
}

async fn require_password(
    state: &AppState,
    user_id: i64,
    plain: &str,
) -> Result<(), axum::response::Response> {
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "password gate: load hash");
            internal()
        })?;
    match password::verify(plain, &hash) {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                code: "invalid_password",
                message: "incorrect password",
            }),
        )
            .into_response()),
        Err(err) => {
            tracing::error!(%err, "password gate: verify");
            Err(internal())
        }
    }
}

pub async fn disable(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    headers: HeaderMap,
    Json(body): Json<PasswordGated>,
) -> Result<StatusCode, axum::response::Response> {
    require_password(&state, session.user_id, &body.password).await?;

    sqlx::query(
        r#"UPDATE users
              SET totp_enabled = FALSE,
                  totp_secret  = NULL,
                  backup_codes = '[]'::jsonb
            WHERE id = $1"#,
    )
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(%err, "disable: clear totp");
        internal()
    })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "totp.disabled",
        Some(&session.username),
        &meta,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn regenerate_backup(
    State(state): State<AppState>,
    AuthUser(session): AuthUser,
    headers: HeaderMap,
    Json(body): Json<PasswordGated>,
) -> Result<Json<BackupCodesResponse>, axum::response::Response> {
    require_password(&state, session.user_id, &body.password).await?;

    // Only makes sense on an account that actually has TOTP on; otherwise
    // the codes are useless.
    let (on,): (bool,) = sqlx::query_as("SELECT totp_enabled FROM users WHERE id = $1")
        .bind(session.user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "regen: totp state");
            internal()
        })?;
    if !on {
        return Err(bad_request(
            "totp_not_enabled",
            "enable TOTP before regenerating backup codes",
        ));
    }

    let plain = backup_codes::generate_plaintext();
    let hashes = backup_codes::hash_all(&plain).map_err(|err| {
        tracing::error!(%err, "regen: hash");
        internal()
    })?;
    let json = Value::Array(hashes.into_iter().map(Value::String).collect());

    sqlx::query("UPDATE users SET backup_codes = $1 WHERE id = $2")
        .bind(&json)
        .bind(session.user_id)
        .execute(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(%err, "regen: persist");
            internal()
        })?;

    let meta = auth::session_meta(&headers);
    audit::record(
        &state.pool,
        Some(session.user_id),
        "totp.backup_regenerated",
        Some(&session.username),
        &meta,
    )
    .await;

    Ok(Json(BackupCodesResponse {
        backup_codes: plain,
    }))
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

fn bad_request(code: &'static str, message: &'static str) -> axum::response::Response {
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
