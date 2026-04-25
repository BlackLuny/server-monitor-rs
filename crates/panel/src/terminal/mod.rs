//! Web SSH session bookkeeping + agent ↔ browser frame routing.
//!
//! Two collaborators:
//!   - The WS handler at `/ws/terminal/:server_id` opens / closes sessions
//!     and runs the bidirectional bridge.
//!   - The gRPC agent_service inbound loop forwards [`TerminalOutput`] and
//!     [`TerminalClosed`] frames it receives from agents into this hub.
//!
//! The hub keeps just enough in-memory state to route frames: every active
//! session has an mpsc sender that the WS bridge owns the receiver for. DB
//! rows in `terminal_sessions` are the durable record (open / close
//! timestamps, recording metadata, audit trail).

use std::sync::Arc;

use dashmap::DashMap;
use monitor_proto::v1::{TerminalClosed, TerminalOutput};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::{audit, SessionMeta};

/// Per-user concurrent session cap. The agent enforces a per-agent cap
/// independently — these two together bound the worst case.
pub const MAX_SESSIONS_PER_USER: usize = 4;

/// Channel buffer for frames headed to a single WS bridge. Big enough to
/// soak a paste burst without dropping; small enough to apply backpressure
/// before memory bloats. Per-frame size is already capped at 32 KiB by the
/// agent.
const FRAME_CHANNEL_BUFFER: usize = 256;

/// What the WS bridge needs to relay.
#[derive(Debug)]
pub enum Frame {
    Output(Vec<u8>),
    Closed(ClosedInfo),
}

#[derive(Debug, Clone)]
pub struct ClosedInfo {
    pub exit_code: i32,
    pub error: String,
    pub recording_path: String,
    pub recording_size: i64,
    pub recording_sha256: String,
}

#[derive(Clone, Default)]
pub struct TerminalHub {
    inner: Arc<DashMap<String, Slot>>,
}

struct Slot {
    tx: mpsc::Sender<Frame>,
    user_id: Option<i64>,
    server_row_id: i64,
    agent_id: Uuid,
}

