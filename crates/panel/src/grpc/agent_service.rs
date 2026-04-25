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

        // Version correlation: if this agent is the target of any active
        // assignment whose `version` matches what just registered, that's
        // unambiguous proof the upgrade succeeded.
        if let Err(err) =
            mark_assignments_for_version(self.pool(), agent_id, &req.agent_version).await
        {
            tracing::warn!(%err, "post-register assignment correlation failed");
        }

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

        // Optional `x-agent-version` metadata: the agent reports the binary
        // version it's currently running. Used by the supervisor flow so a
        // post-swap stream open refreshes `servers.agent_version` without
        // waiting for a Register (which only happens on first install).
        let agent_version = request
            .metadata()
            .get(monitor_proto::AGENT_VERSION_METADATA)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if let Some(ref v) = agent_version {
            if !v.is_empty() {
                if let Err(err) = sqlx::query(
                    "UPDATE servers SET agent_version = $1 WHERE id = $2 AND agent_version IS DISTINCT FROM $1",
                )
                .bind(v)
                .bind(server_row_id)
                .execute(self.pool())
                .await
                {
                    tracing::warn!(%err, %agent_id, "agent_version refresh failed");
                }
                if let Err(err) = mark_assignments_for_version(self.pool(), agent_id, v).await {
                    tracing::warn!(%err, "post-stream assignment correlation failed");
                }
            }
        }

        tracing::info!(%agent_id, version = agent_version.as_deref().unwrap_or("?"), "agent connected (stream)");

        let (session, rx) = self.state.hub.register(agent_id, server_row_id);
        let inbound = request.into_inner();

        // Wake the probe scheduler so it pushes this agent's initial probe
        // assignment as soon as the downstream channel is live.
        self.state.assignment_bus.publish();

        // Catch-up dispatch: any rollout assignment that's still `pending`
        // for this agent gets shipped now that the channel is live.
        let pool = self.state.pool.clone();
        let hub = self.state.hub.clone();
        tokio::spawn(async move {
            if let Err(err) =
                crate::updates::dispatch::dispatch_pending_for_agent(&pool, &hub, agent_id).await
            {
                tracing::warn!(%err, %agent_id, "post-connect update dispatch failed");
            }
        });

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
        AgentPayload::ProbeResult(r) => {
            ingest_probe_results(state, session, std::slice::from_ref(&r)).await;
        }
        AgentPayload::ProbeBatch(b) => {
            ingest_probe_results(state, session, &b.results).await;
        }
        AgentPayload::TerminalOutput(out) => {
            state.terminal_hub.deliver_output(out);
        }
        AgentPayload::TerminalClosed(closed) => {
            state.terminal_hub.deliver_closed(&state.pool, closed).await;
        }
        AgentPayload::UpdateStatus(status) => {
            ingest_update_status(state, session, status).await;
        }
        AgentPayload::RecordingChunk(chunk) => {
            state.recording_hub.deliver_chunk(chunk);
        }
    }
}

async fn ingest_update_status(
    state: &AppState,
    session: &crate::grpc::AgentSession,
    status: monitor_proto::v1::UpdateStatus,
) {
    use monitor_proto::v1::UpdateState;
    let Ok(rollout_id) = status.rollout_id.parse::<i64>() else {
        tracing::debug!(rollout = %status.rollout_id, "ignoring UpdateStatus with non-numeric rollout_id");
        return;
    };
    // Map proto state → assignment state. Anything terminal (Confirmed,
    // Failed, RolledBack) flips the row; intermediate states bump the
    // textual `last_status_message` so an admin watching the UI sees
    // progress without us churning the state column.
    let parsed = UpdateState::try_from(status.state).unwrap_or(UpdateState::Unspecified);
    let new_state = match parsed {
        UpdateState::Confirmed => Some("succeeded"),
        UpdateState::Failed | UpdateState::RolledBack => Some("failed"),
        UpdateState::Downloading | UpdateState::Probing | UpdateState::Switching => Some("sent"),
        _ => None,
    };
    let detail = if status.detail.is_empty() {
        None
    } else {
        Some(status.detail.clone())
    };

    if let Some(target_state) = new_state {
        if let Err(err) = sqlx::query(
            r#"UPDATE update_assignments
                   SET state = $1,
                       last_status_message = COALESCE($2, last_status_message),
                       updated_at = NOW()
                   WHERE rollout_id = $3 AND agent_id = $4"#,
        )
        .bind(target_state)
        .bind(detail.as_deref())
        .bind(rollout_id)
        .bind(session.agent_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%err, "update assignment write failed");
        }
    } else if let Some(d) = detail.as_deref() {
        if let Err(err) = sqlx::query(
            r#"UPDATE update_assignments
                   SET last_status_message = $1, updated_at = NOW()
                   WHERE rollout_id = $2 AND agent_id = $3"#,
        )
        .bind(d)
        .bind(rollout_id)
        .bind(session.agent_id)
        .execute(&state.pool)
        .await
        {
            tracing::warn!(%err, "update assignment status write failed");
        }
    }

    // Best-effort: when every assignment for the rollout is terminal,
    // mark the rollout itself as completed. Aborted/paused take precedence
    // and are handled in the API layer.
    if let Err(err) = maybe_mark_completed(state, rollout_id).await {
        tracing::debug!(%err, "rollout completion check failed");
    }
}

async fn maybe_mark_completed(state: &AppState, rollout_id: i64) -> Result<(), sqlx::Error> {
    let pending: Option<(i64,)> = sqlx::query_as(
        r#"SELECT COUNT(*) FROM update_assignments
              WHERE rollout_id = $1 AND state IN ('pending', 'sent')"#,
    )
    .bind(rollout_id)
    .fetch_optional(&state.pool)
    .await?;
    if pending.map(|(c,)| c).unwrap_or(0) == 0 {
        sqlx::query(
            r#"UPDATE update_rollouts
                   SET state = 'completed', finished_at = COALESCE(finished_at, NOW())
                   WHERE id = $1 AND state IN ('pending', 'active')"#,
        )
        .bind(rollout_id)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}

async fn ingest_probe_results(
    state: &AppState,
    session: &crate::grpc::AgentSession,
    results: &[monitor_proto::v1::ProbeResult],
) {
    if results.is_empty() {
        return;
    }
    if let Err(err) = crate::probes::ingest_batch(&state.pool, session.agent_id, results).await {
        tracing::error!(%err, "probe ingest failed");
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

/// On Register, look up any open rollout assignments whose target version
/// matches the agent_version this agent reported. A match means the swap
/// took — we mark the assignment succeeded right away rather than waiting
/// for an `UpdateState::Confirmed` frame, which an over-eager rollback
/// might never send.
async fn mark_assignments_for_version(
    pool: &PgPool,
    agent_id: Uuid,
    agent_version: &str,
) -> sqlx::Result<()> {
    if agent_version.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE update_assignments AS a
              SET state = 'succeeded',
                  last_status_message = 'agent registered with target version',
                  updated_at = NOW()
              FROM update_rollouts r
              WHERE a.rollout_id = r.id
                AND a.agent_id = $1
                AND r.version = $2
                AND a.state IN ('pending', 'sent')
                AND r.state IN ('pending', 'active', 'paused')"#,
    )
    .bind(agent_id)
    .bind(agent_version)
    .execute(pool)
    .await?;
    Ok(())
}
