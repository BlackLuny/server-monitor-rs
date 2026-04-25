//! Origin-check CSRF middleware.
//!
//! `SameSite=Strict` on the session cookie already prevents the classic
//! cross-site form-POST attack in every currently-shipping browser. This
//! middleware is defense-in-depth for mutating requests: reject anything whose
//! `Origin` (or fallback `Referer`) header doesn't match the request's own
//! `Host`. If neither header is present on a state-changing method, the
//! request is rejected — no modern browser omits both on same-origin
//! fetch/XHR.
//!
//! Safe methods (`GET`, `HEAD`, `OPTIONS`) are passed through untouched.

use axum::{
    extract::Request,
    http::{header, Method, StatusCode},
    middleware::Next,
    response::Response,
};

pub async fn require_same_origin(req: Request, next: Next) -> Result<Response, StatusCode> {
    let method = req.method();
    if matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS) {
        return Ok(next.run(req).await);
    }

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    // Prefer Origin (exact match on scheme+host+port). Fall back to Referer's
    // authority component when Origin is absent — some older clients only send
    // the latter, and fetch() in a classic same-origin context always sends
    // one or the other.
    let origin_authority = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(authority_of)
        .or_else(|| {
            req.headers()
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .and_then(authority_of)
        });

    let Some(authority) = origin_authority else {
        return Err(StatusCode::FORBIDDEN);
    };

    if authority != host {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(req).await)
}

/// Pull the `host[:port]` substring out of an absolute URL. We stay string-level
/// to avoid pulling in a URL parser for a two-line comparison.
fn authority_of(raw: &str) -> Option<String> {
    let without_scheme = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    let end = without_scheme
        .find(['/', '?', '#'])
        .unwrap_or(without_scheme.len());
    let authority = &without_scheme[..end];
    if authority.is_empty() {
        None
    } else {
        Some(authority.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::authority_of;

    #[test]
    fn authority_extraction() {
        assert_eq!(
            authority_of("https://panel.example.com/foo"),
            Some("panel.example.com".to_owned())
        );
        assert_eq!(
            authority_of("http://localhost:8080/"),
            Some("localhost:8080".to_owned())
        );
        assert_eq!(
            authority_of("https://example.com"),
            Some("example.com".to_owned())
        );
        assert_eq!(authority_of(""), None);
    }
}
