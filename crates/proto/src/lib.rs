//! Protobuf definitions and tonic-generated code for server-monitor-rs.
//!
//! `agent.proto` defines the single RPC surface between the panel and agents:
//!
//! - [`v1::agent_service_client`] (feature `client`) — used by the agent.
//! - [`v1::agent_service_server`] (feature `server`) — used by the panel.
//!
//! Both features are enabled by default. Downstream crates should opt out of
//! whichever side they do not need to avoid pulling the extra generated code.

#![allow(
    clippy::doc_markdown,
    clippy::large_enum_variant,
    clippy::derive_partial_eq_without_eq
)]

/// Monitor v1 — generated from `proto/monitor/v1/agent.proto`.
pub mod v1 {
    tonic::include_proto!("monitor.v1");
}

/// gRPC metadata header name the agent uses to present its long-lived token
/// on the [`v1::agent_service_client::AgentServiceClient::stream`] RPC.
pub const SERVER_TOKEN_METADATA: &str = "x-server-token";
