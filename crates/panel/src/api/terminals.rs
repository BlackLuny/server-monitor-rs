//! Terminal session listings + recording streaming.
//!
//! Recordings live on the agent's filesystem. The metadata endpoint surfaces
//! path / size / sha256, and `download` proxies a streamed `.cast` via the
//! existing gRPC channel — so admins can play back `asciinema play <file>`
//! without ever shelling into the host.

#![allow(clippy::result_large_err, clippy::type_complexity)]

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::stream::Stream;
use monitor_proto::v1::{
    panel_to_agent::Payload as DownPayload, PanelToAgent, RecordingFetchChunk,
    RecordingFetchRequest,
};
use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::terminal::recordings::FetchGuard;

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

/// Stream the .cast file for a closed session by routing a fetch through
/// the agent's existing gRPC channel.
pub async fn download_recording(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Response, Response> {
    let row: Option<(Uuid, Option<String>, Option<i64>)> = sqlx::query_as(
        r#"SELECT s.agent_id, t.recording_path, t.recording_size
             FROM terminal_sessions t
             JOIN servers s ON s.id = t.server_id
             WHERE t.id = $1"#,
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;

    let Some((agent_id, path, size)) = row else {
        return Err((StatusCode::NOT_FOUND, "session not found").into_response());
    };
    if path.as_deref().map(str::is_empty).unwrap_or(true) {
        return Err((StatusCode::NOT_FOUND, "no recording captured").into_response());
    }

    // Agent must be online — recordings live on its disk; the panel does
    // not cache them. Reflect this honestly so the UI can prompt the
    // admin to wake the host.
    let Some(session) = state.hub.get(&agent_id) else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "agent offline").into_response());
    };

    let session_str = session_id.to_string();
    let (rx, guard) = state.recording_hub.open(session_str.clone());

    let request = PanelToAgent {
        seq: 0,
        payload: Some(DownPayload::RecordingFetch(RecordingFetchRequest {
            session_id: session_str.clone(),
        })),
    };
    if !session.try_send(request) {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "agent channel full").into_response());
    }

    let body_stream = chunks_to_body_stream(rx, guard);
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-asciinema-recording")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{session_id}.cast\""),
        )
        .body(Body::from_stream(body_stream))
        .map_err(|err| {
            tracing::error!(%err, "failed to build streamed response");
            (StatusCode::INTERNAL_SERVER_ERROR, "stream init failed").into_response()
        })?;

    if let Some(bytes) = size.filter(|&n| n > 0) {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            bytes.to_string().parse().expect("u64 → header"),
        );
    }
    Ok(response)
}

/// State threaded through `futures::stream::unfold` so the [`FetchGuard`]
/// drops with the stream — that removes the slot from the hub the moment
/// the client disconnects.
struct StreamState {
    rx: tokio::sync::mpsc::Receiver<RecordingFetchChunk>,
    _guard: FetchGuard,
    done: bool,
}

/// Turn the agent-side chunked protocol into a byte stream Axum can serve.
/// Stops at the first frame with `eof = true`. Errors are surfaced as a
/// stream-level `io::Error`, which truncates the body so the client sees
/// an incomplete download rather than a silent success.
fn chunks_to_body_stream(
    rx: tokio::sync::mpsc::Receiver<RecordingFetchChunk>,
    guard: FetchGuard,
) -> impl Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    let init = StreamState {
        rx,
        _guard: guard,
        done: false,
    };
    futures::stream::unfold(init, |mut state| async move {
        if state.done {
            return None;
        }
        match state.rx.recv().await {
            Some(chunk) => {
                if !chunk.error.is_empty() {
                    state.done = true;
                    return Some((Err(std::io::Error::other(chunk.error)), state));
                }
                if chunk.eof {
                    state.done = true;
                    if chunk.data.is_empty() {
                        return None;
                    }
                    return Some((Ok(bytes::Bytes::from(chunk.data)), state));
                }
                Some((Ok(bytes::Bytes::from(chunk.data)), state))
            }
            None => {
                state.done = true;
                Some((
                    Err(std::io::Error::other("recording stream ended early")),
                    state,
                ))
            }
        }
    })
}

fn internal(err: sqlx::Error) -> Response {
    tracing::error!(%err, "terminals db error");
    (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
}
