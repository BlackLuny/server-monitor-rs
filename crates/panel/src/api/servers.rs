//! Server CRUD / listing endpoints.
//!
//! - `GET  /api/servers`  — list all servers with hardware + latest metric sample.
//! - `POST /api/servers`  — create a new server row, return the install command.
//!
//! Both endpoints are currently unauthenticated (M3 will add login + role
//! filtering). For guest visibility we already honor `hidden_from_guest` via
//! the optional `?guest=true` query flag, which the SvelteKit shell passes
//! whenever the caller is not logged in.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::{settings, state::AppState};

// ---------------------------------------------------------------------------
// GET /api/servers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    /// When `true`, the caller is treated as a guest:
    /// - servers with `hidden_from_guest=true` are dropped, and
    /// - per-row hardware detail is omitted.
    #[serde(default)]
    pub guest: bool,
}

#[derive(Serialize)]
pub struct ServerList {
    pub servers: Vec<ServerRow>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct ServerRow {
    pub id: i64,
    pub agent_id: String,
    pub display_name: String,
    pub group_id: Option<i64>,
    pub group_name: Option<String>,
    pub tags: Value,
    pub hidden_from_guest: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    /// `true` when the agent has been heard from within the last 60 seconds.
    pub online: bool,
    /// `null` for guests.
    pub hardware: Option<Hardware>,
    /// Latest raw metric sample; `null` before the first one lands.
    pub latest: Option<LatestSample>,
    pub agent_version: Option<String>,
    pub location: Option<String>,
    pub flag_emoji: Option<String>,
}

#[derive(Serialize)]
pub struct Hardware {
    pub os: Option<String>,
    pub os_version: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<i32>,
    pub mem_bytes: Option<i64>,
    pub swap_bytes: Option<i64>,
    pub disk_bytes: Option<i64>,
    pub virtualization: Option<String>,
}

#[derive(Serialize)]
pub struct LatestSample {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub cpu_pct: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub swap_used: i64,
    pub swap_total: i64,
    pub load_1: f64,
    pub disk_used: i64,
    pub disk_total: i64,
    pub net_in_bps: i64,
    pub net_out_bps: i64,
    pub process_count: i32,
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ServerList>, ApiError> {
    let rows = sqlx::query_as::<_, ServerRowDb>(LIST_SQL)
        .bind(q.guest)
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::from)?;

    let servers = rows.into_iter().map(|r| r.into_response(q.guest)).collect();

    Ok(Json(ServerList {
        servers,
        updated_at: OffsetDateTime::now_utc(),
    }))
}

/// SQL joining servers + group name + latest raw metric sample in one pass.
/// `$1 = is_guest`: when true we drop hidden-from-guest rows server-side.
const LIST_SQL: &str = r#"
SELECT
    s.id,
    s.agent_id,
    s.display_name,
    s.group_id,
    g.name AS group_name,
    s.tags,
    s.hidden_from_guest,
    s.last_seen_at,
    (s.last_seen_at IS NOT NULL AND s.last_seen_at > NOW() - INTERVAL '60 seconds') AS online,
    s.hw_os, s.hw_os_version, s.hw_kernel, s.hw_arch,
    s.hw_cpu_model, s.hw_cpu_cores,
    s.hw_mem_bytes, s.hw_swap_bytes, s.hw_disk_bytes,
    s.hw_virtualization,
    s.agent_version,
    s.location, s.flag_emoji,
    latest.ts,
    latest.cpu_pct, latest.mem_used, latest.mem_total,
    latest.swap_used, latest.swap_total, latest.load_1,
    latest.disk_used, latest.disk_total,
    latest.net_in_bps, latest.net_out_bps,
    latest.process_count
FROM servers s
LEFT JOIN server_groups g ON g.id = s.group_id
LEFT JOIN LATERAL (
    SELECT ts, cpu_pct, mem_used, mem_total, swap_used, swap_total,
           load_1, disk_used, disk_total, net_in_bps, net_out_bps, process_count
    FROM metric_snapshots
    WHERE server_id = s.id AND granularity = 'raw'
    ORDER BY ts DESC
    LIMIT 1
) latest ON TRUE
WHERE NOT ($1::bool AND s.hidden_from_guest)
ORDER BY COALESCE(g.order_idx, 0), s.order_idx, s.id
"#;

#[derive(sqlx::FromRow)]
struct ServerRowDb {
    id: i64,
    agent_id: uuid::Uuid,
    display_name: String,
    group_id: Option<i64>,
    group_name: Option<String>,
    tags: Value,
    hidden_from_guest: bool,
    last_seen_at: Option<OffsetDateTime>,
    online: bool,

