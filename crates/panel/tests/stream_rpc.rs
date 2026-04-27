//! End-to-end test of the `AgentService.Stream` RPC.
//!
//! Covers: missing/invalid server_token rejection, successful connect →
//! heartbeat → `last_seen_at` update → session registered in the hub, and
//! session cleanup on disconnect.

use std::time::Duration;

use monitor_panel::{
    grpc::{AgentServiceImpl, SessionHub},
    state::AppState,
};
use monitor_proto::{
    v1::{
        agent_service_client::AgentServiceClient, agent_service_server::AgentServiceServer,
        agent_to_panel::Payload as UpPayload, AgentToPanel, HardwareInfo, Heartbeat, Hello,
    },
    SERVER_TOKEN_METADATA,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

fn db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

async fn fresh_pool(db_url: &str) -> PgPool {
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
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

struct Server {
    addr: std::net::SocketAddr,
    hub: SessionHub,
    #[allow(dead_code)]
    pool: PgPool,
}

async fn start_server(pool: PgPool) -> Server {
    let state = AppState::new(pool.clone());
    let hub = state.hub.clone();

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
    Server { addr, hub, pool }
}

/// Insert a server row that is already registered (has agent_id + server_token).
async fn seed_registered(pool: &PgPool, display: &str) -> (uuid::Uuid, String) {
    let agent_id = uuid::Uuid::new_v4();
    let server_token = monitor_common::token::generate();
    sqlx::query("INSERT INTO servers (display_name, agent_id, server_token) VALUES ($1, $2, $3)")
        .bind(display)
        .bind(agent_id)
        .bind(&server_token)
        .execute(pool)
        .await
        .unwrap();
    (agent_id, server_token)
}

#[tokio::test]
async fn rejects_missing_metadata() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let server = start_server(pool).await;

    let mut client = AgentServiceClient::connect(format!("http://{}", server.addr))
        .await
        .unwrap();

    let (_tx, rx) = mpsc::channel::<AgentToPanel>(1);
    let err = client
        .stream(tonic::Request::new(ReceiverStream::new(rx)))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn rejects_bad_token() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let server = start_server(pool).await;

    let mut client = AgentServiceClient::connect(format!("http://{}", server.addr))
        .await
        .unwrap();

    let (_tx, rx) = mpsc::channel::<AgentToPanel>(1);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert(SERVER_TOKEN_METADATA, "bad-token".parse().unwrap());

    let err = client.stream(req).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn heartbeat_updates_last_seen_and_hub() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let (agent_id, token) = seed_registered(&pool, "beta").await;
    let server = start_server(pool.clone()).await;

    let mut client = AgentServiceClient::connect(format!("http://{}", server.addr))
        .await
        .unwrap();

    let (tx, rx) = mpsc::channel::<AgentToPanel>(4);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert(SERVER_TOKEN_METADATA, token.parse().unwrap());

    let response = client.stream(req).await.unwrap();
    let _down: tonic::Streaming<monitor_proto::v1::PanelToAgent> = response.into_inner();

    // Send one heartbeat upstream.
    tx.send(AgentToPanel {
        seq: 1,
        payload: Some(UpPayload::Heartbeat(Heartbeat {
            ts_ms: 0,
            uptime_s: 42,
        })),
    })
    .await
    .unwrap();

    // Wait up to 2s for session to appear in the hub AND last_seen_at to populate.
    let mut registered = false;
    let mut last_seen_populated = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if server.hub.get(&agent_id).is_some() {
            registered = true;
        }
        let seen: Option<time::OffsetDateTime> =
            sqlx::query_scalar("SELECT last_seen_at FROM servers WHERE agent_id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        if seen.is_some() {
            last_seen_populated = true;
        }
        if registered && last_seen_populated {
            break;
        }
    }
    assert!(registered, "session never appeared in hub");
    assert!(last_seen_populated, "last_seen_at was not updated");

    // Drop tx → agent side of stream closes → inbound loop ends → hub cleaned up.
    drop(tx);
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if server.hub.get(&agent_id).is_none() {
            return;
        }
    }
    panic!("session was not removed from hub after agent disconnect");
}

#[tokio::test]
async fn hello_refreshes_hardware_snapshot() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let (agent_id, token) = seed_registered(&pool, "gamma").await;

    // Seed a stale hardware snapshot — this is the state operators end up
    // in today: hw_disk_bytes was right at install time, then they added a
    // disk and the dashboard is now permanently wrong.
    sqlx::query(
        r#"UPDATE servers SET
              hw_cpu_model  = 'old cpu',
              hw_cpu_cores  = 1,
              hw_mem_bytes  = 1024,
              hw_disk_bytes = 21474836480,
              hw_os         = 'linux',
              agent_version = '0.2.0'
            WHERE agent_id = $1"#,
    )
    .bind(agent_id)
    .execute(&pool)
    .await
    .unwrap();

    let server = start_server(pool.clone()).await;
    let mut client = AgentServiceClient::connect(format!("http://{}", server.addr))
        .await
        .unwrap();
    let (tx, rx) = mpsc::channel::<AgentToPanel>(4);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert(SERVER_TOKEN_METADATA, token.parse().unwrap());
    let response = client.stream(req).await.unwrap();
    let _down = response.into_inner();

    tx.send(AgentToPanel {
        seq: 1,
        payload: Some(UpPayload::Hello(Hello {
            hardware: Some(HardwareInfo {
                cpu_model: "shiny new cpu".into(),
                cpu_cores: 8,
                mem_bytes: 16 * 1024 * 1024 * 1024,
                swap_bytes: 0,
                disk_bytes: 500 * 1024 * 1024 * 1024,
                os: "linux".into(),
                os_version: "13".into(),
                kernel: "6.10.0".into(),
                arch: "x86_64".into(),
                virtualization: "kvm".into(),
                boot_id: "boot-xyz".into(),
            }),
            agent_version: "0.2.2".into(),
        })),
    })
    .await
    .unwrap();

    // Poll for the row to reflect the Hello values. Same fixed-budget
    // pattern the heartbeat test uses; the panel processes Hello in an
    // inbound task so the write isn't synchronous with our send.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let row: (String, i32, i64, i64, String) = sqlx::query_as(
            "SELECT hw_cpu_model, hw_cpu_cores, hw_mem_bytes, hw_disk_bytes, agent_version \
               FROM servers WHERE agent_id = $1",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        if row.0 == "shiny new cpu"
            && row.1 == 8
            && row.2 == 16 * 1024 * 1024 * 1024
            && row.3 == 500 * 1024 * 1024 * 1024
            && row.4 == "0.2.2"
        {
            drop(tx);
            return;
        }
    }
    panic!("Hello did not refresh hardware snapshot within budget");
}
