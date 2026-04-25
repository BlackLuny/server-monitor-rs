//! Axum extractors that turn the session cookie into a typed principal.
//!
//! - `AuthUser`  — any logged-in user. 401 otherwise.
//! - `AdminUser` — a logged-in user whose role is `admin`. 401/403 otherwise.
//!
//! Handlers declare what they need by adding one of these to their argument
//! list; failure to authenticate never reaches the handler body. The extractor
//! also doubles as the DB-side session touch, so every authenticated request
//! refreshes the sliding window.

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
};
use axum_extra::extract::cookie::CookieJar;

use crate::state::AppState;

use super::session::{self, Session, COOKIE_NAME};

/// Any authenticated user. Handlers take this when they only need to know
/// *who* is calling, not whether they can mutate things.
#[derive(Debug, Clone)]
pub struct AuthUser(pub Session);

/// An admin. Today `role = 'admin'` is the only allowed value (DB CHECK), but
/// this still enforces the invariant at the handler boundary so later roles
/// can be added without a scavenger hunt through the codebase.
#[derive(Debug, Clone)]
pub struct AdminUser(pub Session);

impl AuthUser {
    /// Shared resolution path for both extractors — mirrors
    /// [`axum_extra::extract::cookie::CookieJar`] conventions but returns our
    /// own `Session` type so handlers never see SQL shapes.
    async fn resolve(parts: &mut Parts, state: &AppState) -> Result<Session, StatusCode> {
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(cookie) = jar.get(COOKIE_NAME) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let sid = cookie.value();
        if sid.is_empty() {
            return Err(StatusCode::UNAUTHORIZED);
        }
        match session::validate_and_touch(&state.pool, sid).await {
            Ok(Some(s)) => Ok(s),
            Ok(None) => Err(StatusCode::UNAUTHORIZED),
            Err(err) => {
                tracing::error!(%err, "session lookup failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Self::resolve(parts, state).await.map(AuthUser)
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let s = AuthUser::resolve(parts, state).await?;
        if s.role == "admin" {
            Ok(AdminUser(s))
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}
