//! End-to-end TOTP enrollment, login, backup-code and disable.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{api, auth::password, state::AppState};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use totp_rs::{Algorithm, Secret, TOTP};

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

async fn seed_admin(pool: &PgPool, username: &str, plaintext: &str) -> i64 {
    let hash = password::hash(plaintext).unwrap();
    sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, role) VALUES ($1, $2, 'admin') RETURNING id",
    )
    .bind(username)
    .bind(&hash)
    .fetch_one(pool)
    .await
    .unwrap()
}

struct R {
    status: u16,
    set_cookie: Option<String>,
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
    R {
        status,
        set_cookie,
        body,
    }
}

fn ck(s: &str) -> String {
    s.split(';').next().unwrap_or("").trim().to_owned()
}

async fn login_basic(addr: std::net::SocketAddr, u: &str, p: &str) -> String {
    let r = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": u, "password": p})),
        None,
    )
    .await;
    assert_eq!(r.status, 200);
    ck(&r.set_cookie.unwrap())
}

fn current_code(secret_b32: &str) -> String {
    let bytes = Secret::Encoded(secret_b32.to_owned()).to_bytes().unwrap();
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        Some("server-monitor".into()),
        "x".into(),
    )
    .unwrap();
    totp.generate_current().unwrap()
}

#[tokio::test]
async fn enroll_confirm_login_with_totp() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool, "root", "correct horse battery staple").await;
    let addr = start(pool.clone()).await;
    let cookie = login_basic(addr, "root", "correct horse battery staple").await;

    // Enroll
    let enroll = req(addr, "POST", "/api/auth/totp/enroll", None, Some(&cookie)).await;
    assert_eq!(enroll.status, 200);
    let secret = enroll.body["secret"].as_str().unwrap().to_owned();
    assert!(enroll.body["otpauth_url"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://"));
    assert!(enroll.body["qr_svg_data_url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/svg+xml;base64,"));

    // Confirm with a valid current code
    let code = current_code(&secret);
    let confirm = req(
        addr,
        "POST",
        "/api/auth/totp/confirm",
        Some(json!({"code": code})),
        Some(&cookie),
    )
    .await;
    assert_eq!(confirm.status, 200);
    let codes = confirm.body["backup_codes"].as_array().unwrap().clone();
    assert_eq!(codes.len(), 10);
    let backup_code = codes[0].as_str().unwrap().to_owned();

    // /me reflects totp_enabled
    let me = req(addr, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(me.body["totp_enabled"], true);

    // New login WITHOUT code → totp_required
    let r1 = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;
    assert_eq!(r1.status, 401);
    assert_eq!(r1.body["code"], "totp_required");

    // New login WITH valid TOTP code
    let code2 = current_code(&secret);
    let r2 = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({
            "username": "root",
            "password": "correct horse battery staple",
            "totp_code": code2
        })),
        None,
    )
    .await;
    assert_eq!(r2.status, 200);
    assert!(r2.set_cookie.is_some());

    // New login with backup code
    let r3 = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({
            "username": "root",
            "password": "correct horse battery staple",
            "totp_code": backup_code
        })),
        None,
    )
    .await;
    assert_eq!(r3.status, 200);

    // Reusing the same backup code must fail
    let r4 = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({
            "username": "root",
            "password": "correct horse battery staple",
            "totp_code": backup_code
        })),
        None,
    )
    .await;
    assert_eq!(r4.status, 401);
    assert_eq!(r4.body["code"], "invalid_totp");

    // Disable requires the password
    let bad = req(
        addr,
        "POST",
        "/api/auth/totp/disable",
        Some(json!({"password": "wrong"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(bad.status, 401);

    let good = req(
        addr,
        "POST",
        "/api/auth/totp/disable",
        Some(json!({"password": "correct horse battery staple"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(good.status, 204);

    // After disable, login no longer requires code
    let r5 = req(
        addr,
        "POST",
        "/api/auth/login",
        Some(json!({"username": "root", "password": "correct horse battery staple"})),
        None,
    )
    .await;
    assert_eq!(r5.status, 200);
}
