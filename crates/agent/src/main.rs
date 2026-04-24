//! monitor-agent entry point.
//!
//! CLI:
//!   monitor-agent configure --endpoint URL --token TOKEN [--heartbeat N]
//!   monitor-agent run
//!   monitor-agent self-check
//!   monitor-agent --version
//!
//! Config path precedence: `--config PATH` → `$MONITOR_AGENT_CONFIG` → platform default.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use monitor_agent::{config, register, stream as stream_mod, transport};
use tokio::sync::watch;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser, Debug)]
#[command(
    name = "monitor-agent",
    version = monitor_common::VERSION,
    about = "server-monitor-rs agent",
)]
struct Cli {
    /// Override the config file path.
    #[arg(short, long, env = "MONITOR_AGENT_CONFIG", global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write (or overwrite) the agent's config file with endpoint + join token.
    Configure {
        /// Panel URL (http:// or https://).
        #[arg(long, allow_hyphen_values = true)]
        endpoint: String,
        /// Join token issued by the panel when the server was added.
        /// Base64-encoded tokens may start with `-`, so hyphen values are allowed.
        #[arg(long, allow_hyphen_values = true)]
        token: String,
        #[arg(long, default_value_t = 10)]
        heartbeat: u64,
    },

    /// Run the agent in the foreground — the mode systemd / launchd / service wrappers invoke.
    Run,

    /// Quick health check: load config + parse endpoint + exit 0. Used by the
    /// supervisor during self-update staging to reject broken binaries.
    SelfCheck,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let path = config::resolve_path(cli.config.as_deref());

    match cli.command {
        Command::Configure {
            endpoint,
            token,
            heartbeat,
        } => cmd_configure(&path, endpoint, token, heartbeat),
        Command::Run => cmd_run(&path).await,
        Command::SelfCheck => cmd_self_check(&path),
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_env("MONITOR_AGENT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .init();
}

fn cmd_configure(
    path: &std::path::Path,
    endpoint: String,
    token: String,
    heartbeat: u64,
) -> anyhow::Result<()> {
    // Parse the endpoint eagerly so an operator typo is reported before writing.
    monitor_common::AgentEndpoint::parse(&endpoint).context("parsing --endpoint")?;

    let cfg = config::AgentConfig {
        endpoint,
        join_token: Some(token),
        agent_id: None,
        server_token: None,
        heartbeat_interval_s: heartbeat,
    };
    cfg.save(path).context("writing config")?;
    tracing::info!(path = %path.display(), "agent configured — run `monitor-agent run` next");
    Ok(())
}

fn cmd_self_check(path: &std::path::Path) -> anyhow::Result<()> {
    let cfg = config::AgentConfig::load(path).context("loading config")?;
    cfg.parsed_endpoint().context("endpoint parse")?;
    println!(
        "self-check ok: endpoint={} registered={}",
        cfg.endpoint,
        !cfg.needs_register()
    );
    Ok(())
}

async fn cmd_run(path: &std::path::Path) -> anyhow::Result<()> {
    let mut cfg = config::AgentConfig::load(path).context("loading config")?;
    tracing::info!(
        version = monitor_common::VERSION,
        endpoint = %cfg.endpoint,
        "monitor-agent starting",
    );

    let parsed = cfg.parsed_endpoint().context("bad endpoint")?;
    let channel = transport::build_channel(&parsed).context("build channel")?;

    if cfg.needs_register() {
        cfg = register_with_retry(&channel, cfg, path).await?;
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_signal().await;
        let _ = shutdown_tx.send(true);
    });

    stream_mod::run(channel, cfg, shutdown_rx).await?;
    tracing::info!("monitor-agent exited");
    Ok(())
}

/// Call Register with backoff until it succeeds or we see a fatal
/// (non-retryable) error, persisting the result on success.
async fn register_with_retry(
    channel: &tonic::transport::Channel,
    mut cfg: config::AgentConfig,
    path: &std::path::Path,
) -> anyhow::Result<config::AgentConfig> {
    use std::time::Duration;

    let mut backoff = Duration::from_secs(1);
    loop {
        match register::register(channel.clone(), &cfg).await {
            Ok(reg) => {
                cfg.agent_id = Some(reg.agent_id.clone());
                cfg.server_token = Some(reg.server_token);
                cfg.join_token = None;
                cfg.save(path)?;
                tracing::info!(agent_id = %reg.agent_id, "registered");
                return Ok(cfg);
            }
            Err(err) => {
                // If the panel explicitly rejected our join_token, keep retrying
                // anyway — the admin may re-seed the token on the panel side.
                tracing::warn!(%err, "register failed, retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
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
