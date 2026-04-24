//! First-time Register handshake.

use monitor_proto::v1::{agent_service_client::AgentServiceClient, RegisterRequest};
use tonic::transport::Channel;

use crate::{config::AgentConfig, hardware};

/// Outcome of a successful Register call — the values the agent must persist.
#[derive(Debug)]
pub struct Registered {
    pub agent_id: String,
    pub server_token: String,
}

/// Run the Register RPC using the join_token from `cfg`. Returns the new
/// credentials on success; leaves the config untouched so the caller can
/// decide where to persist them.
pub async fn register(channel: Channel, cfg: &AgentConfig) -> anyhow::Result<Registered> {
    let Some(join_token) = cfg.join_token.as_deref() else {
        anyhow::bail!("register: missing join_token");
    };

    let hw = hardware::collect();
    let host = hostname::get()
        .ok()
        .and_then(|n| n.into_string().ok())
        .unwrap_or_default();

    let mut client = AgentServiceClient::new(channel);
    let resp = client
        .register(RegisterRequest {
            join_token: join_token.to_owned(),
            hostname: host,
            hardware: Some(hw),
            agent_version: monitor_common::VERSION.to_owned(),
            os: hardware::os_id().to_string(),
            arch: std::env::consts::ARCH.to_string(),
        })
        .await?
        .into_inner();

    Ok(Registered {
        agent_id: resp.agent_id,
        server_token: resp.server_token,
    })
}
