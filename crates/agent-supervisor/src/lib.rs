//! monitor-agent-supervisor — library facade.
//!
//! The binary in `src/main.rs` is the long-running entry point; everything
//! else lives here so integration tests + downstream tools can reuse it.

pub mod ipc;
pub mod staging;
