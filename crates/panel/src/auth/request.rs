//! Helpers for turning raw request metadata into things the auth layer
//! cares about (client IP, user-agent).
//!
//! Behind Caddy / nginx the connection peer is always the proxy, so we
//! prefer the forwarded headers. The intent is best-effort audit logging,
//! not enforcement — a caller who spoofs `X-Forwarded-For` only lies about
//! themselves in the audit trail, they don't gain any authority.

use axum::http::{header, HeaderMap};

use super::SessionMeta;

/// Extract audit metadata from request headers. Falls back to `None` fields
/// when the originating data isn't present.
#[must_use]
pub fn session_meta(headers: &HeaderMap) -> SessionMeta {
    SessionMeta {
        ip: client_ip(headers),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned),
    }
}

fn client_ip(headers: &HeaderMap) -> Option<String> {
    // Canonical Caddy + nginx header. Only the first entry is the real client;
    // anything after is a proxy hop. We trim spaces because the header spec
    // allows whitespace after the comma.
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        let first = xff.split(',').next().unwrap_or("").trim();
        if !first.is_empty() {
            return Some(first.to_owned());
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn prefers_first_xff_entry() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        assert_eq!(client_ip(&h).as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn falls_back_to_x_real_ip() {
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", HeaderValue::from_static("10.0.0.1"));
        assert_eq!(client_ip(&h).as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn returns_none_when_missing() {
        let h = HeaderMap::new();
        assert!(client_ip(&h).is_none());
    }
}
