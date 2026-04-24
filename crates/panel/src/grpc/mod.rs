//! gRPC surface exposed to agents.
//!
//! - `Register` — one-shot token exchange (task #7).
//! - `Stream` — long-lived bidirectional channel carrying metrics, probe
//!   results, terminal IO, and update coordination (task #8).

mod agent_service;
pub mod server;
pub mod session;

pub use agent_service::AgentServiceImpl;
pub use session::{AgentSession, SessionHub};
