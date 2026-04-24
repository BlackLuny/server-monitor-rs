//! tracing-subscriber initialization.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::{LogConfig, LogFormat};

/// Install a global tracing subscriber. Call once from `main`.
pub fn init(cfg: &LogConfig) {
    let env_filter = EnvFilter::try_new(&cfg.filter).unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    match cfg.format {
        LogFormat::Text => {
            registry.with(fmt::layer().with_target(true)).init();
        }
        LogFormat::Json => {
            registry.with(fmt::layer().json().with_target(true)).init();
        }
    }
}
