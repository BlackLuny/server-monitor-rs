//! `AgentService` implementation — the single RPC interface between panel and agents.

// `tonic::Status` is intentionally large (it carries headers/trailers/metadata).
// Returning it in `Result<T, Status>` is the idiomatic tonic pattern, so we
// suppress the `result_large_err` lint module-wide.
#![allow(clippy::result_large_err)]

use std::pin::Pin;

use futures::Stream;
use monitor_proto::v1::{
    agent_service_server::AgentService, agent_to_panel::Payload as AgentPayload, AgentToPanel,
    PanelToAgent, RegisterRequest, RegisterResponse,
};
use sqlx::PgPool;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::{live::LiveUpdate, state::AppState};

pub type StreamOut = Pin<Box<dyn Stream<Item = Result<PanelToAgent, Status>> + Send + 'static>>;

/// Concrete tonic service impl. Cloneable (holds an `AppState` which wraps Arcs).
#[derive(Clone)]
pub struct AgentServiceImpl {
    state: AppState,
}

impl AgentServiceImpl {
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn pool(&self) -> &PgPool {
        &self.state.pool
    }
}

#[tonic::async_trait]
impl AgentService for AgentServiceImpl {
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let req = request.into_inner();

        if req.join_token.trim().is_empty() {
            return Err(Status::unauthenticated("join_token is required"));
        }

        // The agent_id is assigned when the admin adds the server, so Register
        // only looks it up here — never generates a new one. That keeps the
        // identifier stable from the admin UI's perspective.
        let row = sqlx::query_as::<_, (i64, Uuid)>(
            r#"SELECT id, agent_id
               FROM servers
               WHERE join_token = $1 AND server_token IS NULL"#,
        )
        .bind(&req.join_token)
        .fetch_optional(self.pool())
        .await
        .map_err(internal_db_error)?;

        let Some((server_row_id, agent_id)) = row else {
            return Err(Status::unauthenticated(
                "invalid or already-used join_token",
            ));
        };

        let server_token = monitor_common::token::generate();
        let hw = req.hardware.unwrap_or_default();

        sqlx::query(
            r#"UPDATE servers SET
                server_token      = $1,
                join_token        = NULL,
                hw_cpu_model      = NULLIF($2, ''),
                hw_cpu_cores      = NULLIF($3, 0),
                hw_mem_bytes      = NULLIF($4::bigint, 0::bigint),
                hw_swap_bytes     = NULLIF($5::bigint, 0::bigint),
                hw_disk_bytes     = NULLIF($6::bigint, 0::bigint),
                hw_os             = NULLIF($7, ''),
                hw_os_version     = NULLIF($8, ''),
                hw_kernel         = NULLIF($9, ''),
                hw_arch           = NULLIF($10, ''),
                hw_virtualization = NULLIF($11, ''),
                hw_boot_id        = NULLIF($12, ''),
                agent_version     = NULLIF($13, '')
               WHERE id = $14"#,
        )
        .bind(&server_token)
        .bind(&hw.cpu_model)
        .bind(i32::try_from(hw.cpu_cores).unwrap_or(0))
        .bind(i64::try_from(hw.mem_bytes).unwrap_or(0))
        .bind(i64::try_from(hw.swap_bytes).unwrap_or(0))
        .bind(i64::try_from(hw.disk_bytes).unwrap_or(0))
        .bind(if hw.os.is_empty() {
            req.os.clone()
        } else {
            hw.os.clone()
        })
        .bind(&hw.os_version)
        .bind(&hw.kernel)
        .bind(if hw.arch.is_empty() {
            req.arch.clone()
        } else {
            hw.arch.clone()
        })
        .bind(&hw.virtualization)
        .bind(&hw.boot_id)
        .bind(&req.agent_version)
        .bind(server_row_id)
        .execute(self.pool())
        .await
        .map_err(internal_db_error)?;

        tracing::info!(
            agent_id = %agent_id,
            hostname = %req.hostname,
            os = %req.os,
            arch = %req.arch,
            version = %req.agent_version,
            "agent registered",
        );

        Ok(Response::new(RegisterResponse {
            agent_id: agent_id.to_string(),
            server_token,
        }))
    }

    type StreamStream = StreamOut;

    async fn stream(
        &self,
        request: Request<Streaming<AgentToPanel>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        // Authenticate: server_token must match exactly one server row.
        let token = extract_server_token(&request)?;
        let (agent_id, server_row_id) = lookup_agent_by_token(self.pool(), &token).await?;

        tracing::info!(%agent_id, "agent connected (stream)");

        let (session, rx) = self.state.hub.register(agent_id, server_row_id);
        let inbound = request.into_inner();

        // Spawn the inbound loop — it owns the session (so it controls when to
        // drop from the hub). `rx` is returned to tonic to drive downstream.
        tokio::spawn(run_inbound_loop(
            self.state.clone(),
            session.clone(),
            inbound,
        ));

        let out_stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(out_stream)))
    }
}

