//! monitor-agent-supervisor — long-lived process the OS service manager
//! launches.
//!
//! Phase 1 / M1: launch the agent, restart on exit, forward signals.
//! Phase 7 / M7: receive update commands over IPC, stage them into
//! `versions/<v>/`, perform an atomic A/B swap of the `agent` symlink,
//! and watchdog the new process for `grace_s` seconds before confirming.
//! Failure inside the grace window rolls back to `last_known_good`.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};

use monitor_agent_supervisor::{ipc, staging};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Resets the backoff if the agent stayed up at least this long before exiting.
const STABLE_RUN_THRESHOLD: Duration = Duration::from_secs(30);
/// Default grace period when the panel doesn't pin one. Long enough that an
/// agent which connects + heartbeats has actually proved itself.
const DEFAULT_GRACE: Duration = Duration::from_secs(60);

#[derive(Parser, Debug)]
#[command(
    name = "monitor-agent-supervisor",
    version = monitor_common::VERSION,
    about = "Supervisor for monitor-agent",
)]
struct Cli {
    /// Root directory that holds `agent`/`agent.exe`, `versions/`, and `state.json`.
    /// Defaults to the standard install path for the current OS.
    #[arg(long, env = "MONITOR_AGENT_ROOT")]
    root: Option<PathBuf>,

    /// Override the agent binary path. Useful for development.
    #[arg(long)]
    agent_binary: Option<PathBuf>,

    /// Override the IPC socket / named pipe path.
    #[arg(long, env = "MONITOR_SUPERVISOR_IPC")]
    ipc_path: Option<PathBuf>,

    /// Extra arguments passed through to the agent. Defaults to `["run"]`.
    #[arg(last = true)]
    agent_args: Vec<String>,
}

/// Persistent supervisor state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct State {
    /// Version directory we currently launch (`versions/<current>/`).
    current: Option<String>,
    /// Most recent version that proved stable in its grace window.
    last_known_good: Option<String>,
    /// Mid-update "next" pin — if a swap fails the run-loop reverts.
    #[serde(default)]
    staging: Option<String>,
    /// Versions that hit a rollback. Capped at the most recent 5.
    #[serde(default)]
    failed_versions: Vec<String>,
}

#[allow(clippy::needless_return)]
fn default_root() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        return PathBuf::from("/opt/monitor-agent");
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/usr/local/var/monitor-agent");
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"));
        return base.join("monitor-agent");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("./monitor-agent")
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("state.json")
}

fn load_state(root: &Path) -> State {
    let path = state_path(root);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            tracing::warn!(%err, path = %path.display(), "state.json malformed — using defaults");
            State::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(err) => {
            tracing::warn!(%err, "could not read state.json — using defaults");
            State::default()
        }
    }
}

fn save_state(root: &Path, state: &State) {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let body = match serde_json::to_vec_pretty(state) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(%err, "could not serialise state");
            return;
        }
    };
    if let Err(err) = std::fs::write(&tmp, &body) {
        tracing::warn!(%err, "could not write state.json.tmp");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        tracing::warn!(%err, "could not commit state.json");
    }
}

fn agent_bin_name() -> &'static str {
    if cfg!(windows) {
        "monitor-agent.exe"
    } else {
        "monitor-agent"
    }
}

fn current_agent_binary(root: &Path, state: &State, override_path: Option<&Path>) -> PathBuf {
    // Prefer the versioned binary `apply_swap` pinned. Falling back to the
    // override only when there's no current version (fresh install, or a
    // rollback wiped state.current).
    //
    // The previous order made every self-update a no-op: install-agent.sh
    // passes `--agent-binary /opt/monitor-agent/bin/monitor-agent`, so the
    // override was always set, so even after a successful download +
    // extract the supervisor kept running the install-time binary.
    if let Some(ref v) = state.current {
        let versioned = root.join("versions").join(v).join(agent_bin_name());
        if versioned.exists() {
            return versioned;
        }
        // Versioned dir was pruned or never landed — fall through to the
        // override / bin path as a recovery so we don't deadlock the
        // supervisor on a missing binary.
        tracing::warn!(
            version = %v,
            path = %root.join("versions").join(v).join(agent_bin_name()).display(),
            "state.current points at a binary that doesn't exist; falling back",
        );
    }
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    // Fallback: `<root>/bin/<agent>` (matches install-agent.sh layout).
    root.join("bin").join(agent_bin_name())
}

