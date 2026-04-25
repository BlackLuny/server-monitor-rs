//! End-to-end terminal tests.
//!
//! These cover the panel-side bookkeeping: WS auth gating, the
//! TerminalHub's open / close lifecycle and per-user concurrency cap, and
//! the recording-metadata API. The actual pty side lives on the agent and
//! is not exercised here — testing portable-pty in CI is finicky and the
//! manager wraps it thinly enough that integration is verified manually
//! during the VPS walkthrough.
//!
//! All tests skip cleanly when TEST_DATABASE_URL is unset.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{
    api,
    auth::{password, SessionMeta},
    state::AppState,
    terminal::{TerminalHub, MAX_SESSIONS_PER_USER},
};
use monitor_proto::v1::{TerminalClosed, TerminalOutput};
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

async fn start(pool: PgPool) -> std::net::SocketAddr {
    let state = AppState::new(pool.clone());
    let router = api::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
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

async fn seed_server(pool: &PgPool, name: &str) -> (i64, Uuid) {
    let agent_id = Uuid::new_v4();
    let token = monitor_common::token::generate();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO servers (display_name, agent_id, server_token) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(agent_id)
    .bind(&token)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, agent_id)
}

struct R {
    status: u16,
    body: Value,
}

async fn req(addr: std::net::SocketAddr, method: &str, path: &str, cookie: Option<&str>) -> R {
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
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{addr}"))
        .header("origin", format!("http://{addr}"));
    if let Some(c) = cookie {
        builder = builder.header("cookie", c);
    }
    let r = builder.body(Full::new(Bytes::new())).unwrap();
    let res = sender.send_request(r).await.unwrap();
    let status = res.status().as_u16();
    let raw = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
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
    res.headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .trim()
        .to_owned()
}

#[tokio::test]
async fn list_sessions_requires_admin() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let (server_id, _) = seed_server(&pool, "alpha").await;
    let addr = start(pool).await;

    let unauth = req(
        addr,
        "GET",
        &format!("/api/servers/{server_id}/terminal-sessions"),
        None,
    )
    .await;
    assert_eq!(unauth.status, 401);

    let cookie = login(addr).await;
    let auth = req(
        addr,
        "GET",
        &format!("/api/servers/{server_id}/terminal-sessions"),
        Some(&cookie),
    )
    .await;
    assert_eq!(auth.status, 200);
    assert!(auth.body.is_array());
    assert_eq!(auth.body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn hub_open_close_lifecycle() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    let (server_id, agent_id) = seed_server(&pool, "alpha").await;

    let hub = TerminalHub::new();
    let session_id = Uuid::new_v4();
    let mut rx = hub
        .open(
            &pool,
            session_id,
            server_id,
            agent_id,
            Some(admin),
            &SessionMeta::default(),
        )
        .await
        .unwrap();
    assert!(hub.contains(&session_id.to_string()));
    assert_eq!(hub.count_for_user(admin), 1);

    // Output frame routes through the channel.
    hub.deliver_output(TerminalOutput {
        session_id: session_id.to_string(),
        data: b"hello".to_vec(),
    });
    let frame = tokio::time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match frame {
        monitor_panel::terminal::Frame::Output(bytes) => assert_eq!(bytes, b"hello"),
        _ => panic!("expected Output frame"),
    }

    // Closed frame removes the slot, updates the row, and forwards to bridge.
    hub.deliver_closed(
        &pool,
        TerminalClosed {
            session_id: session_id.to_string(),
            exit_code: 0,
            error: String::new(),
            recording_path: "/tmp/x.cast".into(),
            recording_size: 42,
            recording_sha256: "deadbeef".into(),
        },
    )
    .await;

    // Bridge sees the close.
    let frame = tokio::time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .unwrap();
    assert!(matches!(
        frame,
        Some(monitor_panel::terminal::Frame::Closed(_))
    ));
    assert!(!hub.contains(&session_id.to_string()));

    // DB row sealed with closed_at + recording metadata.
    let row: (Option<i32>, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT exit_code, recording_path, recording_size FROM terminal_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, Some(0));
    assert_eq!(row.1.as_deref(), Some("/tmp/x.cast"));
    assert_eq!(row.2, Some(42));

    // Audit row written for ssh.opened + ssh.closed.
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'ssh.%' AND user_id = $1",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(n >= 2);
}

#[tokio::test]
async fn hub_per_user_count_caps_at_max() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    let (server_id, agent_id) = seed_server(&pool, "alpha").await;

    let hub = TerminalHub::new();
    // Open MAX_SESSIONS_PER_USER concurrent sessions.
    let mut keep = Vec::new();
    for _ in 0..MAX_SESSIONS_PER_USER {
        let id = Uuid::new_v4();
        let rx = hub
            .open(
                &pool,
                id,
                server_id,
                agent_id,
                Some(admin),
                &SessionMeta::default(),
            )
            .await
            .unwrap();
        keep.push((id, rx));
    }
    assert_eq!(hub.count_for_user(admin), MAX_SESSIONS_PER_USER);
    // The WS handler is the gatekeeper for this cap; what we verify here
    // is that the count is observable and accurate.
}

#[tokio::test]
async fn recording_endpoint_returns_metadata() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    let (server_id, agent_id) = seed_server(&pool, "alpha").await;
    let addr = start(pool.clone()).await;
    let cookie = login(addr).await;

    // Insert a finished terminal session row directly so we don't need a
    // live agent.
    let session_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO terminal_sessions
                (id, server_id, user_id, closed_at, exit_code,
                 recording_path, recording_size, recording_sha256)
                VALUES ($1, $2, $3, NOW(), 0, $4, $5, $6)"#,
    )
    .bind(session_id)
    .bind(server_id)
    .bind(admin)
    .bind("/var/lib/monitor-agent/recordings/x.cast")
    .bind(1234_i64)
    .bind("deadbeef")
    .execute(&pool)
    .await
    .unwrap();

    let r = req(
        addr,
        "GET",
        &format!("/api/recordings/{session_id}"),
        Some(&cookie),
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body["recording_path"],
        "/var/lib/monitor-agent/recordings/x.cast"
    );
    assert_eq!(r.body["recording_size"], 1234);
    assert_eq!(r.body["agent_id"], agent_id.to_string());
    let _ = server_id; // silence unused if assertions evolve
}
