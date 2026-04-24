//! monitor-agent — library facade.
//!
//! The binary wires a CLI parser around these modules; keeping the logic in a
//! library lets future integration tests run against the same code.

pub mod collector;
pub mod config;
pub mod hardware;
pub mod register;
pub mod stream;
pub mod transport;
