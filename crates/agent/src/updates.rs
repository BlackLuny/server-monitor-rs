//! Agent → supervisor IPC for self-update commands.
//!
//! The agent never replaces its own binary. When it receives an
//! `UpdateAgent` command from the panel it forwards the request to the
//! supervisor process via a unix-domain socket (Linux/macOS) or named
//! pipe (Windows). The supervisor downloads, verifies, swaps the symlink,
//! and restarts the agent — once the new agent connects to the panel its
//! `Register` call updates `servers.agent_version`, which the panel uses
//! to mark the rollout assignment succeeded.
//!
//! Wire format: newline-delimited JSON. Single request, single response,
//! one connection per call. The protocol is intentionally tiny so the
//! supervisor can stay schema-stable across agent versions.

use std::path::PathBuf;

use monitor_proto::v1::{
    agent_to_panel::Payload as UpPayload, AgentToPanel, UpdateAgent, UpdateState, UpdateStatus,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Env var the supervisor sets when launching the agent. When unset, the
/// agent looks at platform defaults below.
const IPC_PATH_ENV: &str = "MONITOR_SUPERVISOR_IPC";

/// Default IPC socket path per platform. Mirrors install-agent.sh layout.
//
// Each cfg arm is the only reachable one per platform; explicit `return`
// keeps it obvious which path applies.
#[cfg(unix)]
#[allow(clippy::needless_return)]
fn default_ipc_path() -> PathBuf {
    if let Some(p) = std::env::var_os(IPC_PATH_ENV) {
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
fn default_ipc_path() -> PathBuf {
    if let Some(p) = std::env::var_os(IPC_PATH_ENV) {
        return PathBuf::from(p);
    }
    PathBuf::from(r"\\.\pipe\monitor-agent-supervisor")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "method")]
enum Request {
    Update {
        rollout_id: String,
        version: String,
        asset_url: String,
        sha256: String,
        attestation_url: String,
        grace_s: u32,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Response {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

/// Forward an `UpdateAgent` command to the supervisor. Always emits at
/// least one `UpdateStatus` upstream so the panel knows what happened.
pub async fn handle_update(cmd: UpdateAgent, upstream: mpsc::Sender<AgentToPanel>, seq: &mut u64) {
    let UpdateAgent {
        rollout_id,
        version,
        asset_url,
        sha256,
        attestation_url,
        grace_s,
    } = cmd;

    if rollout_id.is_empty() {
        tracing::warn!("UpdateAgent dropped — empty rollout_id");
        return;
    }

    // First report: we accepted the command and are about to ask the
    // supervisor. UI flips the assignment row to "sent" on this.
    emit_status(
        &upstream,
        seq,
        &rollout_id,
        &version,
        UpdateState::Downloading,
        "agent → supervisor",
    )
    .await;

    let request = Request::Update {
        rollout_id: rollout_id.clone(),
        version: version.clone(),
        asset_url,
        sha256,
        attestation_url,
        grace_s,
    };

    let path = default_ipc_path();
    match send_request(&path, &request).await {
        Ok(resp) if resp.ok => {
            // Supervisor staged successfully — it'll handle the swap +
            // restart. The new agent's Register tells the panel the rest.
            emit_status(
                &upstream,
                seq,
                &rollout_id,
                &version,
                UpdateState::Staged,
                "supervisor staged",
            )
            .await;
        }
        Ok(resp) => {
            let detail = resp.error.unwrap_or_else(|| "supervisor refused".into());
            tracing::warn!(%rollout_id, %detail, "supervisor rejected update");
            emit_status(
                &upstream,
                seq,
                &rollout_id,
                &version,
                UpdateState::Failed,
                &detail,
            )
            .await;
        }
        Err(err) => {
            tracing::warn!(%err, ipc = %path.display(), "supervisor IPC failed");
            emit_status(
                &upstream,
                seq,
                &rollout_id,
                &version,
                UpdateState::Failed,
                &format!("supervisor IPC: {err}"),
            )
            .await;
        }
    }
}

async fn emit_status(
    upstream: &mpsc::Sender<AgentToPanel>,
    seq: &mut u64,
    rollout_id: &str,
    version: &str,
    state: UpdateState,
    detail: &str,
) {
    let msg = AgentToPanel {
        seq: *seq,
        payload: Some(UpPayload::UpdateStatus(UpdateStatus {
            rollout_id: rollout_id.to_owned(),
            version: version.to_owned(),
            state: state as i32,
            detail: detail.to_owned(),
        })),
    };
    *seq += 1;
    if upstream.send(msg).await.is_err() {
        tracing::debug!("upstream closed before UpdateStatus could ship");
    }
}

#[cfg(unix)]
async fn send_request(path: &std::path::Path, req: &Request) -> Result<Response, std::io::Error> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(path).await?;
    let (read_half, mut write_half) = stream.into_split();

    let mut payload = serde_json::to_vec(req).map_err(|e| std::io::Error::other(e.to_string()))?;
    payload.push(b'\n');
    write_half.write_all(&payload).await?;
    write_half.flush().await?;
    write_half.shutdown().await.ok();

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        return Err(std::io::Error::other("supervisor closed without reply"));
    }
    serde_json::from_str(line.trim()).map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(windows)]
async fn send_request(path: &std::path::Path, req: &Request) -> Result<Response, std::io::Error> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ClientOptions;

    let path_str = path.to_string_lossy().into_owned();
    let mut client = ClientOptions::new().open(path_str)?;

    let mut payload = serde_json::to_vec(req).map_err(|e| std::io::Error::other(e.to_string()))?;
    payload.push(b'\n');
    client.write_all(&payload).await?;
    client.flush().await?;

    let mut reader = BufReader::new(&mut client);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        return Err(std::io::Error::other("supervisor closed without reply"));
    }
    serde_json::from_str(line.trim()).map_err(|e| std::io::Error::other(e.to_string()))
}
