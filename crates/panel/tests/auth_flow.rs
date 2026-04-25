//! End-to-end tests for login / logout / me.
//!
//! Each test spins up a fresh schema, seeds an admin (so we don't have to run
//! the full /setup wizard), and drives the auth flow through real HTTP.

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

async fn seed_admin(pool: &PgPool, username: &str, plaintext: &str) -> i64 {
    let hash = password::hash(plaintext).unwrap();
    sqlx::query_scalar(
        r#"INSERT INTO users (username, password_hash, role)
           VALUES ($1, $2, 'admin') RETURNING id"#,
    )
    .bind(username)
    .bind(&hash)
    .fetch_one(pool)
    .await
    .unwrap()
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
    // "monitor_session=<v>; HttpOnly; ..." → "monitor_session=<v>"
    set_cookie.split(';').next().unwrap_or("").trim().to_owned()
}

#[tokio::test]
async fn login_success_sets_cookie_and_returns_profile() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    let id = seed_admin(&pool, "root", "correct horse battery staple").await;

    let r = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;

    assert_eq!(r.status, 200, "body: {}", r.body);
    assert_eq!(r.body["user_id"], id);
    assert_eq!(r.body["username"], "root");
    assert_eq!(r.body["role"], "admin");
    assert_eq!(r.body["totp_enabled"], false);

    let cookie = r.set_cookie.expect("session cookie must be set");
    assert!(cookie.starts_with("monitor_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));

    // One audit row of `auth.login.success`.
    let (action, audited_id): (String, Option<i64>) =
        sqlx::query_as("SELECT action, user_id FROM audit_log ORDER BY ts DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(action, "auth.login.success");
    assert_eq!(audited_id, Some(id));
}

#[tokio::test]
async fn login_wrong_password_returns_401_and_audits_failure() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;

    let r = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "wrong"})),
        None,
    )
    .await;

    assert_eq!(r.status, 401);
    assert_eq!(r.body["code"], "invalid_credentials");
    assert!(r.set_cookie.is_none(), "no cookie on failure");

    let (action,): (String,) =
        sqlx::query_as("SELECT action FROM audit_log ORDER BY ts DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(action, "auth.login.failure");
}

#[tokio::test]
async fn login_unknown_user_returns_401_same_shape() {
    let Some(db) = db_url() else { return };
    let (addr, _) = start(fresh_pool(&db).await).await;

    let r = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "ghost", "password": "anything"})),
        None,
    )
    .await;

    assert_eq!(r.status, 401);
    assert_eq!(r.body["code"], "invalid_credentials");
}

#[tokio::test]
async fn me_requires_session_cookie() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;

    let unauth = http_req(addr, "GET", "/api/auth/me", None, None).await;
    assert_eq!(unauth.status, 401);

    let login = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;
    let cookie = extract_cookie(&login.set_cookie.unwrap());

    let me = http_req(addr, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(me.status, 200);
    assert_eq!(me.body["username"], "root");
    assert_eq!(me.body["role"], "admin");
    assert_eq!(me.body["totp_enabled"], false);
}

#[tokio::test]
async fn logout_revokes_session_and_clears_cookie() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;

    let login = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;
    let cookie = extract_cookie(&login.set_cookie.unwrap());

    let out = http_req(addr, "POST", "/api/auth/logout", None, Some(&cookie)).await;
    assert_eq!(out.status, 204);
    let clear = out.set_cookie.expect("logout must emit clearing cookie");
    assert!(clear.contains("monitor_session="));
    assert!(clear.contains("Max-Age=0"), "got: {clear}");

    // Next /me with the now-revoked cookie must 401.
    let me = http_req(addr, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(me.status, 401);

    // And the row is marked revoked.
    let revoked: (Option<time::OffsetDateTime>,) =
        sqlx::query_as("SELECT revoked_at FROM login_sessions LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(revoked.0.is_some(), "revoked_at must be populated");
}

#[tokio::test]
async fn totp_enabled_account_requires_second_factor() {
    let Some(db) = db_url() else { return };
    let (addr, pool) = start(fresh_pool(&db).await).await;
    let id = seed_admin(&pool, "root", "correct horse battery staple").await;
    sqlx::query("UPDATE users SET totp_enabled = TRUE WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    let r = http_req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;
    assert_eq!(r.status, 401);
    assert_eq!(r.body["code"], "totp_required");
    assert!(r.set_cookie.is_none());
}