impl TerminalHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Active session count for one user, used by the WS handler to enforce
    /// [`MAX_SESSIONS_PER_USER`].
    #[must_use]
    pub fn count_for_user(&self, user_id: i64) -> usize {
        self.inner
            .iter()
            .filter(|s| s.value().user_id == Some(user_id))
            .count()
    }

    /// Register a new session: inserts the DB row and stores the routing
    /// channel. Returns the receiver the WS bridge drains.
    pub async fn open(
        &self,
        pool: &PgPool,
        session_id: Uuid,
        server_row_id: i64,
        agent_id: Uuid,
        user_id: Option<i64>,
        meta: &SessionMeta,
    ) -> sqlx::Result<mpsc::Receiver<Frame>> {
        sqlx::query(
            r#"INSERT INTO terminal_sessions
                   (id, server_id, user_id, client_ip, user_agent)
                   VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(session_id)
        .bind(server_row_id)
        .bind(user_id)
        .bind(meta.ip.as_deref())
        .bind(meta.user_agent.as_deref())
        .execute(pool)
        .await?;

        audit::record(
            pool,
            user_id,
            "ssh.opened",
            Some(&format!("server:{server_row_id}")),
            meta,
        )
        .await;

        let (tx, rx) = mpsc::channel(FRAME_CHANNEL_BUFFER);
        self.inner.insert(
            session_id.to_string(),
            Slot {
                tx,
                user_id,
                server_row_id,
                agent_id,
            },
        );
        Ok(rx)
    }

    /// Best-effort delivery of an output frame from agent → bridge. If the
    /// channel is full or the bridge has gone away we drop the chunk; that
    /// matches terminal semantics where the user simply sees a glitch and
    /// the next render fixes it.
    pub fn deliver_output(&self, frame: TerminalOutput) {
        let TerminalOutput { session_id, data } = frame;
        if let Some(slot) = self.inner.get(&session_id) {
            let _ = slot.tx.try_send(Frame::Output(data));
        }
    }

    /// Finalise a session triggered by an agent `TerminalClosed` frame.
    /// Updates the DB row with exit info + recording metadata, emits the
    /// audit event, and forwards the closed-frame to the bridge so the
    /// browser can render an exit reason.
    pub async fn deliver_closed(&self, pool: &PgPool, frame: TerminalClosed) {
        let TerminalClosed {
            session_id,
            exit_code,
            error,
            recording_path,
            recording_size,
            recording_sha256,
        } = frame;
        let id = match Uuid::parse_str(&session_id) {
            Ok(u) => u,
            Err(_) => return,
        };

        // Pull bookkeeping fields before we drop the slot so audit gets the
        // right user / server context.
        let (user_id, server_row_id) = self
            .inner
            .get(&session_id)
            .map(|s| (s.user_id, s.server_row_id))
            .unwrap_or((None, 0));

        let path_opt = if recording_path.is_empty() {
            None
        } else {
            Some(recording_path.as_str())
        };
        let sha_opt = if recording_sha256.is_empty() {
            None
        } else {
            Some(recording_sha256.as_str())
        };
        let size_opt = if recording_size > 0 {
            Some(recording_size)
        } else {
            None
        };
        let err_opt = if error.is_empty() {
            None
        } else {
            Some(error.as_str())
        };

        if let Err(err) = sqlx::query(
            r#"UPDATE terminal_sessions
                   SET closed_at = NOW(),
                       exit_code = $2,
                       error = COALESCE($3, error),
                       recording_path = COALESCE($4, recording_path),
                       recording_size = COALESCE($5, recording_size),
                       recording_sha256 = COALESCE($6, recording_sha256)
                   WHERE id = $1 AND closed_at IS NULL"#,
        )
        .bind(id)
        .bind(exit_code)
        .bind(err_opt)
        .bind(path_opt)
        .bind(size_opt)
        .bind(sha_opt)
        .execute(pool)
        .await
        {
            tracing::warn!(%err, %session_id, "terminal close UPDATE failed");
        }

        audit::record(
            pool,
            user_id,
            "ssh.closed",
            Some(&format!("server:{server_row_id}")),
            &SessionMeta::default(),
        )
        .await;

        if let Some((_, slot)) = self.inner.remove(&session_id) {
            let _ = slot
                .tx
                .send(Frame::Closed(ClosedInfo {
                    exit_code,
                    error: err_opt.unwrap_or("").to_string(),
                    recording_path: path_opt.unwrap_or("").to_string(),
                    recording_size: size_opt.unwrap_or(0),
                    recording_sha256: sha_opt.unwrap_or("").to_string(),
                }))
                .await;
        }
    }

    /// Forcibly close from the panel side — used when the WS bridge ends
    /// before we got a TerminalClosed (browser tab closed, network drop).
    /// The agent gets a TerminalClose and the DB row is sealed with the
    /// best info we have.
    pub async fn close_from_panel(&self, pool: &PgPool, session_id: &str, reason: &str) {
        let id = match Uuid::parse_str(session_id) {
            Ok(u) => u,
            Err(_) => return,
        };
        let removed = self.inner.remove(session_id);
        let user_id = removed
            .as_ref()
            .map(|(_, slot)| slot.user_id)
            .unwrap_or(None);
        let server_row_id = removed
            .as_ref()
            .map(|(_, slot)| slot.server_row_id)
            .unwrap_or(0);

        if let Err(err) = sqlx::query(
            r#"UPDATE terminal_sessions
                   SET closed_at = NOW(),
                       error = COALESCE(error, $2)
                   WHERE id = $1 AND closed_at IS NULL"#,
        )
        .bind(id)
        .bind(reason)
        .execute(pool)
        .await
        {
            tracing::warn!(%err, %session_id, "terminal panel-close UPDATE failed");
        }

        audit::record(
            pool,
            user_id,
            "ssh.closed",
            Some(&format!("server:{server_row_id}")),
            &SessionMeta::default(),
        )
        .await;
    }

    /// Whether the panel is still tracking a session id. Used by integration
    /// tests; the WS bridge already holds the receiver.
    #[must_use]
    pub fn contains(&self, session_id: &str) -> bool {
        self.inner.contains_key(session_id)
    }

    /// Locate which agent a given session is bound to. Used by the recording
    /// download endpoint so it can route the fetch RPC.
    #[must_use]
    pub fn agent_for(&self, session_id: &str) -> Option<Uuid> {
        self.inner.get(session_id).map(|s| s.agent_id)
    }
}
