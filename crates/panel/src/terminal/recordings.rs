//! Streaming recordings from the agent to an HTTP client.
//!
//! Flow:
//!   1. Browser hits `GET /api/recordings/:session_id/download`.
//!   2. Handler resolves the session → agent, registers itself in
//!      [`RecordingHub`], pushes `PanelToAgent::RecordingFetch`.
//!   3. Agent replies with `RecordingFetchChunk` frames; the gRPC inbound
//!      loop calls [`RecordingHub::deliver_chunk`].
//!   4. The handler streams those chunks straight into the response body
//!      until it sees `eof`.
//!
//! One outstanding fetch per session at a time. A second request supersedes
//! the first by replacing the channel — the older fetch sees a closed
//! channel and ends with a "superseded" error.

use std::sync::Arc;

use dashmap::DashMap;
use monitor_proto::v1::RecordingFetchChunk;
use tokio::sync::mpsc;

/// Per-session buffer. Recordings are tiny in practice; 64 frames at 64 KiB
/// is 4 MiB of headroom, which is more than enough for the slowest browser
/// download to absorb.
const CHANNEL_BUFFER: usize = 64;

#[derive(Clone, Default)]
pub struct RecordingHub {
    inner: Arc<DashMap<String, mpsc::Sender<RecordingFetchChunk>>>,
}

#[derive(Debug)]
pub enum RecordingFetchError {
    /// The agent emitted a frame with `error` set — the message is verbatim.
    Agent(String),
    /// Stream ended (channel closed) before we saw an `eof` frame.
    StreamEnded,
}

impl std::fmt::Display for RecordingFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent(msg) => write!(f, "agent error: {msg}"),
            Self::StreamEnded => write!(f, "stream ended before eof"),
        }
    }
}

impl std::error::Error for RecordingFetchError {}

impl RecordingHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a fetch slot for `session_id`. The returned receiver gets the
    /// raw chunks the agent emits. The guard removes the slot on drop, so
    /// the gRPC loop stops trying to deliver chunks once the HTTP handler
    /// is gone (client disconnect, error, etc.).
    pub fn open(&self, session_id: String) -> (mpsc::Receiver<RecordingFetchChunk>, FetchGuard) {
        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER);
        // Replace any prior fetcher; the old one will get a closed-channel
        // signal and shut down.
        self.inner.insert(session_id.clone(), tx);
        let guard = FetchGuard {
            inner: self.inner.clone(),
            session_id,
        };
        (rx, guard)
    }

    /// Route an agent-side chunk to whoever is waiting on it. Best-effort:
    /// a slow / disconnected client just gets backpressure; if the channel
    /// is full we drop, but the buffer is generous enough that this rarely
    /// fires for small recordings.
    pub fn deliver_chunk(&self, chunk: RecordingFetchChunk) {
        if let Some(slot) = self.inner.get(&chunk.session_id) {
            let _ = slot.try_send(chunk);
        }
    }
}

pub struct FetchGuard {
    inner: Arc<DashMap<String, mpsc::Sender<RecordingFetchChunk>>>,
    session_id: String,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        self.inner.remove(&self.session_id);
    }
}
