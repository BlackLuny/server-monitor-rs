//! tonic Channel construction from an [`AgentEndpoint`].
//!
//! - `http://…` → plaintext HTTP/2 (h2c).
//! - `https://…` → TLS using the system native roots.

use std::time::Duration;

use monitor_common::{AgentEndpoint, EndpointScheme};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

/// Build a tonic [`Channel`] that dials the given endpoint.
///
/// TLS verification is on by default when the scheme is `https`.
pub fn build_channel(endpoint: &AgentEndpoint) -> anyhow::Result<Channel> {
    let uri = endpoint
        .url
        .parse::<tonic::transport::Uri>()
        .map_err(|e| anyhow::anyhow!("bad endpoint {:?}: {e}", endpoint.url))?;

    let mut builder: Endpoint = Endpoint::from(uri)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .http2_keep_alive_interval(Duration::from_secs(20))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .user_agent(format!("monitor-agent/{}", monitor_common::VERSION))?;

    if matches!(endpoint.scheme, EndpointScheme::Https) {
        let tls = ClientTlsConfig::new()
            .domain_name(endpoint.host.clone())
            .with_webpki_roots();
        builder = builder.tls_config(tls)?;
    }

    // Lazy connect: returns an unconnected Channel that will try on first use.
    // We want the agent to keep retrying on Panel outage, not fail at startup.
    Ok(builder.connect_lazy())
}