fn extract_server_token<T>(req: &Request<T>) -> Result<String, Status> {
    let raw = req
        .metadata()
        .get(monitor_proto::SERVER_TOKEN_METADATA)
        .ok_or_else(|| Status::unauthenticated("missing x-server-token"))?;
    raw.to_str()
        .map(|s| s.to_owned())
        .map_err(|_| Status::unauthenticated("invalid x-server-token encoding"))
}

async fn lookup_agent_by_token(pool: &PgPool, token: &str) -> Result<(Uuid, i64), Status> {
    let row = sqlx::query_as::<_, (Uuid, i64)>(
        r#"SELECT agent_id, id
           FROM servers
           WHERE server_token = $1 AND agent_id IS NOT NULL"#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(internal_db_error)?;
    row.ok_or_else(|| Status::unauthenticated("invalid server_token"))
}

/// Consume the agent's upstream until the client disconnects or errors out.
async fn run_inbound_loop(
    state: AppState,
    session: crate::grpc::AgentSession,
    mut inbound: Streaming<AgentToPanel>,
) {
    let agent_id = session.agent_id;

    while let Some(next) = inbound.next().await {
        match next {
            Ok(msg) => {
                if let Some(payload) = msg.payload {
                    handle_payload(&state, &session, payload).await;
                }
            }
            Err(status) => {
                tracing::info!(%agent_id, %status, "agent stream errored");
                break;
            }
        }
    }

    tracing::info!(%agent_id, "agent disconnected");
    state.hub.remove_if_matches(agent_id, &session.connected_at);
}

async fn handle_payload(
    state: &AppState,
    session: &crate::grpc::AgentSession,
    payload: AgentPayload,
) {
    let pool = &state.pool;
    match payload {
        AgentPayload::Heartbeat(hb) => {
            if let Err(err) =
                sqlx::query(r#"UPDATE servers SET last_seen_at = NOW() WHERE id = $1"#)
                    .bind(session.server_row_id)
                    .execute(pool)
                    .await
            {
                tracing::warn!(agent_id = %session.agent_id, %err, "heartbeat update failed");
            } else {
                tracing::debug!(
                    agent_id = %session.agent_id,
                    uptime_s = hb.uptime_s,
                    "heartbeat",
                );
            }
        }
        AgentPayload::Metrics(snapshot) => {
            ingest_and_broadcast(state, session, std::slice::from_ref(&snapshot)).await;
        }
        AgentPayload::MetricsBatch(batch) => {
            ingest_and_broadcast(state, session, &batch.snapshots).await;
        }
        // M4 will persist probe results; M5 the terminal IO; M7 the update status.
        AgentPayload::ProbeResult(_)
        | AgentPayload::TerminalOutput(_)
        | AgentPayload::TerminalClosed(_)
        | AgentPayload::UpdateStatus(_) => {}
    }
}

async fn ingest_and_broadcast(
    state: &AppState,
    session: &crate::grpc::AgentSession,
    snapshots: &[monitor_proto::v1::MetricSnapshot],
) {
    if snapshots.is_empty() {
        return;
    }
    let pool = &state.pool;
    let n = snapshots.len();
    match crate::metrics::ingest_batch(pool, session.server_row_id, snapshots).await {
        Ok(inserted) => {
            tracing::debug!(
                agent_id = %session.agent_id,
                sent = n,
                inserted,
                "metrics ingested",
            );
        }
        Err(err) => {
            tracing::warn!(agent_id = %session.agent_id, %err, sent = n, "metrics insert failed");
            return;
        }
    }

    // Publish only the most recent snapshot — subscribers plot the latest
    // value; no value in flooding the bus with all 5 samples every 5 seconds.
    if let Some(last) = snapshots.last() {
        let hidden: Option<(bool,)> =
            sqlx::query_as("SELECT hidden_from_guest FROM servers WHERE id = $1")
                .bind(session.server_row_id)
                .fetch_optional(pool)
                .await
                .unwrap_or(None);

        let hidden_from_guest = hidden.map(|(h,)| h).unwrap_or(false);
        let ts = match time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(last.ts_ms) * 1_000_000,
        ) {
            Ok(t) => t,
            Err(_) => time::OffsetDateTime::now_utc(),
        };
        state.live.publish(LiveUpdate {
            server_id: session.server_row_id,
            hidden_from_guest,
            ts,
            cpu_pct: last.cpu_pct,
            mem_used: i64::try_from(last.mem_used).unwrap_or(i64::MAX),
            mem_total: i64::try_from(last.mem_total).unwrap_or(i64::MAX),
            net_in_bps: i64::try_from(last.net_in_bps).unwrap_or(i64::MAX),
            net_out_bps: i64::try_from(last.net_out_bps).unwrap_or(i64::MAX),
            load_1: last.load_1,
        });
    }
}

fn internal_db_error(err: sqlx::Error) -> Status {
    tracing::error!(%err, "database error in AgentService");
    Status::internal("database error")
}