    hw_os: Option<String>,
    hw_os_version: Option<String>,
    hw_kernel: Option<String>,
    hw_arch: Option<String>,
    hw_cpu_model: Option<String>,
    hw_cpu_cores: Option<i32>,
    hw_mem_bytes: Option<i64>,
    hw_swap_bytes: Option<i64>,
    hw_disk_bytes: Option<i64>,
    hw_virtualization: Option<String>,
    agent_version: Option<String>,
    location: Option<String>,
    flag_emoji: Option<String>,

    ts: Option<OffsetDateTime>,
    cpu_pct: Option<f64>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    swap_used: Option<i64>,
    swap_total: Option<i64>,
    load_1: Option<f64>,
    disk_used: Option<i64>,
    disk_total: Option<i64>,
    net_in_bps: Option<i64>,
    net_out_bps: Option<i64>,
    process_count: Option<i32>,
}

impl ServerRowDb {
    fn into_response(self, guest: bool) -> ServerRow {
        let hardware = if guest {
            None
        } else {
            Some(Hardware {
                os: self.hw_os,
                os_version: self.hw_os_version,
                kernel: self.hw_kernel,
                arch: self.hw_arch,
                cpu_model: self.hw_cpu_model,
                cpu_cores: self.hw_cpu_cores,
                mem_bytes: self.hw_mem_bytes,
                swap_bytes: self.hw_swap_bytes,
                disk_bytes: self.hw_disk_bytes,
                virtualization: self.hw_virtualization,
            })
        };

        let latest = self.ts.map(|ts| LatestSample {
            ts,
            cpu_pct: self.cpu_pct.unwrap_or(0.0),
            mem_used: self.mem_used.unwrap_or(0),
            mem_total: self.mem_total.unwrap_or(0),
            swap_used: self.swap_used.unwrap_or(0),
            swap_total: self.swap_total.unwrap_or(0),
            load_1: self.load_1.unwrap_or(0.0),
            disk_used: self.disk_used.unwrap_or(0),
            disk_total: self.disk_total.unwrap_or(0),
            net_in_bps: self.net_in_bps.unwrap_or(0),
            net_out_bps: self.net_out_bps.unwrap_or(0),
            process_count: self.process_count.unwrap_or(0),
        });

        ServerRow {
            id: self.id,
            agent_id: self.agent_id.to_string(),
            display_name: self.display_name,
            group_id: self.group_id,
            group_name: self.group_name,
            tags: self.tags,
            hidden_from_guest: self.hidden_from_guest,
            last_seen_at: self.last_seen_at,
            online: self.online,
            hardware,
            latest,
            agent_version: self.agent_version,
            location: self.location,
            flag_emoji: self.flag_emoji,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/servers  (dev-only until M3)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateServer {
    pub display_name: String,
}

#[derive(Serialize)]
pub struct CreatedServer {
    pub id: i64,
    pub agent_id: String,
    pub display_name: String,
    pub join_token: String,
    pub install_command: String,
}

pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateServer>,
) -> impl IntoResponse {
    let display = body.display_name.trim().to_owned();
    if display.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                code: "display_name_required",
                message: "display_name must not be empty".into(),
            }),
        )
            .into_response();
    }

    let endpoint = match settings::agent_endpoint(&state.pool).await {
        Ok(Some(ep)) => ep,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    code: "agent_endpoint_not_configured",
                    message: "agent_endpoint must be set in settings before adding servers".into(),
                }),
            )
                .into_response();
        }
        Err(err) => {
            tracing::error!(%err, "reading agent_endpoint");
            return internal_error();
        }
    };

    let join_token = monitor_common::token::generate();
    let row = sqlx::query_as::<_, (i64, uuid::Uuid)>(
        r#"INSERT INTO servers (display_name, join_token)
           VALUES ($1, $2)
           RETURNING id, agent_id"#,
    )
    .bind(&display)
    .bind(&join_token)
    .fetch_one(&state.pool)
    .await;

    let (id, agent_id) = match row {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(%err, "inserting server row");
            return internal_error();
        }
    };

    let install_command = format!(
        "curl -fsSL {endpoint}/install-agent.sh | sudo sh -s -- \
            --endpoint={endpoint} --token={join_token}"
    );

    (
        StatusCode::CREATED,
        Json(CreatedServer {
            id,
            agent_id: agent_id.to_string(),
            display_name: display,
            join_token,
            install_command,
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Error plumbing
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

pub struct ApiError(anyhow::Error);

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(error = %self.0, "api error");
        internal_error()
    }
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
