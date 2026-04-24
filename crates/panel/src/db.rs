//! Postgres connection pool and migration bootstrap.

use std::time::Duration;

use anyhow::Context;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::DatabaseConfig;

/// Initialize the connection pool and apply pending migrations.
///
/// Migrations are embedded at compile time from `crates/panel/migrations/`.
pub async fn connect_and_migrate(cfg: &DatabaseConfig) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&cfg.url)
        .await
        .context("connecting to Postgres")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    tracing::info!(
        pool_size = cfg.max_connections,
        "database connected and migrations applied",
    );
    Ok(pool)
}
