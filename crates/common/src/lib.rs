//! Shared types, utilities, and configuration helpers for server-monitor-rs.
//!
//! This crate is deliberately small. Anything that cannot be shared between the
//! panel and agent (database access, HTTP routing, pty handling, …) lives in the
//! respective binary crate instead.

pub mod endpoint;
pub mod error;
pub mod password;
pub mod token;
pub mod version;

pub use endpoint::{AgentEndpoint, EndpointScheme};
pub use error::{Error, Result};
pub use version::VERSION;
