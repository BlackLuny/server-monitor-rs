//! Embedded SvelteKit SPA assets.
//!
//! Assets are read from `frontend/build/` relative to the workspace root at
//! compile time. The build step that populates that directory runs ahead of
//! `cargo build` in release pipelines; during development the same directory
//! holds the placeholder produced before the real frontend exists.
//!
//! Behavior mirrors a classic SPA host:
//! - Exact-path asset match → serve the asset with the correct MIME type.
//! - Unknown path → serve `index.html` so client-side routing works.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../frontend/build/"]
#[exclude = "*.map"]
struct Assets;

/// Axum fallback handler that serves embedded assets or the SPA index.
pub async fn handler(request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if let Some(resp) = respond_with_asset(path) {
        return resp;
    }
    // SPA fallback: serve the index for any unknown path so client-side routers
    // can own the navigation. API routes are matched above `fallback`, so this
    // only fires for non-API paths.
    respond_with_asset("index.html").unwrap_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "frontend assets not embedded",
        )
            .into_response()
    })
}

fn respond_with_asset(path: &str) -> Option<Response> {
    let lookup_path = if path.is_empty() { "index.html" } else { path };
    let asset = Assets::get(lookup_path)?;

    let mime = mime_guess::from_path(lookup_path).first_or_octet_stream();
    let mut response = Response::new(Body::from(asset.data.into_owned()));
    if let Ok(value) = HeaderValue::from_str(mime.as_ref()) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    // Cache long for fingerprinted assets, short for index.html itself.
    let cache_control = if lookup_path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    Some(response)
}

/// Used by the HTTP server smoke test and by the API module's 404 handler.
#[allow(dead_code)]
pub fn asset_exists(uri: &Uri) -> bool {
    let path = uri.path().trim_start_matches('/');
    Assets::get(if path.is_empty() { "index.html" } else { path }).is_some()
}
