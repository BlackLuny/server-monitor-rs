//! Lightweight `GET /api/agents` listing — what /probes/:id and similar
//! pages call to populate the "select agents" pickers. Returns just the
//! handful of fields those UIs need.

use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::{auth::AdminUser, state::AppState};

#[derive(Serialize, sqlx::FromRow)]
pub struct AgentRow {
    pub agent_id: String,
    pub display_name: String,
    pub online: bool,
    pub group_name: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<AgentRow>>, StatusCode> {
    sqlx::query_as::<_, (uuid::Uuid, String, bool, Option<String>)>(
        r#"SELECT s.agent_id,
                  s.display_name,
                  (s.last_seen_at IS NOT NULL
                   AND s.last_seen_at > NOW() - INTERVAL '60 seconds') AS online,
                  g.name
             FROM servers s
             LEFT JOIN server_groups g ON g.id = s.group_id
            ORDER BY s.display_name"#,
    )
    .fetch_all(&state.pool)
    .await
    .map(|rows| {
        Json(
            rows.into_iter()
                .map(|(uid, name, online, group)| AgentRow {
                    agent_id: uid.to_string(),
                    display_name: name,
                    online,
                    group_name: group,
                })
                .collect(),
        )
    })
    .map_err(|err| {
        tracing::error!(%err, "agents: list");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}
