//! Session cookie construction.
//!
//! All session cookies share the same baseline: `HttpOnly` (no JS access),
//! `SameSite=Strict` (cross-site requests drop the cookie, which kills CSRF
//! on its own), and `Path=/`. `Secure` is opt-in via the caller because the
//! correct value depends on whether the panel is reachable over HTTPS.
//!
//! Plain-HTTP deployments (typical for an internal box without a TLS
//! terminator) must pass `secure=false` — modern browsers silently refuse
//! to store cookies marked `Secure` on `http://` origins, and a login that
//! looks successful would actually drop the session before the next
//! request.

use axum_extra::extract::cookie::{Cookie, SameSite};
use time::Duration;

use super::session::{COOKIE_NAME, SESSION_TTL_DAYS};

/// Build a login cookie carrying the given session id. Expiry matches the
/// server-side sliding window; the browser will forget the cookie at the
/// *absolute* TTL even if it's still valid server-side. That's acceptable —
/// the user will simply see a login prompt one second sooner.
#[must_use]
pub fn build<'a>(session_id: String, secure: bool) -> Cookie<'a> {
    let mut c = Cookie::new(COOKIE_NAME, session_id);
    c.set_http_only(true);
    c.set_same_site(SameSite::Strict);
    c.set_secure(secure);
    c.set_path("/");
    c.set_max_age(Duration::days(SESSION_TTL_DAYS));
    c
}

/// Build an empty, immediately-expired cookie to clear the session on logout.
#[must_use]
pub fn clear<'a>(secure: bool) -> Cookie<'a> {
    let mut c = Cookie::new(COOKIE_NAME, "");
    c.set_http_only(true);
    c.set_same_site(SameSite::Strict);
    c.set_secure(secure);
    c.set_path("/");
    c.set_max_age(Duration::seconds(0));
    c
}
