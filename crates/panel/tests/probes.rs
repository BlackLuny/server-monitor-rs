//! End-to-end probe tests.
//!
//! Covers:
//!   - admin guard on POST/PATCH/DELETE
//!   - effective state matrix changes when an override flips
//!   - ProbeBatch results land in probe_results
//!   - rollup raw → m1 averages success rate / latency
//!   - the scheduler computes the right per-agent set

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{api, auth::password, probes, state::AppState};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use uuid::Uuid;

fn db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

async fn fresh_pool(db_url: &str) -> PgPool {
    let schema = format!("test_{}", Uuid::new_v4().simple());
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
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(30)).await;
    (addr, pool)
}

async fn seed_admin(pool: &PgPool) -> i64 {
    let hash = password::hash("hunter2-admin").unwrap();
    sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, role) VALUES ('root', $1, 'admin') RETURNING id",
    )
    .bind(&hash)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn seed_server(pool: &PgPool, name: &str) -> Uuid {
    let agent_id = Uuid::new_v4();
    let token = monitor_common::token::generate();
    sqlx::query("INSERT INTO servers (display_name, agent_id, server_token) VALUES ($1, $2, $3)")
        .bind(name)
        .bind(agent_id)
        .bind(&token)
        .execute(pool)
        .await
        .unwrap();
    agent_id
}

struct R {
    status: u16,
    body: Value,
}

async fn req(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> R {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpStream;

    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(async move { conn.await.ok() });

    let body_bytes = match &body {
        Some(v) => Full::new(Bytes::from(v.to_string())),
        None => Full::new(Bytes::new()),
    };
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{addr}"))
        .header("origin", format!("http://{addr}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(c) = cookie {
        builder = builder.header("cookie", c);
    }
    let req = builder.body(body_bytes).unwrap();
    let res = sender.send_request(req).await.unwrap();
    let status = res.status().as_u16();
    let raw = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap()
    };
    R { status, body }
}

async fn login(addr: std::net::SocketAddr) -> String {
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpStream;

    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .unwrap();
    tokio::spawn(async move { conn.await.ok() });
    let body = json!({"username": "root", "password": "hunter2-admin"});
    let req = hyper::Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("host", format!("{addr}"))
        .header("origin", format!("http://{addr}"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let res = sender.send_request(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .trim()
        .to_owned();
    cookie
}

#[tokio::test]
async fn create_requires_admin_session() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let (addr, _) = start(pool).await;

    let unauth = req(
        addr,
        "POST",
        "/api/probes",
        Some(json!({"name":"X","kind":"icmp","target":"1.1.1.1"})),
        None,
    )
    .await;
    assert_eq!(unauth.status, 401);

    let cookie = login(addr).await;
    let auth = req(
        addr,
        "POST",
        "/api/probes",
        Some(json!({"name":"Cloudflare","kind":"icmp","target":"1.1.1.1"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(auth.status, 201);
    assert_eq!(auth.body["kind"], "icmp");
    assert_eq!(auth.body["default_enabled"], true);
}

#[tokio::test]
async fn validation_rejects_bad_input() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let (addr, _) = start(pool).await;
    let cookie = login(addr).await;

    let no_port = req(
        addr,
        "POST",
        "/api/probes",
        Some(json!({"name":"x","kind":"tcp","target":"1.2.3.4"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(no_port.status, 400);
    assert_eq!(no_port.body["code"], "port_required");

    let bad_url = req(
        addr,
        "POST",
        "/api/probes",
        Some(json!({"name":"x","kind":"http","target":"example.com"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(bad_url.status, 400);
    assert_eq!(bad_url.body["code"], "invalid_url");
}

#[tokio::test]
async fn override_flip_changes_effective_matrix() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let agent1 = seed_server(&pool, "alpha").await;
    let agent2 = seed_server(&pool, "beta").await;
    let (addr, _) = start(pool.clone()).await;
    let cookie = login(addr).await;

    let create = req(
        addr,
        "POST",
        "/api/probes",
        Some(json!({"name":"DNS","kind":"icmp","target":"1.1.1.1"})),
        Some(&cookie),
    )
    .await;
    let pid = create.body["id"].as_i64().unwrap();

    // Default-on → both agents effectively enabled.
    let m0 = req(
        addr,
        "GET",
        &format!("/api/probes/{pid}/agents"),
        None,
        Some(&cookie),
    )
    .await;
    let arr = m0.body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|r| r["effective_enabled"] == true));

    // Disable the probe just for `agent1`.
    let flip = req(
        addr,
        "PUT",
        &format!("/api/probes/{pid}/agents/{agent1}"),
        Some(json!({"enabled": false})),
        Some(&cookie),
    )
    .await;
    assert_eq!(flip.status, 204);

    let m1 = req(
        addr,
        "GET",
        &format!("/api/probes/{pid}/agents"),
        None,
        Some(&cookie),
    )
    .await;
    let arr = m1.body.as_array().unwrap();
    let row1 = arr
        .iter()
        .find(|r| r["agent_id"] == agent1.to_string())
        .unwrap();
    let row2 = arr
        .iter()
        .find(|r| r["agent_id"] == agent2.to_string())
        .unwrap();
    assert_eq!(row1["effective_enabled"], false);
    assert_eq!(row1["override_enabled"], false);
    assert_eq!(row2["effective_enabled"], true);
    assert_eq!(row2["override_enabled"], Value::Null);

    // Setting same-as-default clears the override row.
    let clear = req(
        addr,
        "PUT",
        &format!("/api/probes/{pid}/agents/{agent1}"),
        Some(json!({"enabled": true})),
        Some(&cookie),
    )
    .await;
    assert_eq!(clear.status, 204);
    let n_overrides: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM probe_agent_overrides WHERE probe_id = $1")
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n_overrides, 0);
}

#[tokio::test]
async fn ingest_probe_batch_persists_rows() {
    use monitor_proto::v1::ProbeResult;

    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let agent = seed_server(&pool, "alpha").await;

    // Create a probe to attach results to.
    let pid: i64 = sqlx::query_scalar(
        "INSERT INTO probes (name, kind, target) VALUES ('t', 'icmp', '1.1.1.1') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let now_ms: i64 = sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM NOW()) * 1000)::bigint")
        .fetch_one(&pool)
        .await
        .unwrap();
    let results = vec![
        ProbeResult {
            probe_id: pid.to_string(),
            ts_ms: now_ms - 2_000,
            ok: true,
            latency_us: 12_345,
            status_code: 0,
            error: String::new(),
        },
        ProbeResult {
            probe_id: pid.to_string(),
            ts_ms: now_ms - 1_000,
            ok: false,
            latency_us: 0,
            status_code: 0,
            error: "timeout".into(),
        },
        // Duplicate ts → should be deduped by ON CONFLICT.
        ProbeResult {
            probe_id: pid.to_string(),
            ts_ms: now_ms - 1_000,
            ok: true,
            latency_us: 999,
            status_code: 0,
            error: String::new(),
        },
    ];
    let inserted = probes::ingest_batch(&pool, agent, &results).await.unwrap();
    // Postgres `ON CONFLICT DO NOTHING` returns rows_affected = inserts only.
    assert_eq!(inserted, 2);

    let rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM probe_results WHERE probe_id = $1 AND granularity='raw'",
    )
    .bind(pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rows, 2);
}
