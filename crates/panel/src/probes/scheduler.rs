//! Probe assignment scheduler.
//!
//! Responsibility: tell every connected agent which probes they should be
//! running, *right now*, given:
//!   - the current `probes` rows (and each one's `default_enabled` + global
//!     `enabled` flag)
//!   - the per-agent overrides in `probe_agent_overrides`
//!
//! It runs as a single tokio task that listens on an [`AssignmentBus`]
//! broadcast channel. Anyone who mutates probe state — API handlers, the
//! gRPC agent_service when a new agent connects — calls `bus.publish()` to
//! kick this task. The task then snapshots all connected agents, recomputes
//! the effective set for each, diffs it against what was last pushed, and
//! emits either:
//!   - `ProbeAssignmentSync` when the agent has no last-pushed state (first
//!     contact, or panel restart), or
//!   - `ProbeAssignmentDelta` with the actual added / updated / removed.
//!
//! Crash semantics: if the panel restarts, the in-memory `last_pushed` map
//! is empty, so every agent's next nudge produces a full Sync — that's the
//! whole point of having both message variants.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use monitor_proto::v1::{
    panel_to_agent::Payload as DownPayload, PanelToAgent, Probe, ProbeAssignmentDelta,
    ProbeAssignmentSync, ProbeType,
};
use sqlx::PgPool;
use tokio::sync::{broadcast, watch, Mutex};
use uuid::Uuid;

use crate::grpc::SessionHub;

/// Broadcast bus used to wake the scheduler when probe assignment may have
/// changed. Cheap to clone and pass through `AppState`.
#[derive(Clone)]
pub struct AssignmentBus {
    tx: broadcast::Sender<()>,
}

impl AssignmentBus {
    #[must_use]
    pub fn new() -> Self {
        // Capacity 8 is plenty: the scheduler coalesces multiple wakes via
        // tokio::select, and an overrun just means we recompute one extra
        // time which is harmless.
        let (tx, _rx) = broadcast::channel(8);
        Self { tx }
    }

    /// Wake the scheduler. Failures (no receiver) are silent — they only
    /// happen if the scheduler hasn't started yet, which is fine because
    /// once it does it will recompute from scratch on its first tick.
    pub fn publish(&self) {
        let _ = self.tx.send(());
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }
}

impl Default for AssignmentBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Long-running task that keeps every connected agent's probe set in sync.
pub struct Scheduler {
    pool: PgPool,
    hub: SessionHub,
    bus: AssignmentBus,
    /// What we last pushed to each agent. Probes keyed by their numeric id.
    last_pushed: Arc<Mutex<HashMap<Uuid, HashMap<i64, Probe>>>>,
}