fn versions_dir(root: &Path) -> PathBuf {
    root.join("versions")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MONITOR_SUPERVISOR_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let root = cli.root.clone().unwrap_or_else(default_root);
    let state = load_state(&root);
    let agent_args = if cli.agent_args.is_empty() {
        vec!["run".to_string()]
    } else {
        cli.agent_args.clone()
    };

    tracing::info!(
        version = monitor_common::VERSION,
        root = %root.display(),
        current_version = state.current.as_deref().unwrap_or("<unset>"),
        last_known_good = state.last_known_good.as_deref().unwrap_or("<none>"),
        "supervisor starting",
    );

    let shutdown = install_shutdown_handler();

    // IPC channel: the listener pushes (Request, oneshot<Response>) tuples;
    // the run-loop owns serialisation of update operations.
    let (ipc_tx, ipc_rx) = mpsc::channel::<(ipc::Request, oneshot::Sender<ipc::Response>)>(8);
    let ipc_path = cli.ipc_path.clone().unwrap_or_else(ipc::default_ipc_path);
    let ipc_handle = tokio::spawn({
        let path = ipc_path.clone();
        let shutdown = shutdown.clone();
        async move {
            if let Err(err) = ipc::serve(path, ipc_tx, shutdown).await {
                tracing::error!(%err, "ipc server failed");
            }
        }
    });

    let (staging_results_tx, staging_results_rx) = mpsc::unbounded_channel();

    let exit = run_loop(LoopCtx {
        root,
        state,
        agent_args,
        agent_binary_override: cli.agent_binary,
        ipc_path,
        ipc_rx,
        shutdown,
        staging_results_tx,
        staging_results_rx,
        inflight: None,
    })
    .await;

    let _ = ipc_handle.await;
    exit
}

/// Watch channel: flipped to `true` when SIGTERM/SIGINT arrives.
type ShutdownRx = watch::Receiver<bool>;

fn install_shutdown_handler() -> ShutdownRx {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_signal().await;
        let _ = tx.send(true);
    });
    rx
}

#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT");
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM"),
        _ = int.recv() => tracing::info!("SIGINT"),
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Ctrl-C");
}

struct LoopCtx {
    root: PathBuf,
    state: State,
    agent_args: Vec<String>,
    agent_binary_override: Option<PathBuf>,
    ipc_path: PathBuf,
    ipc_rx: mpsc::Receiver<(ipc::Request, oneshot::Sender<ipc::Response>)>,
    shutdown: ShutdownRx,
    /// Routes completed staging outcomes back into the run loop so the
    /// reply oneshot can be answered + a [`PendingSwap`] enqueued without
    /// the run loop blocking on the staging task itself. Aborts can
    /// therefore preempt a download in progress.
    staging_results_tx: mpsc::UnboundedSender<StagingResult>,
    staging_results_rx: mpsc::UnboundedReceiver<StagingResult>,
    /// At most one staging at a time. The cancel sender lets [`Request::Abort`]
    /// preempt the in-flight download.
    inflight: Option<InflightStage>,
}

struct InflightStage {
    rollout_id: String,
    version: String,
    cancel: oneshot::Sender<()>,
}

struct StagingResult {
    rollout_id: String,
    version: String,
    grace_s: u32,
    reply: oneshot::Sender<ipc::Response>,
    outcome: Result<staging::Staged, staging::StagingError>,
}

/// Holding-pen for an in-flight rollout. Set by IPC, consumed by the run
/// loop on the next agent restart so the swap happens in a single place.
struct PendingSwap {
    new_version: String,
    new_binary: PathBuf,
    grace: Duration,
}

