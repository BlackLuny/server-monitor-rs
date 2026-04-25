//! monitor-panel — library facade.
//!
//! The binary in `src/main.rs` is a thin wiring layer; everything else is
//! exposed here so integration tests and xtask commands can reuse it.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod grpc;
pub mod http_server;
pub mod live;
pub mod metrics;
pub mod probes;
pub mod settings;
pub mod shutdown;
pub mod state;
pub mod telemetry;
pub mod terminal;
