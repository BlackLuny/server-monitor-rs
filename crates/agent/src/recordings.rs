//! Streaming asciinema recordings back to the panel.
//!
//! When the panel asks for a recorded session via `RecordingFetchRequest`,
//! the agent looks up `<recording_dir>/<session_id>.cast` and ships it as a
//! sequence of `RecordingFetchChunk` frames over the existing AgentToPanel
//! stream. Always terminates with one frame carrying `eof = true`; errors
//! are reported in-band via the same final frame's `error` field so the
//! panel never has to wait for a timeout.

use std::path::PathBuf;

use monitor_proto::v1::{agent_to_panel::Payload as UpPayload, AgentToPanel, RecordingFetchChunk};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

const CHUNK_BYTES: usize = 64 * 1024;

/// Hard cap on the bytes shipped per fetch. Recordings are tiny in practice
/// (kilobytes per minute of session); the cap keeps a corrupt or oversized
/// file from monopolising the upstream channel.
const MAX_FETCH_BYTES: u64 = 256 * 1024 * 1024;

/// Read `<dir>/<session_id>.cast` and stream it to the panel. Always emits
/// at least one final frame with `eof = true`; on failure that frame also
/// carries `error`.
pub async fn serve_fetch(
    session_id: String,
    dir: PathBuf,
    upstream: mpsc::Sender<AgentToPanel>,
    mut seq: u64,
) {
    // Path-traversal guard: session_id is panel-supplied. We treat it as an
    // opaque token, refuse anything that could escape `dir`.
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        send_error(&upstream, &mut seq, &session_id, "invalid session_id").await;
        return;
    }
    let path = dir.join(format!("{session_id}.cast"));
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(err) => {
            send_error(
                &upstream,
                &mut seq,
                &session_id,
                &format!("recording not found: {err}"),
            )
            .await;
            return;
        }
    };
    if metadata.len() > MAX_FETCH_BYTES {
        send_error(
            &upstream,
            &mut seq,
            &session_id,
            &format!("recording too large: {} bytes", metadata.len()),
        )
        .await;
        return;
    }

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(err) => {
            send_error(
                &upstream,
                &mut seq,
                &session_id,
                &format!("recording open failed: {err}"),
            )
            .await;
            return;
        }
    };

    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut offset: u64 = 0;
    loop {
        match file.read(&mut buf).await {
            Ok(0) => {
                let frame = AgentToPanel {
                    seq,
                    payload: Some(UpPayload::RecordingChunk(RecordingFetchChunk {
                        session_id: session_id.clone(),
                        offset,
                        data: Vec::new(),
                        eof: true,
                        error: String::new(),
                    })),
                };
                let _ = upstream.send(frame).await;
                return;
            }
            Ok(n) => {
                let frame = AgentToPanel {
                    seq,
                    payload: Some(UpPayload::RecordingChunk(RecordingFetchChunk {
                        session_id: session_id.clone(),
                        offset,
                        data: buf[..n].to_vec(),
                        eof: false,
                        error: String::new(),
                    })),
                };
                seq = seq.saturating_add(1);
                if upstream.send(frame).await.is_err() {
                    return;
                }
                offset = offset.saturating_add(n as u64);
            }
            Err(err) => {
                send_error(
                    &upstream,
                    &mut seq,
                    &session_id,
                    &format!("recording read failed: {err}"),
                )
                .await;
                return;
            }
        }
    }
}

async fn send_error(
    upstream: &mpsc::Sender<AgentToPanel>,
    seq: &mut u64,
    session_id: &str,
    msg: &str,
) {
    let frame = AgentToPanel {
        seq: *seq,
        payload: Some(UpPayload::RecordingChunk(RecordingFetchChunk {
            session_id: session_id.to_owned(),
            offset: 0,
            data: Vec::new(),
            eof: true,
            error: msg.to_owned(),
        })),
    };
    *seq = seq.saturating_add(1);
    let _ = upstream.send(frame).await;
}
