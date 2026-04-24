//! monitor-agent-supervisor — the long-lived process the OS service manager
//! (systemd / launchd / OpenRC / Windows Service) actually launches.
//!
//! M1 scope (this file): launch the agent child process, forward signals,
//! restart with exponential backoff on non-zero exit, stop cleanly on SIGTERM.
//!
//! M7 scope (future): IPC with the agent for self-update coordination,
//! atomic swap of `agent` symlink, grace-period probing of the new version,
//! automatic rollback to `last_known_good`.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Resets the backoff if the agent stayed up at least this long before exiting.
const STABLE_RUN_THRESHOLD: Duration = Duration::from_secs(30);

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

    /// Extra arguments passed through to the agent. Defaults to `["run"]`.
    #[arg(last = true)]
    agent_args: Vec<String>,
}

/// Persistent supervisor state. M1 reads it if it exists; M7 will write to it
/// during update coordination (current / staging / last_known_good).
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    current: Option<String>,
    last_known_good: Option<String>,
    #[serde(default)]
    staging: Option<String>,
}

// Explicit `return` in cfg-gated arms makes it obvious that exactly one is
// reachable per platform; clippy would otherwise flag them as needless.
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
        let base = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Program Files"));
        return base.join("monitor-agent");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("./monitor-agent")
    }
}

fn load_state(root: &Path) -> State {
    let path = root.join("state.json");
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

fn resolve_agent_binary(root: &Path, state: &State, override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    if let Some(ref v) = state.current {
        return root.join("versions").join(v).join(agent_bin_name());
    }
    // Fallback: `<root>/agent` (symlink typically maintained by the installer).
    root.join(agent_bin_name())
}

fn agent_bin_name() -> &'static str {
    if cfg!(windows) {
        "agent.exe"
    } else {
        "agent"
    }
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

    let agent_binary = resolve_agent_binary(&root, &state, cli.agent_binary.as_deref());
    let agent_args = if cli.agent_args.is_empty() {
        vec!["run".to_string()]
    } else {
        cli.agent_args.clone()
    };

    tracing::info!(
        version = monitor_common::VERSION,
        root = %root.display(),
        agent = %agent_binary.display(),
        ?agent_args,
        current_version = state.current.as_deref().unwrap_or("<unset>"),
        "supervisor starting",
    );

    let shutdown = install_shutdown_handler();
    run_loop(&agent_binary, &agent_args, shutdown).await
}

/// Watch channel: flipped to `true` when SIGTERM/SIGINT arrives.
type ShutdownRx = tokio::sync::watch::Receiver<bool>;

fn install_shutdown_handler() -> ShutdownRx {
    let (tx, rx) = tokio::sync::watch::channel(false);
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

/// Supervisor main loop: launch the agent, watch it, restart with backoff.
async fn run_loop(
    agent_binary: &Path,
    agent_args: &[String],
    mut shutdown: ShutdownRx,
) -> anyhow::Result<()> {
    let mut backoff = MIN_BACKOFF;

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }

        let started_at = Instant::now();
        let child = spawn_agent(agent_binary, agent_args).await;

        let child = match child {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(%err, "failed to spawn agent");
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.changed() => return Ok(()),
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let status = run_child(child, &mut shutdown).await?;
        let uptime = started_at.elapsed();

        if *shutdown.borrow() {
            return Ok(());
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
            _ = shutdown.changed() => return Ok(()),
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn spawn_agent(binary: &Path, args: &[String]) -> anyhow::Result<Child> {
    Command::new(binary)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {}", binary.display()))
}

async fn run_child(mut child: Child, shutdown: &mut ShutdownRx) -> anyhow::Result<ExitStatus> {
    tokio::select! {
        result = child.wait() => Ok(result.context("waiting for agent exit")?),
        _ = shutdown.changed() => {
            tracing::info!("forwarding shutdown to agent");
            // Best-effort graceful termination.
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                // SAFETY: `kill(2)` with a process ID we just spawned is a
                // well-defined syscall; any failure is logged by the caller's
                // subsequent `wait`.
                #[allow(unsafe_code)]
                unsafe {
                    libc_kill(pid as i32, 15 /* SIGTERM */);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }
            // Give the agent up to 10s to quit; then force-kill.
            match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(res) => Ok(res.context("waiting for agent graceful exit")?),
                Err(_) => {
                    tracing::warn!("agent did not exit within 10s — killing");
                    let _ = child.kill().await;
                    Ok(child.wait().await.context("waiting after kill")?)
                }
            }
        }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    // Avoids pulling `libc` just for this single FFI call. A failure returns -1
    // and we rely on the subsequent `wait()` to reap the process anyway.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig);
}
