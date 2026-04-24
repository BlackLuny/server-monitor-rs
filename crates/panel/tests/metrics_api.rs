//! Integration tests for GET /api/servers and /api/servers/:id/metrics.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{api, state::AppState};
use monitor_proto::v1::MetricSnapshot;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;

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

async fn start(pool: PgPool) -> (std::net::SocketAddr, PgPool) {
    let state = AppState::new(pool.clone());
    let router = api::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    (addr, pool)
}

async fn seed_server_with_metrics(pool: &PgPool, display: &str, hidden: bool) -> i64 {
    let agent_id = uuid::Uuid::new_v4();
    let server_token = monitor_common::token::generate();
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO servers (display_name, agent_id, server_token, hidden_from_guest,
         hw_os, hw_arch, hw_cpu_model, hw_cpu_cores, hw_mem_bytes, agent_version)
         VALUES ($1, $2, $3, $4, 'Linux', 'x86_64', 'Xeon', 4, 8000000000, '0.1.0')
         RETURNING id",
    )
    .bind(display)
    .bind(agent_id)
    .bind(&server_token)
    .bind(hidden)
    .fetch_one(pool)
    .await
    .unwrap();

    // Mark the server as recently seen so `online` turns true.
    sqlx::query("UPDATE servers SET last_seen_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();

    // Seed a handful of raw metrics over the last minute.
    let now_ms: i64 = sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM NOW()) * 1000)::bigint")
        .fetch_one(pool)
        .await
        .unwrap();
    let snapshots: Vec<MetricSnapshot> = (0i64..5)
        .map(|i| MetricSnapshot {
            ts_ms: now_ms - (4 - i) * 5_000,
            cpu_pct: 10.0 + (i as f64) * 5.0,
            cpu_pct_per_core: vec![],
            mem_used: 1_000_000_000u64 + (i as u64) * 100_000_000,
            mem_total: 8_000_000_000,
            swap_used: 0,
            swap_total: 0,
            load_1: 0.5,
            load_5: 0.6,
            load_15: 0.7,
            disk_used: 10_000_000_000,
            disk_total: 100_000_000_000,
            disks: vec![],
            net_in_bps: 1024 * (i + 1) as u64,
            net_out_bps: 2048,
            net_in_total: 0,
            net_out_total: 0,
            nets: vec![],
            process_count: 100,
            tcp_conn: 10,
            udp_conn: 3,
            temperature_c: 55.0,
            gpu_pct: -1.0,
        })
        .collect();
    monitor_panel::metrics::ingest_batch(pool, id, &snapshots)
        .await
        .unwrap();
    id
}

async fn http_get_json(addr: std::net::SocketAddr, path: &str) -> Value {
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpStream;

    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(async move { conn.await.ok() });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("{addr}"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = sender.send_request(req).await.unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn list_returns_servers_and_latest_metric() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    let id = seed_server_with_metrics(&pool, "alpha", false).await;

    let body = http_get_json(addr, "/api/servers").await;
    let servers = body["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1);
    let s = &servers[0];
    assert_eq!(s["id"].as_i64().unwrap(), id);
    assert_eq!(s["display_name"], "alpha");
    assert_eq!(s["online"], true);
    assert_eq!(s["hardware"]["cpu_model"], "Xeon");
    let latest = &s["latest"];
    assert!(!latest.is_null());
    let cpu = latest["cpu_pct"].as_f64().unwrap();
    // Latest (i=4) should be 10 + 4*5 = 30.
    assert!((cpu - 30.0).abs() < 0.01, "got {cpu}");
}

#[tokio::test]
async fn list_hides_sensitive_fields_in_guest_mode() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    seed_server_with_metrics(&pool, "public", false).await;
    seed_server_with_metrics(&pool, "private", true).await;

    // Non-guest sees both.
    let all = http_get_json(addr, "/api/servers").await;
    assert_eq!(all["servers"].as_array().unwrap().len(), 2);
    // Hardware visible.
    assert!(!all["servers"][0]["hardware"].is_null());

    // Guest sees only the public one.
    let guest = http_get_json(addr, "/api/servers?guest=true").await;
    let list = guest["servers"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["display_name"], "public");
    assert!(
        list[0]["hardware"].is_null(),
        "hardware must be stripped for guests"
    );
}

#[tokio::test]
async fn metrics_endpoint_returns_points() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    let id = seed_server_with_metrics(&pool, "alpha", false).await;

    let body = http_get_json(addr, &format!("/api/servers/{id}/metrics?range=1h")).await;
    assert_eq!(body["granularity"], "raw");
    assert_eq!(body["range"], "1h");
    let points = body["points"].as_array().unwrap();
    assert_eq!(points.len(), 5);
    // Points must be ascending by timestamp.
    for w in points.windows(2) {
        assert!(w[0]["ts"].as_str().unwrap() <= w[1]["ts"].as_str().unwrap());
    }
}