async fn run_loop(mut ctx: LoopCtx) -> anyhow::Result<()> {
    let mut backoff = MIN_BACKOFF;
    let mut pending_swap: Option<PendingSwap> = None;
    let mut grace_window: Option<(String, Instant, Duration)> = None;

    loop {
        if *ctx.shutdown.borrow() {
            return Ok(());
        }

        // Apply any swap requested via IPC since we last looped.
        if let Some(swap) = pending_swap.take() {
            apply_swap(&mut ctx, swap, &mut grace_window);
        }

        let agent_binary =
            current_agent_binary(&ctx.root, &ctx.state, ctx.agent_binary_override.as_deref());
        let started_at = Instant::now();
        let child = spawn_agent(&agent_binary, &ctx.agent_args, &ctx.ipc_path).await;

        let child = match child {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(%err, "failed to spawn agent");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = ctx.shutdown.changed() => return Ok(()),
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let status = run_child(child, &mut ctx, &mut pending_swap).await?;
        let uptime = started_at.elapsed();

        if *ctx.shutdown.borrow() {
            return Ok(());
        }

        // Resolve any open grace window.
        if let Some((version, started, grace)) = grace_window.take() {
            if uptime >= grace {
                confirm_version(&mut ctx, &version);
            } else {
                tracing::warn!(
                    version,
                    uptime_s = uptime.as_secs(),
                    grace_s = grace.as_secs(),
                    "agent exited inside grace window — rolling back"
                );
                rollback(&mut ctx, &version);
            }
            // Suppress the "unused" warning when grace_window's start time
            // turns out to be a no-op data point in the success path.
            let _ = started;
        }

        if status.success() {
            tracing::info!(?uptime, "agent exited cleanly — restarting");
        } else {
            tracing::warn!(?status, ?uptime, "agent exited with failure");
        }

        if uptime >= STABLE_RUN_THRESHOLD {
            backoff = MIN_BACKOFF;
        }

        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = ctx.shutdown.changed() => return Ok(()),
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn apply_swap(
    ctx: &mut LoopCtx,
    swap: PendingSwap,
    grace_window: &mut Option<(String, Instant, Duration)>,
) {
    let previous = ctx.state.current.clone();
    tracing::info!(
        from = previous.as_deref().unwrap_or("<unset>"),
        to = %swap.new_version,
        binary = %swap.new_binary.display(),
        "applying staged swap"
    );
    ctx.state.staging = None;
    ctx.state.last_known_good = previous;
    ctx.state.current = Some(swap.new_version.clone());
    save_state(&ctx.root, &ctx.state);
    *grace_window = Some((swap.new_version, Instant::now(), swap.grace));
}

fn confirm_version(ctx: &mut LoopCtx, version: &str) {
    tracing::info!(version, "swap confirmed");
    if ctx.state.current.as_deref() == Some(version) {
        ctx.state.last_known_good = Some(version.to_owned());
        save_state(&ctx.root, &ctx.state);
    }
    prune_old_versions(ctx);
}

fn rollback(ctx: &mut LoopCtx, failed_version: &str) {
    let lkg = ctx.state.last_known_good.clone();
    record_failed(ctx, failed_version);
    if let Some(prev) = lkg {
        tracing::info!(failed = failed_version, restoring = %prev, "rolling back");
        ctx.state.current = Some(prev);
    } else {
        tracing::error!(
            failed = failed_version,
            "no last_known_good to roll back to — supervisor will keep retrying"
        );
        ctx.state.current = None;
    }
    ctx.state.staging = None;
    save_state(&ctx.root, &ctx.state);
}

fn record_failed(ctx: &mut LoopCtx, version: &str) {
    if !ctx.state.failed_versions.contains(&version.to_owned()) {
        ctx.state.failed_versions.push(version.to_owned());
    }
    while ctx.state.failed_versions.len() > 5 {
        ctx.state.failed_versions.remove(0);
    }
}

fn prune_old_versions(ctx: &mut LoopCtx) {
    let keep: Vec<String> = [ctx.state.current.clone(), ctx.state.last_known_good.clone()]
        .into_iter()
        .flatten()
        .collect();
    let dir = versions_dir(&ctx.root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut existing: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    existing.sort();
    while existing.len() > 3 {
        let candidate = existing.remove(0);
        if !keep.contains(&candidate) {
            tracing::info!(version = %candidate, "removing old version directory");
            let _ = std::fs::remove_dir_all(dir.join(candidate));
        }
    }
}

async fn spawn_agent(binary: &Path, args: &[String], ipc_path: &Path) -> anyhow::Result<Child> {
    Command::new(binary)
        .args(args)
        .env("MONITOR_SUPERVISOR_IPC", ipc_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {}", binary.display()))
}

async fn run_child(
    mut child: Child,
    ctx: &mut LoopCtx,
    pending_swap: &mut Option<PendingSwap>,
) -> anyhow::Result<ExitStatus> {
    loop {
        tokio::select! {
            result = child.wait() => {
                return result.context("waiting for agent exit");
            }
            ipc_msg = ctx.ipc_rx.recv() => {
                match ipc_msg {
                    Some((request, reply)) => {
                        handle_ipc(ctx, request, reply);
                    }
                    None => continue,
                }
            }
            staged = ctx.staging_results_rx.recv() => {
                if let Some(result) = staged {
                    if apply_staging_result(ctx, result, pending_swap) {
                        tracing::info!("agent restart triggered for swap");
                        let _ = child.kill().await;
                    }
                }
            }
            _ = ctx.shutdown.changed() => {
                tracing::info!("forwarding shutdown to agent");
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    #[allow(unsafe_code)]
                    unsafe {
                        libc_kill(pid as i32, 15 /* SIGTERM */);
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = child.start_kill();
                }
                match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                    Ok(res) => return res.context("waiting for agent graceful exit"),
                    Err(_) => {
                        tracing::warn!("agent did not exit within 10s — killing");
                        let _ = child.kill().await;
                        return child.wait().await.context("waiting after kill");
                    }
                }
            }
        }
    }
}

/// Dispatch one IPC request. `Update` spawns a cancellable staging task and
/// returns immediately so the run loop can keep polling — the eventual
/// success/failure response is sent from inside the spawned task. `Abort`
/// fires the cancel oneshot for the matching in-flight stage and replies
/// instantly.
fn handle_ipc(ctx: &mut LoopCtx, request: ipc::Request, reply: oneshot::Sender<ipc::Response>) {
    match request {
        ipc::Request::Update {
            rollout_id,
            version,
            asset_url,
            sha256,
            attestation_url,
            grace_s,
        } => {
            if ctx.inflight.is_some() {
                let _ = reply.send(ipc::Response::error("another staging is in progress"));
                return;
            }
            tracing::info!(%rollout_id, %version, "supervisor received update request");
            ctx.state.staging = Some(version.clone());
            save_state(&ctx.root, &ctx.state);

            let (cancel_tx, cancel_rx) = oneshot::channel();
            ctx.inflight = Some(InflightStage {
                rollout_id: rollout_id.clone(),
                version: version.clone(),
                cancel: cancel_tx,
            });

            let versions = versions_dir(&ctx.root);
            let results_tx = ctx.staging_results_tx.clone();
            tokio::spawn(async move {
                let outcome = staging::stage_cancellable(
                    &versions,
                    &version,
                    &asset_url,
                    &sha256,
                    &attestation_url,
                    Some(cancel_rx),
                )
                .await;
                let _ = results_tx.send(StagingResult {
                    rollout_id,
                    version,
                    grace_s,
                    reply,
                    outcome,
                });
            });
        }
        ipc::Request::Abort { rollout_id } => {
            let matched = ctx
                .inflight
                .as_ref()
                .is_some_and(|stage| stage.rollout_id == rollout_id);
            if matched {
                if let Some(stage) = ctx.inflight.take() {
                    tracing::info!(%rollout_id, version = %stage.version, "abort: cancelling staging");
                    let _ = stage.cancel.send(());
                }
                let _ = reply.send(ipc::Response::ok());
            } else {
                let _ = reply.send(ipc::Response::error("no matching staging in progress"));
            }
        }
    }
}

/// Returns true if the result produced a `PendingSwap`, signalling that the
/// caller should restart the agent.
fn apply_staging_result(
    ctx: &mut LoopCtx,
    result: StagingResult,
    pending_swap: &mut Option<PendingSwap>,
) -> bool {
    let StagingResult {
        rollout_id,
        version,
        grace_s,
        reply,
        outcome,
    } = result;

    // Drop the inflight slot regardless — the staging task is done.
    if let Some(slot) = ctx.inflight.take() {
        if slot.rollout_id != rollout_id {
            // Shouldn't happen given we serialise on the run loop, but if a
            // race ever opens we put it back; the new in-flight wins.
            ctx.inflight = Some(slot);
        }
    }

    match outcome {
        Ok(staged) => {
            *pending_swap = Some(PendingSwap {
                new_version: version,
                new_binary: staged.agent_binary,
                grace: Duration::from_secs(u64::from(grace_s.max(1))).max(DEFAULT_GRACE),
            });
            let _ = reply.send(ipc::Response::ok());
            true
        }
        Err(err) => {
            tracing::warn!(%err, version, "staging failed");
            ctx.state.staging = None;
            save_state(&ctx.root, &ctx.state);
            let _ = reply.send(ipc::Response::error(err.to_string()));
            false
        }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig);
}

#[cfg(test)]
mod tests {
    use super::{current_agent_binary, State};
    use std::path::Path;

    #[test]
    fn current_binary_prefers_versioned_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let versioned = root.join("versions").join("v0.2.2");
        std::fs::create_dir_all(&versioned).unwrap();
        let bin = versioned.join(super::agent_bin_name());
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();

        let state = State {
            current: Some("v0.2.2".into()),
            ..Default::default()
        };
        let override_path = Path::new("/opt/monitor-agent/bin/monitor-agent");
        let chosen = current_agent_binary(root, &state, Some(override_path));
        assert_eq!(
            chosen, bin,
            "state.current must beat the install-time override; otherwise self-update is a no-op"
        );
    }

    #[test]
    fn current_binary_falls_back_to_override_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let state = State::default();
        let override_path = Path::new("/opt/monitor-agent/bin/monitor-agent");
        let chosen = current_agent_binary(tmp.path(), &state, Some(override_path));
        assert_eq!(chosen, override_path);
    }

    #[test]
    fn current_binary_falls_back_to_override_if_versioned_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = State {
            current: Some("v9.9.9".into()),
            ..Default::default()
        };
        let override_path = Path::new("/opt/monitor-agent/bin/monitor-agent");
        let chosen = current_agent_binary(tmp.path(), &state, Some(override_path));
        assert_eq!(
            chosen, override_path,
            "missing versioned binary must not deadlock the supervisor"
        );
    }
}
