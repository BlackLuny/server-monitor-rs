//! HTTP server lifecycle — binds the listener, wires the Axum router to it,
//! and awaits graceful shutdown.

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{api, shutdown::ShutdownRx, state::AppState};

/// Run the HTTP server until `shutdown` fires.
pub async fn run(
    addr: SocketAddr,
    state: AppState,
    mut shutdown: ShutdownRx,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding http listener on {addr}"))?;
    let bound = listener.local_addr()?;
    tracing::info!(addr = %bound, "http server listening");

    let router = api::router(state);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.changed().await.ok();
        })
        .await
        .context("http server error")?;

    tracing::info!("http server stopped");
    Ok(())
}
