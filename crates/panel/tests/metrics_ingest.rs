//! Integration tests for metric ingestion via the Stream RPC.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{grpc::AgentServiceImpl, state::AppState};
use monitor_proto::{
    v1::{
        agent_service_client::AgentServiceClient, agent_service_server::AgentServiceServer,
        agent_to_panel::Payload as UpPayload, AgentToPanel, DiskUsage, MetricBatch, MetricSnapshot,
        NetUsage,
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

async fn start_server(pool: PgPool) -> std::net::SocketAddr {
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

async fn seed_registered(pool: &PgPool, display: &str) -> (i64, uuid::Uuid, String) {
    let agent_id = uuid::Uuid::new_v4();
    let server_token = monitor_common::token::generate();
    let (row_id,): (i64,) = sqlx::query_as(
        "INSERT INTO servers (display_name, agent_id, server_token) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(display)
    .bind(agent_id)
    .bind(&server_token)
    .fetch_one(pool)
    .await
    .unwrap();
    (row_id, agent_id, server_token)
}

fn make_snapshot(ts_ms: i64) -> MetricSnapshot {
    MetricSnapshot {
        ts_ms,
        cpu_pct: 42.5,
        cpu_pct_per_core: vec![10.0, 20.0, 30.0, 40.0],
        mem_used: 4_000_000_000,
        mem_total: 16_000_000_000,
        swap_used: 100,
        swap_total: 2_000_000_000,
        load_1: 0.5,
        load_5: 0.7,
        load_15: 1.0,
        disk_used: 50_000_000_000,
        disk_total: 500_000_000_000,
        disks: vec![DiskUsage {
            mount: "/".into(),
            fstype: "ext4".into(),
            used: 50_000_000_000,
            total: 500_000_000_000,
            read_bps: 0,
            write_bps: 0,
        }],
        net_in_bps: 1024,
        net_out_bps: 2048,
        net_in_total: 10240,
        net_out_total: 20480,
        nets: vec![NetUsage {
            name: "eth0".into(),
            rx_bps: 1024,
            tx_bps: 2048,
            rx_total: 10240,
            tx_total: 20480,
        }],
        process_count: 120,
        tcp_conn: 15,
        udp_conn: 3,
        temperature_c: 55.0,
        gpu_pct: -1.0,
    }
}

#[tokio::test]
async fn batch_ingest_persists_rows() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let (server_row_id, _agent_id, token) = seed_registered(&pool, "gamma").await;
    let addr = start_server(pool.clone()).await;

    let mut client = AgentServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap();
    let (tx, rx) = mpsc::channel::<AgentToPanel>(4);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert(SERVER_TOKEN_METADATA, token.parse().unwrap());
    let _resp = client.stream(req).await.unwrap();

    // First batch: 3 samples at distinct timestamps.
    let base = 1_700_000_000_000i64; // fixed reference ms
    tx.send(AgentToPanel {
        seq: 1,
        payload: Some(UpPayload::MetricsBatch(MetricBatch {
            snapshots: vec![
                make_snapshot(base),
                make_snapshot(base + 1000),
                make_snapshot(base + 2000),
            ],
        })),
    })
    .await
    .unwrap();

    // Second batch: 2 more new samples + 1 duplicate of the first batch.
    tx.send(AgentToPanel {
        seq: 2,
        payload: Some(UpPayload::MetricsBatch(MetricBatch {
            snapshots: vec![
                make_snapshot(base + 2000), // duplicate ts — should be ignored
                make_snapshot(base + 3000),
                make_snapshot(base + 4000),
            ],
        })),
    })
    .await
    .unwrap();

    // Poll until all 5 distinct rows are present.
    let mut count: i64 = 0;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metric_snapshots WHERE server_id = $1 AND granularity = 'raw'",
        )
        .bind(server_row_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        if count >= 5 {
            break;
        }
    }
    assert_eq!(count, 5, "expected 5 distinct rows (duplicate ts deduped)");

    // Spot-check one row's shape.
    let (cpu_pct, mem_used, cpu_per_core, disks): (f64, i64, serde_json::Value, serde_json::Value) =
        sqlx::query_as(
            "SELECT cpu_pct, mem_used, cpu_per_core, disks FROM metric_snapshots \
             WHERE server_id = $1 AND granularity = 'raw' ORDER BY ts LIMIT 1",
        )
        .bind(server_row_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!((cpu_pct - 42.5).abs() < 0.001);
    assert_eq!(mem_used, 4_000_000_000);
    assert_eq!(cpu_per_core.as_array().unwrap().len(), 4);
    assert_eq!(disks.as_array().unwrap().len(), 1);
    assert_eq!(disks[0]["mount"], "/");
}
