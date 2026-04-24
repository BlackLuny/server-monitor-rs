//! Signal-driven graceful shutdown primitive.

use tokio::signal;
use tokio::sync::watch;

/// Receiver end of the shutdown channel. Clone freely and `.changed().await`
/// inside each task that needs to participate in graceful shutdown.
pub type ShutdownRx = watch::Receiver<bool>;

/// Spawn a task that waits for SIGINT / SIGTERM and flips the returned
/// [`ShutdownRx`] to `true` exactly once.
pub fn install_handlers() -> ShutdownRx {
    let (tx, rx) = watch::channel(false);

    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received, notifying tasks");
        let _ = tx.send(true);
    });

    rx
}

#[cfg(unix)]
async fn wait_for_signal() {
    use signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM"),
        _ = int.recv() => tracing::info!("SIGINT"),
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() {
    let _ = signal::ctrl_c().await;
    tracing::info!("Ctrl-C");
}
