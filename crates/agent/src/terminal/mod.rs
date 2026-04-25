//! Terminal session manager — owns one pty task per active session.
//!
//! Wire model:
//!   - Stream layer hands every PanelToAgent.Terminal* frame to [`Manager`].
//!   - On TerminalOpen the manager spawns a [`pty::Session`] task and stores
//!     control channels.
//!   - TerminalInput / TerminalResize / TerminalClose flow through those
//!     channels into the pty task.
//!   - The pty task emits TerminalOutput frames into the same upstream
//!     channel that metrics / probe results use, so flow control is uniform.
//!
//! Concurrency limits: at most [`MAX_SESSIONS_PER_AGENT`] active at once. The
//! 9th open is rejected with a synthetic TerminalClosed frame so the panel
//! UI shows an error instead of hanging.

use std::collections::HashMap;
use std::path::PathBuf;

use monitor_proto::v1::{
    agent_to_panel::Payload as UpPayload, AgentToPanel, TerminalClose, TerminalClosed,
    TerminalInput, TerminalOpen, TerminalResize,
};
use tokio::sync::mpsc;

mod pty;

/// Per-agent concurrent session cap. The panel enforces a per-user cap on top.
pub const MAX_SESSIONS_PER_AGENT: usize = 8;

/// Sender end of the upstream AgentToPanel channel that the stream task drains.
pub type Upstream = mpsc::Sender<AgentToPanel>;

pub struct Manager {
    sessions: HashMap<String, SessionHandle>,
    upstream: Upstream,
    recording_dir: PathBuf,
}

struct SessionHandle {
    stdin_tx: mpsc::Sender<Vec<u8>>,
    resize_tx: mpsc::Sender<(u32, u32)>,
    close_tx: mpsc::Sender<()>,
}

impl Manager {
    #[must_use]
    pub fn new(upstream: Upstream, recording_dir: PathBuf) -> Self {
        Self {
            sessions: HashMap::new(),
            upstream,
            recording_dir,
        }
    }

    pub fn open(&mut self, req: TerminalOpen) {
        let TerminalOpen {
            session_id,
            cols,
            rows,
            shell,
            record,
        } = req;
        if session_id.is_empty() {
            tracing::warn!("terminal open with empty session_id — dropping");
            return;
        }
        if self.sessions.contains_key(&session_id) {
            tracing::warn!(%session_id, "duplicate TerminalOpen ignored");
            return;
        }
        if self.sessions.len() >= MAX_SESSIONS_PER_AGENT {
            tracing::warn!(
                %session_id,
                cap = MAX_SESSIONS_PER_AGENT,
                "terminal session limit reached — refusing"
            );
            self.emit_closed(TerminalClosed {
                session_id,
                exit_code: -1,
                error: "agent session limit reached".into(),
                recording_path: String::new(),
                recording_size: 0,
                recording_sha256: String::new(),
            });
            return;
        }

        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let (resize_tx, resize_rx) = mpsc::channel::<(u32, u32)>(8);
        let (close_tx, close_rx) = mpsc::channel::<()>(1);

        let cfg = pty::Config {
            session_id: session_id.clone(),
            cols: cols.max(1) as u16,
            rows: rows.max(1) as u16,
            shell: if shell.is_empty() { None } else { Some(shell) },
            record,
            recording_dir: self.recording_dir.clone(),
        };
        let upstream = self.upstream.clone();
        tokio::spawn(async move {
            pty::run(cfg, stdin_rx, resize_rx, close_rx, upstream).await;
        });

        self.sessions.insert(
            session_id,
            SessionHandle {
                stdin_tx,
                resize_tx,
                close_tx,
            },
        );
    }

    /// Recordings dir this manager writes to. Reused by the recording-fetch
    /// handler so panel and agent agree on the on-disk layout.
    #[must_use]
    pub fn recording_dir(&self) -> &std::path::Path {
        &self.recording_dir
    }

    pub fn input(&mut self, msg: TerminalInput) {
        let TerminalInput { session_id, data } = msg;
        let Some(handle) = self.sessions.get(&session_id) else {
            return;
        };
        // Try-send so a slow shell can't backpressure the entire stream.
        if handle.stdin_tx.try_send(data).is_err() {
            tracing::warn!(%session_id, "stdin channel full — dropping input chunk");
        }
    }

    pub fn resize(&mut self, msg: TerminalResize) {
        let TerminalResize {
            session_id,
            cols,
            rows,
        } = msg;
        let Some(handle) = self.sessions.get(&session_id) else {
            return;
        };
        let _ = handle.resize_tx.try_send((cols.max(1), rows.max(1)));
    }

    pub fn close(&mut self, msg: TerminalClose) {
        let TerminalClose { session_id } = msg;
        if let Some(handle) = self.sessions.remove(&session_id) {
            let _ = handle.close_tx.try_send(());
        }
    }

    /// Drop every active session. Called when the gRPC stream is shutting
    /// down so child processes don't outlive their bridge.
    pub fn shutdown_all(&mut self) {
        for (_, handle) in self.sessions.drain() {
            let _ = handle.close_tx.try_send(());
        }
    }

    fn emit_closed(&self, closed: TerminalClosed) {
        let upstream = self.upstream.clone();
        tokio::spawn(async move {
            let _ = upstream
                .send(AgentToPanel {
                    seq: 0,
                    payload: Some(UpPayload::TerminalClosed(closed)),
                })
                .await;
        });
    }
}

/// Best-effort cleanup hook for callers that drop the manager without
/// shutting it down explicitly.
impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

/// Default per-platform recordings dir. Mirrors the install paths used by
/// install-agent.sh and the systemd unit.
//
// Each cfg arm is the only reachable one per platform; explicit `return`
// keeps it easy to read which path applies.
#[allow(clippy::needless_return)]
#[must_use]
pub fn default_recording_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        return PathBuf::from("/var/lib/monitor-agent/recordings");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/Library/Application Support/monitor-agent/recordings");
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
        return base.join("monitor-agent").join("recordings");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("./monitor-agent-recordings")
    }
}
