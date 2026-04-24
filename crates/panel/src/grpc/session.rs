//! Connected agent session registry.
//!
//! For every Agent that successfully opens the [`Stream`] RPC we create an
//! [`AgentSession`] holding a `mpsc::Sender<PanelToAgent>` that other panel
//! modules (API handlers, probe scheduler, terminal, update rollouts) can use
//! to push a `PanelToAgent` message down the live wire.
//!
//! [`Stream`]: monitor_proto::v1::agent_service_server::AgentService::stream

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use monitor_proto::v1::PanelToAgent;
use tokio::sync::mpsc;
use tonic::Status;
use uuid::Uuid;

/// Buffered capacity of the per-agent downstream channel.
///
/// Kept small because panel-to-agent messages are infrequent (probe configs,
/// terminal IO, update commands); a large buffer would just hide stalls.
const DOWNSTREAM_BUFFER: usize = 64;

/// Live handle to a single Agent connection. Cloneable because it is stored
/// both in the [`SessionHub`] and as local state inside the Stream task.
#[derive(Clone)]
pub struct AgentSession {
    pub agent_id: Uuid,
    pub server_row_id: i64,
    pub tx: mpsc::Sender<Result<PanelToAgent, Status>>,
    pub connected_at: Arc<Instant>,
}

impl AgentSession {
    /// Non-blocking attempt to enqueue a message for the agent.
    ///
    /// Returns `false` if the channel is full (caller may log or drop) or closed.
    pub fn try_send(&self, msg: PanelToAgent) -> bool {
        self.tx.try_send(Ok(msg)).is_ok()
    }
}

/// Global registry of currently connected agents.
#[derive(Default, Clone)]
pub struct SessionHub {
    sessions: Arc<DashMap<Uuid, AgentSession>>,
}

impl SessionHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new session, returning the downstream receiver half and the
    /// registered session. Replaces any previous session for the same agent.
    pub fn register(
        &self,
        agent_id: Uuid,
        server_row_id: i64,
    ) -> (AgentSession, mpsc::Receiver<Result<PanelToAgent, Status>>) {
        let (tx, rx) = mpsc::channel(DOWNSTREAM_BUFFER);
        let session = AgentSession {
            agent_id,
            server_row_id,
            tx,
            connected_at: Arc::new(Instant::now()),
        };
        if self.sessions.insert(agent_id, session.clone()).is_some() {
            tracing::warn!(%agent_id, "replacing existing session — agent reconnected");
        }
        (session, rx)
    }

    /// Drop the session for the given agent if it still matches the provided
    /// `connected_at` (compared by Arc pointer equality). This avoids a
    /// reconnect race where a new session is registered, then the old Stream
    /// task completes and tries to remove the new registration.
    pub fn remove_if_matches(&self, agent_id: Uuid, connected_at: &Arc<Instant>) {
        self.sessions.remove_if(&agent_id, |_, existing| {
            Arc::ptr_eq(&existing.connected_at, connected_at)
        });
    }

    /// Number of currently-connected agents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Look up an agent session; returns a clone (cheap — `mpsc::Sender`).
    #[must_use]
    pub fn get(&self, agent_id: &Uuid) -> Option<AgentSession> {
        self.sessions.get(agent_id).map(|s| s.clone())
    }
}
