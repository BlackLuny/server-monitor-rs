//! Parsing and classification of the panel URL that agents connect to.
//!
//! Admins configure this in the panel settings (see `settings.agent_endpoint`)
//! as a full URL:
//!
//! - `http://panel.example.com:9090` — plaintext gRPC (h2c)
//! - `https://panel.example.com` — TLS, default port 443, root path
//! - `https://panel.example.com/grpc` — TLS via Caddy reverse proxy
//!   (`/grpc/*` is h2c-forwarded to panel)
//!
//! The agent feeds the parsed [`AgentEndpoint`] into its tonic `Channel` setup.

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

/// Which transport the agent should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointScheme {
    /// Plain HTTP/2 (no TLS). Suitable for loopback / trusted LAN.
    Http,
    /// HTTP/2 over TLS.
    Https,
}

impl EndpointScheme {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    /// Default TCP port for this scheme.
    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// Parsed representation of the panel endpoint an agent should dial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEndpoint {
    /// Transport scheme (http or https).
    pub scheme: EndpointScheme,
    /// Authority host (e.g. `panel.example.com`).
    pub host: String,
    /// TCP port. Always present (filled from scheme default when omitted).
    pub port: u16,
    /// Path prefix including leading slash; `/` when no prefix was provided.
    /// Used when the panel is exposed behind a reverse proxy that routes a
    /// sub-path to the gRPC backend.
    pub path: String,
    /// Canonical URL string (`scheme://host[:port]path`).
    pub url: String,
}

impl AgentEndpoint {
    /// Parse a user-provided URL.
    ///
    /// # Errors
    /// Returns [`Error::InvalidEndpoint`] when the input cannot be parsed, uses
    /// an unsupported scheme, or is missing a host.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(Error::InvalidEndpoint("empty".into()));
        }
        let parsed = Url::parse(trimmed)?;

        let scheme = match parsed.scheme() {
            "http" => EndpointScheme::Http,
            "https" => EndpointScheme::Https,
            other => {
                return Err(Error::InvalidEndpoint(format!(
                    "unsupported scheme: {other}"
                )));
            }
        };

        let host = parsed
            .host_str()
            .ok_or_else(|| Error::InvalidEndpoint("missing host".into()))?
            .to_owned();

        let port = parsed.port().unwrap_or_else(|| scheme.default_port());

        // Normalize the path: `""` → `/`, strip trailing slash unless it is the root.
        let mut path = parsed.path().to_owned();
        if path.is_empty() {
            path.push('/');
        } else if path.len() > 1 && path.ends_with('/') {
            path.pop();
        }

        let url = if parsed.port().is_some() {
            format!("{}://{}:{}{}", scheme.as_str(), host, port, path)
        } else {
            format!("{}://{}{}", scheme.as_str(), host, path)
        };

        Ok(Self {
            scheme,
            host,
            port,
            path,
            url,
        })
    }

    /// True when the endpoint expects TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.scheme, EndpointScheme::Https)
    }

    /// `true` when a non-root path prefix was provided (indicating a proxy path).
    #[must_use]
    pub fn has_path_prefix(&self) -> bool {
        self.path != "/"
    }
}

impl fmt::Display for AgentEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_with_port() {
        let ep = AgentEndpoint::parse("http://panel:9090").unwrap();
        assert_eq!(ep.scheme, EndpointScheme::Http);
        assert_eq!(ep.host, "panel");
        assert_eq!(ep.port, 9090);
        assert_eq!(ep.path, "/");
        assert!(!ep.is_tls());
        assert!(!ep.has_path_prefix());
    }

    #[test]
    fn parses_https_default_port() {
        let ep = AgentEndpoint::parse("https://panel.example.com").unwrap();
        assert_eq!(ep.port, 443);
        assert!(ep.is_tls());
        assert_eq!(ep.path, "/");
    }

    #[test]
    fn parses_https_with_path_prefix() {
        let ep = AgentEndpoint::parse("https://panel.example.com/grpc").unwrap();
        assert_eq!(ep.path, "/grpc");
        assert!(ep.has_path_prefix());
    }

    #[test]
    fn trims_trailing_slash() {
        let ep = AgentEndpoint::parse("https://panel.example.com/grpc/").unwrap();
        assert_eq!(ep.path, "/grpc");
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            AgentEndpoint::parse(""),
            Err(Error::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert!(matches!(
            AgentEndpoint::parse("ftp://panel"),
            Err(Error::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn rejects_missing_host() {
        assert!(AgentEndpoint::parse("http://").is_err());
    }
}
