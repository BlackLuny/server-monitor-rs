//! HTTP API surface.
//!
//! Top-level wiring. Each module owns its handler signatures and emits the
//! same `(axum::response::Response)` contract; the router here just binds
//! verbs to paths.

mod agents;
mod auth;
mod groups;
mod installer;
pub mod metrics;
mod probes;
mod servers;
mod settings;
mod setup;
mod static_files;
mod terminal_ws;
mod terminals;
mod totp;
mod updates;
mod users;
mod ws;

pub use metrics::server_metrics;

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put, Router},
    Json,
};
use serde::Serialize;
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::{auth as auth_mod, state::AppState};

/// Build the top-level router.
pub fn router(state: AppState) -> Router {
    let audit_limit: axum::routing::MethodRouter<AppState> = get(settings::list_audit);

    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/version", get(version))
        // ---- agent installer (unauthenticated; join token is the auth) ----
        .route("/install-agent.sh", get(installer::install_agent_sh))
        .route("/install-agent.ps1", get(installer::install_agent_ps1))
        // ---- first-run wizard ----
        .route("/api/setup/status", get(setup::status))
        .route("/api/setup", post(setup::create))
        // ---- sessions ----
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/me", get(auth::me))
        // ---- 2FA ----
        .route("/api/auth/totp/enroll", post(totp::enroll))
        .route("/api/auth/totp/confirm", post(totp::confirm))
        .route("/api/auth/totp/disable", post(totp::disable))
        .route("/api/auth/totp/regenerate-backup", post(totp::regenerate_backup))
        // ---- servers ----
        .route("/api/servers", get(servers::list).post(servers::create))
        .route(
            "/api/servers/:id",
            patch(servers::update).delete(servers::delete_one),
        )
        .route("/api/servers/sparklines", get(metrics::server_sparklines))
        .route("/api/servers/:id/metrics", get(metrics::server_metrics))
        .route("/api/servers/:id/install", get(servers::install_info))
        .route(
            "/api/servers/:id/install/rotate",
            post(servers::rotate_install),
        )
        // ---- groups ----
        .route("/api/groups", get(groups::list).post(groups::create))
        .route(
            "/api/groups/:id",
            patch(groups::update).delete(groups::delete_one),
        )
        // ---- agents (lightweight list for UI selectors) ----
        .route("/api/agents", get(agents::list))
        // ---- probes ----
        .route("/api/probes", get(probes::list).post(probes::create))
        .route(
            "/api/probes/:id",
            patch(probes::update).delete(probes::delete_one),
        )
        .route("/api/probes/:id/results", get(probes::results))
        .route(
            "/api/probes/:id/agents",
            get(probes::list_agents_for_probe),
        )
        .route(
            "/api/probes/:id/agents/:agent_id",
            put(probes::set_override),
        )
        // ---- users (admin only) ----
        .route("/api/users", get(users::list).post(users::create))
        .route("/api/users/:id", delete(users::delete_one))
        .route(
            "/api/users/:id/password",
            put(users::reset_password),
        )
        .route(
            "/api/auth/password",
            put(users::change_own_password),
        )
        // ---- settings ----
        .route("/api/settings", get(settings::list))
        .route("/api/settings/:key", put(settings::put_one))
        // ---- audit log ----
        .route("/api/audit", audit_limit)
        // ---- terminal sessions (admin only) ----
        .route(
            "/api/servers/:id/terminal-sessions",
            get(terminals::list_for_server),
        )
        .route("/api/recordings/:session_id", get(terminals::recording))
        .route(
            "/api/recordings/:session_id/download",
            get(terminals::download_recording),
        )
        // ---- self-update orchestration (admin only) ----
        .route("/api/updates/latest", get(updates::latest))
        .route("/api/updates/recent", get(updates::recent))
        .route(
            "/api/updates/rollouts",
            get(updates::list).post(updates::create),
        )
        .route("/api/updates/rollouts/:id", get(updates::detail))
        .route("/api/updates/rollouts/:id/pause", post(updates::pause))
        .route("/api/updates/rollouts/:id/resume", post(updates::resume))
        .route("/api/updates/rollouts/:id/abort", post(updates::abort))
        // ---- websocket ----
        .route("/ws/live", get(ws::handler))
        .route("/ws/terminal/:server_id", get(terminal_ws::handler))
        .fallback(static_files::handler)
        // CSRF guard on every mutating method via Origin-check middleware.
        .layer(middleware::from_fn(auth_mod::csrf::require_same_origin))
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Response {
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
