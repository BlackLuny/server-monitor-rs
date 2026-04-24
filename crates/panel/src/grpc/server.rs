//! gRPC server lifecycle.

use std::net::SocketAddr;

use anyhow::Context;
use monitor_proto::v1::agent_service_server::AgentServiceServer;
use tonic::transport::Server;

use crate::{shutdown::ShutdownRx, state::AppState};

use super::AgentServiceImpl;

/// Run the tonic gRPC server until `shutdown` fires.
pub async fn run(
    addr: SocketAddr,
    state: AppState,
    mut shutdown: ShutdownRx,
) -> anyhow::Result<()> {
    let service = AgentServiceImpl::new(state);

    tracing::info!(%addr, "grpc server listening");

    Server::builder()
        .add_service(AgentServiceServer::new(service))
        .serve_with_shutdown(addr, async move {
            shutdown.changed().await.ok();
        })
        .await
        .with_context(|| format!("grpc server on {addr}"))?;

    tracing::info!("grpc server stopped");
    Ok(())
}
