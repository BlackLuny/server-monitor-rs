//! Terminal session listings + recording metadata.
//!
//! M5 simplification: recordings live on the agent's filesystem. This
//! endpoint surfaces the metadata (path / size / sha256) so an operator can
//! retrieve a `.cast` over SSH and play it back with
//! `asciinema play /var/lib/monitor-agent/recordings/<id>.cast`.
//! Streamed downloads are scheduled for M5.1, which will add a dedicated
//! gRPC fetch RPC so the panel can range-request chunks.

#![allow(clippy::result_large_err, clippy::type_complexity)]

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{auth::AdminUser, state::AppState};

#[derive(Serialize, FromRow)]
pub struct TerminalSessionRow {
    pub id: Uuid,
    pub server_id: i64,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub opened_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub recording_path: Option<String>,
    pub recording_size: Option<i64>,
    pub recording_sha256: Option<String>,
    pub client_ip: Option<String>,
}

pub async fn list_for_server(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(server_id): Path<i64>,
) -> Result<Json<Vec<TerminalSessionRow>>, Response> {
    let rows = sqlx::query_as::<_, TerminalSessionRow>(
        r#"SELECT t.id, t.server_id, t.user_id, u.username,
                  t.opened_at, t.closed_at, t.exit_code, t.error,
                  t.recording_path, t.recording_size, t.recording_sha256,
                  t.client_ip
             FROM terminal_sessions t
             LEFT JOIN users u ON u.id = t.user_id
             WHERE t.server_id = $1
             ORDER BY t.opened_at DESC
             LIMIT 50"#,
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(rows))
}

#[derive(Serialize)]
pub struct RecordingResponse {
    pub session_id: Uuid,
    pub server_id: i64,
    pub agent_id: Uuid,
    pub recording_path: Option<String>,
    pub recording_size: Option<i64>,
    pub recording_sha256: Option<String>,
    /// One-line hint surfaced in the UI so admins can fetch+play recordings
    /// today even though streamed download is M5.1.
    pub fetch_hint: String,
}

pub async fn recording(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<RecordingResponse>, Response> {
    let row: Option<(i64, Uuid, Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        r#"SELECT t.server_id, s.agent_id,
                  t.recording_path, t.recording_size, t.recording_sha256
             FROM terminal_sessions t
             JOIN servers s ON s.id = t.server_id
             WHERE t.id = $1"#,
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let Some((server_id, agent_id, path, size, sha)) = row else {
        return Err((StatusCode::NOT_FOUND, "session not found").into_response());
    };

    let hint = match path.as_deref() {
        Some(p) => format!("ssh <agent-host> 'asciinema play {p}'"),
        None => "no recording captured for this session".to_owned(),
    };
    Ok(Json(RecordingResponse {
        session_id,
        server_id,
        agent_id,
        recording_path: path,
        recording_size: size,
        recording_sha256: sha,
        fetch_hint: hint,
    }))
}

fn internal(err: sqlx::Error) -> Response {
    tracing::error!(%err, "terminals db error");
    (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
}
