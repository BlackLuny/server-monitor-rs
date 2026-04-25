//! Integration tests for the admin-gated `POST /api/servers` endpoint.
//!
//! Each case seeds a single admin via `password::hash`, logs in over real
//! HTTP to capture the session cookie, then calls the create endpoint with
//! that cookie attached. This is the same shape the SvelteKit shell uses,
//! and it lets us exercise the auth + CSRF middleware on the same path.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{api, auth::password, state::AppState};
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

async fn start(pool: PgPool) -> std::net::SocketAddr {
    let state = AppState::new(pool);
    let router = api::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
}

async fn seed_admin(pool: &PgPool, username: &str, plaintext: &str) {
    let hash = password::hash(plaintext).unwrap();
    sqlx::query("INSERT INTO users (username, password_hash, role) VALUES ($1, $2, 'admin')")
        .bind(username)
        .bind(&hash)
        .execute(pool)
        .await
        .unwrap();
}

struct HttpResponse {
    status: u16,
    set_cookie: Option<String>,
    body: Value,
}

async fn http_req(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> HttpResponse {
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
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let raw = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap()
    };
    HttpResponse {
        status,
        set_cookie,
        body,
    }
}

fn extract_cookie(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap_or("").trim().to_owned()
}

async fn login(addr: std::net::SocketAddr, username: &str, password: &str) -> String {
    let r = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": username, "password": password})),
        None,
    )
    .await;
    assert_eq!(r.status, 200);
    extract_cookie(&r.set_cookie.expect("cookie"))
}

#[tokio::test]
async fn create_requires_admin_session() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    let addr = start(pool).await;

    let r = http_req(
        addr,
        "POST",
        "/api/servers",
        Some(json!({"display_name": "alpha"})),
        None,
    )
    .await;
    assert_eq!(r.status, 401, "no cookie → unauthorized");
}

#[tokio::test]
async fn create_fails_when_endpoint_not_configured() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    let addr = start(pool).await;
    let cookie = login(addr, "root", "correct horse battery staple").await;

    let r = http_req(
        addr,
        "POST",
        "/api/servers",
        Some(json!({"display_name": "alpha"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.body["code"], "agent_endpoint_not_configured");
}

#[tokio::test]
async fn create_rejects_empty_display_name() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'agent_endpoint'")
        .bind(Value::String("https://panel.example.com".into()))
        .execute(&pool)
        .await
        .unwrap();
    let addr = start(pool).await;
    let cookie = login(addr, "root", "correct horse battery staple").await;

    let r = http_req(
        addr,
        "POST",
        "/api/servers",
        Some(json!({"display_name": "   "})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.body["code"], "display_name_required");
}

#[tokio::test]
async fn create_succeeds_and_returns_install_command() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'agent_endpoint'")
        .bind(Value::String("https://panel.example.com/grpc".into()))
        .execute(&pool)
        .await
        .unwrap();
    let addr = start(pool.clone()).await;
    let cookie = login(addr, "root", "correct horse battery staple").await;

    let r = http_req(
        addr,
        "POST",
        "/api/servers",
        Some(json!({"display_name": "alpha"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, 201, "body: {}", r.body);

    let agent_id = r.body["agent_id"].as_str().unwrap();
    let join_token = r.body["join_token"].as_str().unwrap();
    let install = r.body["install_command"].as_str().unwrap();
    assert!(uuid::Uuid::parse_str(agent_id).is_ok());
    assert_eq!(join_token.len(), 43);
    assert!(install.contains("https://panel.example.com/grpc"));
    assert!(install.contains(join_token));

    let (display, stored_token, server_token): (String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT display_name, join_token, server_token FROM servers WHERE agent_id = $1",
        )
        .bind(uuid::Uuid::parse_str(agent_id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(display, "alpha");
    assert_eq!(stored_token.as_deref(), Some(join_token));
    assert!(server_token.is_none());
}

#[tokio::test]
async fn delete_requires_admin_and_audits() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'agent_endpoint'")
        .bind(Value::String("https://panel.example.com".into()))
        .execute(&pool)
        .await
        .unwrap();
    let addr = start(pool.clone()).await;
    let cookie = login(addr, "root", "correct horse battery staple").await;

    let create = http_req(
        addr,
        "POST",
        "/api/servers",
        Some(json!({"display_name": "doomed"})),
        Some(&cookie),
    )
    .await;
    let id = create.body["id"].as_i64().unwrap();

    // Without cookie: 401
    let unauth = http_req(addr, "DELETE", &format!("/api/servers/{id}"), None, None).await;
    assert_eq!(unauth.status, 401);

    // With admin cookie: 204 + row gone
    let ok = http_req(
        addr,
        "DELETE",
        &format!("/api/servers/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(ok.status, 204);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}
