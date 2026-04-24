//! `GET /ws/live` — WebSocket endpoint streaming live metric updates.
//!
//! Protocol: a single outbound JSON message per metric update. The initial
//! implementation is one-way (server → client); the client does not send
//! anything back. A future M5 PR will extend this socket for terminal IO,
//! probably switching to a tagged-variant protocol with `type` discrimination
//! that `LiveUpdate` already uses.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use serde::Deserialize;

use crate::{live::LiveUpdate, state::AppState};

#[derive(Deserialize)]
pub struct LiveQuery {
    #[serde(default)]
    pub guest: bool,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<LiveQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run(socket, state, q.guest))
}

async fn run(mut socket: WebSocket, state: AppState, guest: bool) {
    let mut rx = state.live.subscribe();
    loop {
        tokio::select! {
            // New metric update — forward to the client.
            incoming = rx.recv() => {
                match incoming {
                    Ok(update) => {
                        if guest && update.hidden_from_guest {
                            continue;
                        }
                        if !send_update(&mut socket, &update).await {
                            break;
                        }
                    }
                    // A slow subscriber lagged — keep going but log so we know.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "live ws subscriber lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // Client pings / closes / noise; keep reading so the connection
            // stays healthy and we notice when they go away.
            frame = socket.recv() => {
                match frame {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    // Ignore text / binary / ping / pong — the server is the
                    // only publisher in M2.
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

async fn send_update(socket: &mut WebSocket, update: &LiveUpdate) -> bool {
    match serde_json::to_string(update) {
        Ok(text) => socket.send(Message::Text(text)).await.is_ok(),
        Err(err) => {
            tracing::warn!(%err, "failed to serialize live update");
            true
        }
    }
}
