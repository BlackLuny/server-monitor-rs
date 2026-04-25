//! `GET /ws/terminal/:server_id` — bridge a browser xterm.js to an agent pty.
//!
//! Wire protocol on the WS side (kept tiny so the browser code stays simple):
//!   - **binary frames** from the client → agent stdin
//!   - **text frames** are JSON control messages: `{"type":"resize","cols":..,"rows":..}`
//!   - first text frame from client may be `{"type":"open","cols":..,"rows":..,"shell":".."}`
//!     to override the initial pty geometry; the server also accepts the
//!     `?cols=&rows=` query string fallback for browsers that race the WS
//!     ready state.
//!   - **binary frames** from agent → client (raw stdout bytes)
//!   - **text frame** from server before close: `{"type":"closed","exit_code":0,"error":"...","recording":{..}}`
//!
//! Everything else (session row, audit, recording metadata) is owned by
//! [`crate::terminal::TerminalHub`].

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use monitor_proto::v1::{
    panel_to_agent::Payload as DownPayload, PanelToAgent, TerminalClose, TerminalInput,
    TerminalOpen, TerminalResize,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    auth::{request::session_meta, AdminUser},
    settings,
    state::AppState,
    terminal::{Frame, MAX_SESSIONS_PER_USER},
};

#[derive(Deserialize)]
pub struct TerminalQuery {
    #[serde(default)]
    pub cols: Option<u32>,
    #[serde(default)]
    pub rows: Option<u32>,
    #[serde(default)]
    pub shell: Option<String>,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(server_id): Path<i64>,
    Query(q): Query<TerminalQuery>,
    headers: HeaderMap,
    AdminUser(session): AdminUser,
) -> Response {
    // Per-user concurrency cap, enforced before we accept the upgrade so a
    // client gets a real 429 instead of a hung socket.
    if state.terminal_hub.count_for_user(session.user_id) >= MAX_SESSIONS_PER_USER {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "session limit reached for this user",
        )
            .into_response();
    }

    let row: Option<(Uuid, bool, String)> = match sqlx::query_as(
        "SELECT agent_id, terminal_enabled, ssh_recording FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(%err, "server lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db").into_response();
        }
    };

    let Some((agent_id, terminal_enabled, ssh_recording)) = row else {
        return (StatusCode::NOT_FOUND, "no such server").into_response();
    };

    if !terminal_enabled {
        return (StatusCode::FORBIDDEN, "terminal disabled for this server").into_response();
    }

    let agent_session = match state.hub.get(&agent_id) {
        Some(s) => s,
        None => {
            return (StatusCode::SERVICE_UNAVAILABLE, "agent offline").into_response();
        }
    };

    // Recording policy: column overrides global default.
    let record = match ssh_recording.as_str() {
        "on" => true,
        "off" => false,
        _ => settings::ssh_recording_default(&state.pool)
            .await
            .unwrap_or(false),
    };

    let meta = session_meta(&headers);
    let user_id = session.user_id;

    ws.on_upgrade(move |socket| async move {
        let session_id = Uuid::new_v4();
        let rx = match state
            .terminal_hub
            .open(
                &state.pool,
                session_id,
                server_id,
                agent_id,
                Some(user_id),
                &meta,
            )
            .await
        {
            Ok(rx) => rx,
            Err(err) => {
                tracing::error!(%err, "terminal session insert failed");
                return;
            }
        };

        // Send TerminalOpen to the agent. If the agent stream is full
        // give up cleanly — better to surface a quick failure than to
        // dangle a half-open session row.
        let cols = q.cols.unwrap_or(80).max(1);
        let rows = q.rows.unwrap_or(24).max(1);
        let shell = q.shell.unwrap_or_default();
        let open_msg = PanelToAgent {
            seq: 0,
            payload: Some(DownPayload::TerminalOpen(TerminalOpen {
                session_id: session_id.to_string(),
                cols,
                rows,
                shell,
                record,
            })),
        };
        if !agent_session.try_send(open_msg) {
            tracing::warn!(%session_id, "agent stream full — aborting terminal open");
            state
                .terminal_hub
                .close_from_panel(&state.pool, &session_id.to_string(), "agent stream full")
                .await;
            return;
        }

        run_bridge(socket, state, agent_session, session_id, rx).await;
    })
}

async fn run_bridge(
    mut socket: WebSocket,
    state: AppState,
    agent_session: crate::grpc::AgentSession,
    session_id: Uuid,
    mut rx: tokio::sync::mpsc::Receiver<Frame>,
) {
    let session_id_str = session_id.to_string();

    let closed_reason: &'static str = loop {
        tokio::select! {
            // Agent → browser.
            frame = rx.recv() => {
                match frame {
                    Some(Frame::Output(bytes)) => {
                        if socket.send(Message::Binary(bytes)).await.is_err() {
                            break "ws closed";
                        }
                    }
                    Some(Frame::Closed(info)) => {
                        let payload = json!({
                            "type": "closed",
                            "exit_code": info.exit_code,
                            "error": info.error,
                            "recording": {
                                "path": info.recording_path,
                                "size": info.recording_size,
                                "sha256": info.recording_sha256,
                            },
                        });
                        let _ = socket.send(Message::Text(payload.to_string())).await;
                        return;
                    }
                    None => {
                        // Hub dropped — should be rare; treat as closed.
                        return;
                    }
                }
            }
            // Browser → agent.
            msg = socket.recv() => {
                match msg {
                    None => break "client disconnected",
                    Some(Err(_)) => break "ws error",
                    Some(Ok(Message::Close(_))) => break "client closed",
                    Some(Ok(Message::Binary(data))) => {
                        let msg = PanelToAgent {
                            seq: 0,
                            payload: Some(DownPayload::TerminalInput(TerminalInput {
                                session_id: session_id_str.clone(),
                                data,
                            })),
                        };
                        if !agent_session.try_send(msg) {
                            tracing::warn!(%session_id, "agent stream full — dropping stdin");
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if let Some((kind, cols, rows)) = parse_resize(&text) {
                            let msg = PanelToAgent {
                                seq: 0,
                                payload: Some(DownPayload::TerminalResize(TerminalResize {
                                    session_id: session_id_str.clone(),
                                    cols,
                                    rows,
                                })),
                            };
                            if kind == "resize" {
                                let _ = agent_session.try_send(msg);
                            }
                        }
                    }
                    // Pings / pongs are handled by axum automatically.
                    Some(Ok(_)) => {}
                }
            }
        }
    };

    // We exited the loop because the WS side ended; tell the agent to
    // close the pty and seal the DB row.
    let close_msg = PanelToAgent {
        seq: 0,
        payload: Some(DownPayload::TerminalClose(TerminalClose {
            session_id: session_id_str.clone(),
        })),
    };
    let _ = agent_session.try_send(close_msg);
    state
        .terminal_hub
        .close_from_panel(&state.pool, &session_id_str, closed_reason)
        .await;
}

/// Parse `{"type":"resize","cols":N,"rows":M}` into a tuple. Anything else
/// returns `None` so the bridge can ignore it without exploding.
fn parse_resize(text: &str) -> Option<(&'static str, u32, u32)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let kind = v.get("type")?.as_str()?;
    if kind != "resize" {
        return None;
    }
    let cols = v.get("cols").and_then(|x| x.as_u64())? as u32;
    let rows = v.get("rows").and_then(|x| x.as_u64())? as u32;
    Some(("resize", cols.max(1), rows.max(1)))
}
