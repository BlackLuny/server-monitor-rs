//! Push `UpdateAgent` / `UpdateAbort` frames to currently-connected agents.
//!
//! Rollout assignments live in Postgres; the panel doesn't talk to the
//! agent until either an admin starts a rollout or an agent reconnects.
//! This module is the one place that turns DB rows into PanelToAgent
//! messages. Aborts mirror the same path: every connected agent whose
//! assignment is still in flight gets one `UpdateAbort` so it can tell its
//! supervisor to cancel the in-progress download.

use monitor_proto::v1::{
    panel_to_agent::Payload as DownPayload, PanelToAgent, UpdateAbort, UpdateAgent,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{grpc::SessionHub, settings};

/// Default supervisor grace window in seconds. Mirrors the supervisor's own
/// `DEFAULT_GRACE`. Sent on every UpdateAgent so the supervisor doesn't
/// have to assume.
const DEFAULT_GRACE_S: u32 = 60;

/// Resolve the per-deployment attestation policy. When the operator sets
/// `attestation_required = true` the supervisor receives the repo slug in
/// `UpdateAgent.attestation_url`, which it interprets as "verify with
/// `gh attestation verify`." When the setting is unset / false we send an
/// empty string (the default) and the supervisor skips verification.
async fn attestation_repo(pool: &PgPool) -> String {
    let required = settings::get::<bool>(pool, "attestation_required")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if !required {
        return String::new();
    }
    settings::get::<String>(pool, "update_repo")
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Push `UpdateAgent` for every still-pending assignment on `rollout_id`,
/// for whichever agents happen to be online right now. Marks each row
/// `state = 'sent'` after a successful enqueue. Returns the count of
/// rows that flipped.
pub async fn dispatch_pending_for_rollout(
    pool: &PgPool,
    hub: &SessionHub,
    rollout_id: i64,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        r#"SELECT a.agent_id, r.version, a.artefact_url, a.artefact_sha256
              FROM update_assignments a
              JOIN update_rollouts r ON r.id = a.rollout_id
              WHERE a.rollout_id = $1
                AND a.state = 'pending'
                AND r.state IN ('pending', 'active')"#,
    )
    .bind(rollout_id)
    .fetch_all(pool)
    .await?;

    let attestation = attestation_repo(pool).await;
    let mut sent = 0usize;
    for (agent_id, version, asset_url, sha256) in rows {
        let Some(session) = hub.get(&agent_id) else {
            continue;
        };
        let frame = PanelToAgent {
            seq: 0,
            payload: Some(DownPayload::Update(UpdateAgent {
                rollout_id: rollout_id.to_string(),
                version,
                asset_url,
                sha256,
                attestation_url: attestation.clone(),
                grace_s: DEFAULT_GRACE_S,
            })),
        };
        if !session.try_send(frame) {
            tracing::warn!(%agent_id, rollout_id, "agent channel full — leaving pending");
            continue;
        }
        sqlx::query(
            r#"UPDATE update_assignments
                   SET state = 'sent', updated_at = NOW()
                   WHERE rollout_id = $1 AND agent_id = $2 AND state = 'pending'"#,
        )
        .bind(rollout_id)
        .bind(agent_id)
        .execute(pool)
        .await?;
        sent += 1;
    }
    Ok(sent)
}

/// On agent reconnect, push `UpdateAgent` for any pending assignment
/// targeting this agent. Mirrors `dispatch_pending_for_rollout`'s row
/// filter; the difference is the index column (agent_id, not rollout_id).
pub async fn dispatch_pending_for_agent(
    pool: &PgPool,
    hub: &SessionHub,
    agent_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        r#"SELECT a.rollout_id, r.version, a.artefact_url, a.artefact_sha256
              FROM update_assignments a
              JOIN update_rollouts r ON r.id = a.rollout_id
              WHERE a.agent_id = $1
                AND a.state = 'pending'
                AND r.state IN ('pending', 'active')"#,
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await?;

    let Some(session) = hub.get(&agent_id) else {
        return Ok(0);
    };

    let attestation = attestation_repo(pool).await;
    let mut sent = 0usize;
    for (rollout_id, version, asset_url, sha256) in rows {
        let frame = PanelToAgent {
            seq: 0,
            payload: Some(DownPayload::Update(UpdateAgent {
                rollout_id: rollout_id.to_string(),
                version,
                asset_url,
                sha256,
                attestation_url: attestation.clone(),
                grace_s: DEFAULT_GRACE_S,
            })),
        };
        if !session.try_send(frame) {
            tracing::warn!(%agent_id, rollout_id, "agent channel full on reconnect");
            continue;
        }
        sqlx::query(
            r#"UPDATE update_assignments
                   SET state = 'sent', updated_at = NOW()
                   WHERE rollout_id = $1 AND agent_id = $2 AND state = 'pending'"#,
        )
        .bind(rollout_id)
        .bind(agent_id)
        .execute(pool)
        .await?;
        sent += 1;
    }
    Ok(sent)
}

/// Push `UpdateAbort` for every still-active assignment on `rollout_id`,
/// then mark the rows `failed` with the abort reason. Pending rows that
/// belong to currently-offline agents are also marked failed so the
/// rollout doesn't drag on indefinitely after the abort.
pub async fn dispatch_aborts_for_rollout(
    pool: &PgPool,
    hub: &SessionHub,
    rollout_id: i64,
    reason: &str,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        r#"SELECT agent_id, state
              FROM update_assignments
              WHERE rollout_id = $1
                AND state IN ('pending', 'sent')"#,
    )
    .bind(rollout_id)
    .fetch_all(pool)
    .await?;

    let mut notified = 0usize;
    for (agent_id, _state) in &rows {
        if let Some(session) = hub.get(agent_id) {
            let frame = PanelToAgent {
                seq: 0,
                payload: Some(DownPayload::UpdateAbort(UpdateAbort {
                    rollout_id: rollout_id.to_string(),
                    reason: reason.to_owned(),
                })),
            };
            if session.try_send(frame) {
                notified += 1;
            }
        }
    }

    // Whether or not we got a frame out the door, mark all in-flight rows
    // failed — the rollout itself is aborted, so nothing more should ship.
    sqlx::query(
        r#"UPDATE update_assignments
               SET state = 'failed',
                   last_status_message = $2,
                   updated_at = NOW()
               WHERE rollout_id = $1 AND state IN ('pending', 'sent')"#,
    )
    .bind(rollout_id)
    .bind(reason)
    .execute(pool)
    .await?;

    Ok(notified)
}
