//! DB-backed session store.
//!
//! Every login inserts a `login_sessions` row; every authenticated request
//! validates the cookie against this table. The cookie content itself is just
//! the primary key — opaque, 32 bytes of OS randomness, base64url-encoded.
//!
//! Validity window: `revoked_at IS NULL AND last_used_at > NOW() - 7 days`.
//! Each validation bumps `last_used_at` so active sessions stay alive.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use sqlx::PgPool;
use time::OffsetDateTime;

/// Sliding window — matches the 7-day decision from the M3 design.
pub const SESSION_TTL_DAYS: i64 = 7;

/// Cookie + header name used throughout the auth layer.
pub const COOKIE_NAME: &str = "monitor_session";

/// Generate a 32-byte URL-safe opaque session id.
#[must_use]
pub fn new_session_id() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Metadata captured at login time — used for audit + /sessions listing later.
#[derive(Debug, Default, Clone)]
pub struct SessionMeta {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// A successfully validated session, as returned by the auth extractor.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: OffsetDateTime,
}

/// Insert a new session row. Returns the opaque id to place in the cookie.
pub async fn create(pool: &PgPool, user_id: i64, meta: &SessionMeta) -> sqlx::Result<String> {
    let id = new_session_id();
    sqlx::query(
        r#"INSERT INTO login_sessions (id, user_id, ip, user_agent)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(&id)
    .bind(user_id)
    .bind(meta.ip.as_deref())
    .bind(meta.user_agent.as_deref())
    .execute(pool)
    .await?;
    Ok(id)
}

/// Look up a session and atomically refresh `last_used_at`. Returns `None` if
/// the session is unknown, revoked, or past the sliding window.
///
/// The `RETURNING` trick lets us filter + touch in a single round trip.
pub async fn validate_and_touch(pool: &PgPool, session_id: &str) -> sqlx::Result<Option<Session>> {
    sqlx::query_as::<_, Session>(
        r#"
        WITH touched AS (
            UPDATE login_sessions
               SET last_used_at = NOW()
             WHERE id = $1
               AND revoked_at IS NULL
               AND last_used_at > NOW() - ($2 || ' days')::interval
            RETURNING id, user_id, created_at, last_used_at
        )
        SELECT t.id, t.user_id, u.username, u.role, t.created_at, t.last_used_at
          FROM touched t
          JOIN users u ON u.id = t.user_id
        "#,
    )
    .bind(session_id)
    .bind(SESSION_TTL_DAYS.to_string())
    .fetch_optional(pool)
    .await
}

/// Revoke a specific session (logout). No error if already revoked or missing.
pub async fn revoke(pool: &PgPool, session_id: &str) -> sqlx::Result<()> {
    sqlx::query(
        r#"UPDATE login_sessions
              SET revoked_at = NOW()
            WHERE id = $1 AND revoked_at IS NULL"#,
    )
    .bind(session_id)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Revoke every active session for a user (e.g. password reset).
pub async fn revoke_all_for_user(pool: &PgPool, user_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        r#"UPDATE login_sessions
              SET revoked_at = NOW()
            WHERE user_id = $1 AND revoked_at IS NULL"#,
    )
    .bind(user_id)
    .execute(pool)
    .await
    .map(|_| ())
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for Session {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            username: row.try_get("username")?,
            role: row.try_get("role")?,
            created_at: row.try_get("created_at")?,
            last_used_at: row.try_get("last_used_at")?,
        })
    }
}
