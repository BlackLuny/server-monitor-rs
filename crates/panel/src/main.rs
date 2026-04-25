//! monitor-panel entry point.
//!
//! Boots the tokio runtime, loads configuration, initializes logging, and
//! spawns the HTTP and gRPC servers side-by-side. All substantive logic lives
//! in the sibling library (`lib.rs`) so it stays testable.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use monitor_panel::{
    config::Config, db, grpc, http_server, metrics, shutdown, state::AppState, telemetry,
};

#[derive(Debug, Parser)]
#[command(
    name = "monitor-panel",
    version = monitor_common::VERSION,
    about = "server-monitor-rs control panel",
)]
struct Cli {
    /// Path to config.yaml. Falls back to $MONITOR_CONFIG or default search paths.
    #[arg(short, long, env = "MONITOR_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cfg = Config::load(cli.config.as_deref()).context("loading panel configuration")?;

    telemetry::init(&cfg.log);

    tracing::info!(
        version = monitor_common::VERSION,
        http = %cfg.http.listen,
        grpc = %cfg.grpc.listen,
        "starting monitor-panel",
    );

    let pool = db::connect_and_migrate(&cfg.database).await?;
    let app_state = AppState::new(pool).with_cookies_secure(!cfg.http.insecure_cookies);
    if cfg.http.insecure_cookies {
        tracing::warn!(
            "MONITOR_HTTP__INSECURE_COOKIES is set: session cookies will not carry the Secure flag. \
             Acceptable for plain-HTTP deployments, but every request travels in cleartext."
        );
    }

    let shutdown_rx = shutdown::install_handlers();

    // Periodic time-series aggregation + retention pruning.
    metrics::rollup::spawn(app_state.pool.clone(), shutdown_rx.clone());

    // Probe scheduler keeps every connected agent's probe set in sync with
    // the DB; the rollup task aggregates raw probe samples like metrics.
    monitor_panel::probes::Scheduler::new(
        app_state.pool.clone(),
        app_state.hub.clone(),
        app_state.assignment_bus.clone(),
    )
    .spawn(shutdown_rx.clone());
    monitor_panel::probes::rollup::spawn(app_state.pool.clone(), shutdown_rx.clone());

    // GitHub Releases poller — keeps `settings.latest_release` cached so
    // the rollout API responds instantly without hitting the API per-call.
    monitor_panel::updates::poller::spawn(app_state.pool.clone(), shutdown_rx.clone());

    let http = tokio::spawn(http_server::run(
        cfg.http.listen,
        app_state.clone(),
        shutdown_rx.clone(),
    ));
    let grpc = tokio::spawn(grpc::server::run(
        cfg.grpc.listen,
        app_state.clone(),
        shutdown_rx.clone(),
    ));

    let (http_res, grpc_res) = tokio::join!(http, grpc);
    http_res.context("http task panicked")??;
    grpc_res.context("grpc task panicked")??;

    tracing::info!("monitor-panel exited cleanly");
    Ok(())
}
