//! Tiny line-delimited JSON RPC server for the supervisor.
//!
//! Wire format mirrors `monitor-agent/src/updates.rs`:
//!
//!   request:   `{"method":"update", "rollout_id":..., "version":..., "asset_url":..., "sha256":..., "grace_s":...}\n`
//!   response:  `{"ok":true}` | `{"ok":false,"error":"…"}`
//!
//! One request per connection, then close. Keeps the surface small.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Update {
        rollout_id: String,
        version: String,
        asset_url: String,
        sha256: String,
        // The Sigstore attestation bundle URL. Reserved for future
        // verification; today only sha256 is enforced.
        #[serde(default)]
        #[allow(dead_code)]
        attestation_url: String,
        #[serde(default = "default_grace")]
        grace_s: u32,
    },
}

fn default_grace() -> u32 {
    60
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

/// Default IPC socket / pipe path. Must match the agent's lookup.
//
// Each cfg arm is the only reachable one per platform; explicit `return`
// keeps it obvious which path applies on each OS.
#[cfg(unix)]
#[allow(clippy::needless_return)]
pub fn default_ipc_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MONITOR_SUPERVISOR_IPC") {
        return PathBuf::from(p);
    }
    #[cfg(target_os = "linux")]
    {
        return PathBuf::from("/run/monitor-agent/supervisor.sock");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/usr/local/var/monitor-agent/supervisor.sock");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        PathBuf::from("/tmp/monitor-agent-supervisor.sock")
    }
}

#[cfg(windows)]
pub fn default_ipc_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MONITOR_SUPERVISOR_IPC") {
        return PathBuf::from(p);
    }
    PathBuf::from(r"\\.\pipe\monitor-agent-supervisor")
}

/// Listen forever. Each request gets dispatched into `requests_tx`; the
/// run-loop owns the actual swap so it can serialise updates.
#[cfg(unix)]
pub async fn serve(
    socket_path: PathBuf,
    requests_tx: mpsc::Sender<(Request, tokio::sync::oneshot::Sender<Response>)>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // Stale socket from a previous crash blocks bind.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(path = %socket_path.display(), "supervisor IPC listening");

    let requests_tx = Arc::new(requests_tx);

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, _) = match accept {
                    Ok(a) => a,
                    Err(err) => {
                        tracing::warn!(%err, "ipc accept failed");
                        continue;
                    }
                };
                let tx = requests_tx.clone();
                tokio::spawn(async move {
                    let (read_half, mut write_half) = stream.into_split();
                    let mut reader = BufReader::new(read_half);
                    let mut line = String::new();
                    if let Err(err) = reader.read_line(&mut line).await {
                        tracing::debug!(%err, "ipc read failed");
                        return;
                    }
                    let resp = match serde_json::from_str::<Request>(line.trim()) {
                        Ok(req) => {
                            let (rtx, rrx) = tokio::sync::oneshot::channel();
                            if tx.send((req, rtx)).await.is_err() {
                                Response::error("supervisor stopped")
                            } else {
                                rrx.await.unwrap_or_else(|_| Response::error("supervisor dropped reply"))
                            }
                        }
                        Err(err) => Response::error(format!("bad request: {err}")),
                    };
                    let mut bytes = match serde_json::to_vec(&resp) {
                        Ok(b) => b,
                        Err(_) => b"{\"ok\":false}".to_vec(),
                    };
                    bytes.push(b'\n');
                    let _ = write_half.write_all(&bytes).await;
                    let _ = write_half.shutdown().await;
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("supervisor IPC stopping");
                    let _ = std::fs::remove_file(&socket_path);
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(windows)]
pub async fn serve(
    pipe_path: PathBuf,
    requests_tx: mpsc::Sender<(Request, tokio::sync::oneshot::Sender<Response>)>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let path_str = pipe_path.to_string_lossy().into_owned();
    let requests_tx = Arc::new(requests_tx);
    tracing::info!(path = %path_str, "supervisor IPC listening");

    loop {
        let mut server = ServerOptions::new()
            .first_pipe_instance(false)
            .create(&path_str)?;
        tokio::select! {
            connect = server.connect() => {
                if let Err(err) = connect {
                    tracing::warn!(%err, "named pipe connect failed");
                    continue;
                }
                let tx = requests_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(&mut server);
                    let mut line = String::new();
                    if let Err(err) = reader.read_line(&mut line).await {
                        tracing::debug!(%err, "ipc read failed");
                        return;
                    }
                    let resp = match serde_json::from_str::<Request>(line.trim()) {
                        Ok(req) => {
                            let (rtx, rrx) = tokio::sync::oneshot::channel();
                            if tx.send((req, rtx)).await.is_err() {
                                Response::error("supervisor stopped")
                            } else {
                                rrx.await.unwrap_or_else(|_| Response::error("supervisor dropped reply"))
                            }
                        }
                        Err(err) => Response::error(format!("bad request: {err}")),
                    };
                    let mut bytes = match serde_json::to_vec(&resp) {
                        Ok(b) => b,
                        Err(_) => b"{\"ok\":false}".to_vec(),
                    };
                    bytes.push(b'\n');
                    let _ = server.write_all(&bytes).await;
                    let _ = server.shutdown().await;
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("supervisor IPC stopping");
                    return Ok(());
                }
            }
        }
    }
}
