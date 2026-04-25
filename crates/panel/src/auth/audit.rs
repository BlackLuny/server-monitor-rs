//! Shared helper for writing admin-action rows to `audit_log`.
//!
//! Keeping this centralized lets every endpoint use the same columns (and
//! forces us to look at the same list when adding a new one), while audit
//! failures remain non-fatal — they log at `warn` so operators see them but
//! user-facing flow doesn't break.

use sqlx::PgPool;

use super::SessionMeta;

/// Insert one audit row. A failure here is logged but never bubbled up —
/// audit records are a best-effort trail, never on the critical path of a
/// successful user action.
pub async fn record(
    pool: &PgPool,
    user_id: Option<i64>,
    action: &str,
    target: Option<&str>,
    meta: &SessionMeta,
) {
    if let Err(err) = sqlx::query(
        r#"INSERT INTO audit_log (user_id, action, target, ip, user_agent)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(user_id)
    .bind(action)
    .bind(target)
    .bind(meta.ip.as_deref())
    .bind(meta.user_agent.as_deref())
    .execute(pool)
    .await
    {
        tracing::warn!(%err, action, "audit write failed");
    }
}
