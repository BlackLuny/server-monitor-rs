//! HTTP API surface.
//!
//! Currently exposes:
//! - `GET /healthz` — liveness probe (cheap, no DB access)
//! - `GET /api/version` — build version for the UI's footer + update checks
//! - `GET /*` — embedded SvelteKit SPA (with SPA fallback for unknown paths)
//!
//! Admin/auth routes land under `/api/...` in tasks #3 (auth) and #9 (servers).

pub mod metrics;
mod servers;
mod static_files;
mod ws;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, Router},
    Json,
};
use serde::Serialize;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::state::AppState;

/// Build the top-level router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/version", get(version))
        .route("/api/servers", get(servers::list).post(servers::create))
        .route("/api/servers/:id/metrics", get(metrics::server_metrics))
        .route("/ws/live", get(ws::handler))
        .fallback(static_files::handler)
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        // Permissive CORS is fine for now because the same origin serves the UI.
        // Tighten to the configured domain when we add explicit proxy deployments.
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Response {
    // Health check must fail when the DB is unreachable — otherwise container
    // orchestrators keep routing traffic to a panel that can't persist anything.
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(err) => {
            tracing::warn!(%err, "healthz: database check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "db unavailable").into_response()
        }
    }
}

#[derive(Serialize)]
struct VersionInfo {
    version: &'static str,
    name: &'static str,
}

async fn version(State(_): State<AppState>) -> Json<VersionInfo> {
    Json(VersionInfo {
        version: monitor_common::VERSION,
        name: "monitor-panel",
    })
}
