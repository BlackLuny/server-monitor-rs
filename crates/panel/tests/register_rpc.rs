// Integration test — pragmatic tuple types in queries outweigh the clippy
// complaint about type complexity.
#![allow(clippy::type_complexity)]

//! End-to-end test of the `AgentService.Register` RPC.
//!
//! Requires a Postgres reachable via `$TEST_DATABASE_URL`; each test runs in
//! its own transient schema so they may run in parallel. The test is skipped
//! when the env var is absent so `cargo test` on a bare machine stays green.

use std::time::Duration;

use monitor_panel::{grpc::AgentServiceImpl, state::AppState};
use monitor_proto::v1::{
    agent_service_client::AgentServiceClient, agent_service_server::AgentServiceServer,
    HardwareInfo, RegisterRequest,
};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;

fn db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

async fn fresh_pool(db_url: &str) -> sqlx::PgPool {
    let schema = format!("test_{}", uuid::Uuid::new_v4().simple());
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect({
            let schema = schema.clone();
            move |conn, _meta| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query(&format!("SET search_path TO {schema}"))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            }
        })
        .connect(db_url)
        .await
        .expect("connect test DB");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations apply");
    pool
}

async fn start_server(pool: sqlx::PgPool) -> std::net::SocketAddr {
    let state = AppState::new(pool);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServiceServer::new(AgentServiceImpl::new(state)))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
}

async fn seed_pending(pool: &sqlx::PgPool, display: &str) -> String {
    let token = monitor_common::token::generate();
    sqlx::query("INSERT INTO servers (display_name, join_token) VALUES ($1, $2)")
        .bind(display)
        .bind(&token)
        .execute(pool)
        .await
        .unwrap();
    token
}

#[tokio::test]
async fn rejects_empty_token() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let addr = start_server(pool).await;

    let mut client = AgentServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let err = client
        .register(RegisterRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn rejects_unknown_token() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let addr = start_server(pool).await;

    let mut client = AgentServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let err = client
        .register(RegisterRequest {
            join_token: "never-issued".into(),
            hostname: "host".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn succeeds_once_then_rejects_replay() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let token = seed_pending(&pool, "alpha").await;
    let addr = start_server(pool.clone()).await;

    let mut client = AgentServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let resp = client
        .register(RegisterRequest {
            join_token: token.clone(),
            hostname: "alpha.example.com".into(),
            agent_version: "0.1.0".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            hardware: Some(HardwareInfo {
                cpu_model: "Intel Xeon".into(),
                cpu_cores: 8,
                mem_bytes: 16 * 1024 * 1024 * 1024,
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(uuid::Uuid::parse_str(&resp.agent_id).is_ok());
    assert_eq!(resp.server_token.len(), 43);

    let (agent_id, server_token, join_token, hw_cpu_model, hw_cpu_cores, agent_version): (
        Option<uuid::Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT agent_id, server_token, join_token, hw_cpu_model, hw_cpu_cores, agent_version
         FROM servers WHERE display_name = 'alpha'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(agent_id.unwrap().to_string(), resp.agent_id);
    assert_eq!(server_token.unwrap(), resp.server_token);
    assert!(join_token.is_none(), "join_token must be cleared");
    assert_eq!(hw_cpu_model.as_deref(), Some("Intel Xeon"));
    assert_eq!(hw_cpu_cores, Some(8));
    assert_eq!(agent_version.as_deref(), Some("0.1.0"));

    let err = client
        .register(RegisterRequest {
            join_token: token,
            hostname: "alpha.example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}
