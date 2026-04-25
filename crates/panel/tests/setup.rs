//! Integration tests for the first-run setup wizard.
//!
//! Each test runs against its own Postgres schema so the `users` table is
//! guaranteed empty at the start — that's the only environment where setup
//! is supposed to be permitted.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{api, state::AppState};
use serde_json::{json, Value};
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

async fn http_json(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Option<String>, Value) {
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
        // Mimic a same-origin browser fetch so the CSRF middleware passes.
        .header("origin", format!("http://{addr}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let req = builder.body(body_bytes).unwrap();
    let res = sender.send_request(req).await.unwrap();
    let status = res.status().as_u16();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, set_cookie, json)
}

#[tokio::test]
async fn status_before_setup_is_uninitialized() {
    let Some(db) = db_url() else { return };
    let (addr, _pool) = start(fresh_pool(&db).await).await;

    let (code, _, body) = http_json(addr, "GET", "/api/setup/status", None).await;
    assert_eq!(code, 200);
    assert_eq!(body["initialized"], false);
}

#[tokio::test]
async fn setup_creates_first_admin_and_sets_cookie() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;

    let (code, set_cookie, body) = http_json(
        addr,
        "POST",
        "/api/setup",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
    )
    .await;

    assert_eq!(code, 201, "body: {body}");
    assert_eq!(body["username"], "root");
    assert_eq!(body["role"], "admin");
    // Cookie with our name must be present — the exact value is opaque.
    let cookie = set_cookie.expect("setup must set a session cookie");
    assert!(cookie.starts_with("monitor_session="), "got {cookie}");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));

    // DB state: one admin row, one audit row, one live session.
    let n_users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n_users, 1);
    let n_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM login_sessions WHERE revoked_at IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n_sessions, 1);
    let audit: (String,) = sqlx::query_as("SELECT action FROM audit_log ORDER BY ts DESC LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audit.0, "setup.admin_created");

    // Status flips to initialized.
    let (_, _, status) = http_json(addr, "GET", "/api/setup/status", None).await;
    assert_eq!(status["initialized"], true);
}

#[tokio::test]
async fn setup_rejects_when_already_initialized() {
    let Some(db) = db_url() else { return };
    let (addr, _) = start(fresh_pool(&db).await).await;

    let first = http_json(
        addr,
        "POST",
        "/api/setup",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
    )
    .await;
    assert_eq!(first.0, 201);

    let second = http_json(
        addr,
        "POST",
        "/api/setup",
        Some(json!({"username": "intruder", "password": "another long password"})),
    )
    .await;
    assert_eq!(second.0, 403);
    assert_eq!(second.2["code"], "already_initialized");
}

#[tokio::test]
async fn setup_rejects_short_password() {
    let Some(db) = db_url() else { return };
    let (addr, _) = start(fresh_pool(&db).await).await;

    let (code, _, body) = http_json(
        addr,
        "POST",
        "/api/setup",
        Some(json!({"username": "root", "password": "short"})),
    )
    .await;
    assert_eq!(code, 400);
    assert_eq!(body["code"], "password_too_short");
}

#[tokio::test]
async fn setup_rejects_empty_username() {
    let Some(db) = db_url() else { return };
    let (addr, _) = start(fresh_pool(&db).await).await;

    let (code, _, body) = http_json(
        addr,
        "POST",
        "/api/setup",
        Some(json!({"username": "   ", "password": "correct horse battery staple"})),
    )
    .await;
    assert_eq!(code, 400);
    assert_eq!(body["code"], "username_required");
}