impl Scheduler {
    pub fn new(pool: PgPool, hub: SessionHub, bus: AssignmentBus) -> Self {
        Self {
            pool,
            hub,
            bus,
            last_pushed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn the loop. Exits when `shutdown` flips.
    pub fn spawn(self, mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut rx = self.bus.subscribe();
            // Run once at startup so a panel restart immediately reconciles
            // every reconnecting agent.
            if let Err(err) = self.tick().await {
                tracing::warn!(%err, "probe scheduler initial tick failed");
            }
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                                if let Err(err) = self.tick().await {
                                    tracing::warn!(%err, "probe scheduler tick failed");
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            tracing::info!("probe scheduler stopping");
                            return;
                        }
                    }
                }
            }
        })
    }

    /// Recompute and push deltas for every currently-connected agent. Public
    /// only because tests are easier to write that way.
    pub async fn tick(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let agents = self.hub.agent_ids();
        if agents.is_empty() {
            return Ok(());
        }
        let probes = self.fetch_enabled_probes().await?;
        for agent_id in agents {
            self.reconcile(agent_id, &probes).await?;
        }
        Ok(())
    }

    /// All globally-enabled probes, indexed by id.
    async fn fetch_enabled_probes(
        &self,
    ) -> Result<HashMap<i64, ProbeRow>, Box<dyn std::error::Error + Send + Sync>> {
        let rows: Vec<ProbeRow> = sqlx::query_as(
            r#"SELECT id, name, kind, target, port, interval_s, timeout_ms,
                       http_method, http_expect_code, http_expect_body,
                       default_enabled
                  FROM probes
                 WHERE enabled = TRUE"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.id, r)).collect())
    }

    /// Compute the effective probe set for one agent and emit a sync/delta.
    async fn reconcile(
        &self,
        agent_id: Uuid,
        all_probes: &HashMap<i64, ProbeRow>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let session = match self.hub.get(&agent_id) {
            Some(s) => s,
            None => return Ok(()),
        };

        // Per-agent override map.
        let overrides: HashMap<i64, bool> = sqlx::query_as::<_, (i64, bool)>(
            "SELECT probe_id, enabled FROM probe_agent_overrides WHERE agent_id = $1",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();

        let effective: HashMap<i64, Probe> = all_probes
            .iter()
            .filter_map(|(id, p)| {
                let on = overrides.get(id).copied().unwrap_or(p.default_enabled);
                if on {
                    Some((*id, p.to_proto()))
                } else {
                    None
                }
            })
            .collect();

        let mut last_pushed = self.last_pushed.lock().await;
        let prev = last_pushed.get(&agent_id).cloned().unwrap_or_default();

        // First contact (or post-restart) → full Sync. Removes-only deltas
        // are also a Sync because we can't distinguish "agent never heard
        // about this probe" from "we already removed it".
        let send_sync = prev.is_empty();
        if send_sync {
            let probes: Vec<Probe> = effective.values().cloned().collect();
            let msg = PanelToAgent {
                seq: 0,
                payload: Some(DownPayload::ProbeAssignmentSync(ProbeAssignmentSync {
                    probes,
                })),
            };
            if !session.try_send(msg) {
                tracing::warn!(%agent_id, "agent stream full — dropping initial probe sync");
                return Ok(());
            }
            last_pushed.insert(agent_id, effective);
            return Ok(());
        }

        // Delta computation.
        let prev_ids: HashSet<i64> = prev.keys().copied().collect();
        let curr_ids: HashSet<i64> = effective.keys().copied().collect();
        let mut added = Vec::new();
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        for id in &curr_ids {
            match prev.get(id) {
                None => added.push(effective[id].clone()),
                Some(p) if probe_differs(p, &effective[id]) => {
                    updated.push(effective[id].clone());
                }
                _ => {}
            }
        }
        for id in prev_ids.difference(&curr_ids) {
            removed.push(id.to_string());
        }
        if added.is_empty() && updated.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let msg = PanelToAgent {
            seq: 0,
            payload: Some(DownPayload::ProbeAssignmentDelta(ProbeAssignmentDelta {
                added,
                updated,
                removed_probe_ids: removed,
            })),
        };
        if !session.try_send(msg) {
            tracing::warn!(%agent_id, "agent stream full — dropping probe delta");
            return Ok(());
        }
        last_pushed.insert(agent_id, effective);
        Ok(())
    }
}

#[derive(Clone, sqlx::FromRow)]
struct ProbeRow {
    id: i64,
    name: String,
    kind: String,
    target: String,
    port: Option<i32>,
    interval_s: i32,
    timeout_ms: i32,
    http_method: Option<String>,
    http_expect_code: Option<i32>,
    http_expect_body: Option<String>,
    default_enabled: bool,
}

impl ProbeRow {
    fn to_proto(&self) -> Probe {
        Probe {
            id: self.id.to_string(),
            name: self.name.clone(),
            r#type: kind_to_proto(&self.kind) as i32,
            target: self.target.clone(),
            port: self
                .port
                .map(|p| u32::try_from(p).unwrap_or(0))
                .unwrap_or(0),
            interval_s: u32::try_from(self.interval_s).unwrap_or(60),
            timeout_ms: u32::try_from(self.timeout_ms).unwrap_or(3000),
            http_method: self.http_method.clone().unwrap_or_default(),
            http_expect_code: self
                .http_expect_code
                .map(|c| u32::try_from(c).unwrap_or(0))
                .unwrap_or(0),
            http_expect_body: self.http_expect_body.clone().unwrap_or_default(),
        }
    }
}

fn kind_to_proto(kind: &str) -> ProbeType {
    match kind {
        "icmp" => ProbeType::Icmp,
        "tcp" => ProbeType::Tcp,
        "http" => ProbeType::Http,
        _ => ProbeType::Unspecified,
    }
}

fn probe_differs(a: &Probe, b: &Probe) -> bool {
    a.name != b.name
        || a.r#type != b.r#type
        || a.target != b.target
        || a.port != b.port
        || a.interval_s != b.interval_s
        || a.timeout_ms != b.timeout_ms
        || a.http_method != b.http_method
        || a.http_expect_code != b.http_expect_code
        || a.http_expect_body != b.http_expect_body
}
