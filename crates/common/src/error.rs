//! Shared error type used across the common crate.

use thiserror::Error;

/// Errors surfaced by helper functions in `monitor-common`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid agent endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("password hashing failed: {0}")]
    PasswordHash(String),

    #[error("url parse error: {0}")]
    Url(#[from] url::ParseError),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
