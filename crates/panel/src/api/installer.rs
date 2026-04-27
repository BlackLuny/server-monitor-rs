//! Serves the embedded `install-agent.sh` / `install-agent.ps1` so the
//! "Add server" command from the panel UI is actually fetchable via curl /
//! PowerShell. Both routes are unauthenticated by design: the join token in
//! the install command is the auth boundary, and admins must be able to run
//! the one-liner from a fresh host that has no panel cookie.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

const INSTALL_SH: &[u8] = include_bytes!("../../../../deploy/install-agent.sh");
const INSTALL_PS1: &[u8] = include_bytes!("../../../../deploy/install-agent.ps1");

pub async fn install_agent_sh() -> Response {
    script_response(INSTALL_SH, "text/x-shellscript; charset=utf-8")
}

pub async fn install_agent_ps1() -> Response {
    script_response(INSTALL_PS1, "text/plain; charset=utf-8")
}

fn script_response(body: &'static [u8], content_type: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}
