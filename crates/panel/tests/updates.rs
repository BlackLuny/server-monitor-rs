//! End-to-end coverage for the rollout state machine + assignment
//! generation. The release poller's HTTP path is exercised in lib unit
//! tests; here we focus on the DB-backed orchestration the API depends on.

#![allow(clippy::type_complexity)]

use std::time::Duration;

use monitor_panel::{
    api,
    auth::password,
    state::AppState,
    updates::{
        agent_target_triple, create_rollout, get_rollout, list_rollouts, pause_rollout,
        resume_rollout, rollout::RolloutError, CreateRolloutInput,
    },
};
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

async fn seed_servers(pool: &PgPool, n: usize, os: &str, arch: &str) -> Vec<Uuid> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let agent_id = Uuid::new_v4();
        let token = monitor_common::token::generate();
        sqlx::query(
            r#"INSERT INTO servers
                   (display_name, agent_id, server_token, hw_os, hw_arch)
                   VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(format!("host-{i}"))
        .bind(agent_id)
        .bind(&token)
        .bind(os)
        .bind(arch)
        .execute(pool)
        .await
        .unwrap();
        out.push(agent_id);
    }
    out
}

/// Plant a fully-formed `latest_release` JSON in `settings`. Bypassing the
/// poller keeps these tests offline.
async fn seed_latest_release(pool: &PgPool, version: &str) {
    let value = json!({
        "tag": version,
        "name": version,
        "html_url": null,
        "prerelease": false,
        "published_at": "2026-04-25T00:00:00Z",
        "fetched_at":   "2026-04-25T00:00:01Z",
        "assets": [
            {
                "name": "monitor-agent-x86_64-unknown-linux-musl.tar.gz",
                "url":  "https://example/monitor-agent-x86_64-unknown-linux-musl.tar.gz",
                "size": 1234,
                "sha256": "deadbeef"
            },
            {
                "name": "monitor-agent-aarch64-apple-darwin.tar.gz",
                "url":  "https://example/monitor-agent-aarch64-apple-darwin.tar.gz",
                "size": 1234,
                "sha256": "cafebabe"
            }
        ]
    });
    sqlx::query(
        r#"INSERT INTO settings (key, value) VALUES ('latest_release', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(value)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn create_rollout_with_percent_subsets_pool() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    seed_servers(&pool, 10, "linux", "x86_64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    let rid = create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 30,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    .unwrap();

    let view = get_rollout(&pool, rid).await.unwrap();
    // 30% of 10, ceil → 3.
    assert_eq!(view.summary.assignments_total, 3);
    assert!(view
        .assignments
        .iter()
        .all(|a| a.target == "x86_64-unknown-linux-musl"));
}

#[tokio::test]
async fn create_rollout_skips_unsupported_targets() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    // Mix of supported (linux x86_64) + unsupported (windows arm64).
    seed_servers(&pool, 4, "linux", "x86_64").await;
    seed_servers(&pool, 4, "windows", "aarch64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    let rid = create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 100,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    .unwrap();

    let view = get_rollout(&pool, rid).await.unwrap();
    assert_eq!(view.summary.assignments_total, 4); // only the linux hosts
}

#[tokio::test]
async fn create_rollout_with_explicit_agent_ids() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    let agents = seed_servers(&pool, 6, "linux", "x86_64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    let pick: Vec<Uuid> = agents.iter().take(2).copied().collect();
    let rid = create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 100,
            agent_ids: pick.clone(),
            note: Some("targeted canary".into()),
        },
        Some(admin),
    )
    .await
    .unwrap();
    let view = get_rollout(&pool, rid).await.unwrap();
    assert_eq!(view.summary.assignments_total, pick.len() as i64);
    let got: std::collections::HashSet<Uuid> =
        view.assignments.iter().map(|a| a.agent_id).collect();
    assert_eq!(got, pick.iter().copied().collect());
}

#[tokio::test]
async fn rollout_state_machine_rejects_illegal_transitions() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    seed_servers(&pool, 1, "linux", "x86_64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    let rid = create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 100,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    .unwrap();

    // active → pause OK; double-pause should fail; resume→pause OK again.
    pause_rollout(&pool, rid).await.unwrap();
    match pause_rollout(&pool, rid).await {
        Err(RolloutError::BadTransition { .. }) => {}
        other => panic!("expected BadTransition, got {other:?}"),
    }
    resume_rollout(&pool, rid).await.unwrap();
    pause_rollout(&pool, rid).await.unwrap();

    // Resume from paused; aborting from active is allowed.
    resume_rollout(&pool, rid).await.unwrap();
    monitor_panel::updates::abort_rollout(&pool, rid)
        .await
        .unwrap();
    // After aborting we can't resume.
    match resume_rollout(&pool, rid).await {
        Err(RolloutError::BadTransition { .. }) => {}
        other => panic!("expected BadTransition, got {other:?}"),
    }
}

#[tokio::test]
async fn rollout_requires_cached_release() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    seed_servers(&pool, 1, "linux", "x86_64").await;
    // No seed_latest_release.
    match create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 100,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    {
        Err(RolloutError::NoCachedRelease) => {}
        other => panic!("expected NoCachedRelease, got {other:?}"),
    }
}

#[tokio::test]
async fn rollout_requires_matching_version() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    seed_servers(&pool, 1, "linux", "x86_64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    match create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v9.9.9".into(),
            percent: 100,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    {
        Err(RolloutError::VersionMismatch { .. }) => {}
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn target_triple_helper_unit() {
    assert_eq!(
        agent_target_triple("Linux", "amd64"),
        Some("x86_64-unknown-linux-musl")
    );
    assert_eq!(
        agent_target_triple("darwin", "arm64"),
        Some("aarch64-apple-darwin")
    );
    assert!(agent_target_triple("haiku", "x86_64").is_none());
}

// --------------------------------------------------------------------------
// HTTP-side admin guard smoke test
// --------------------------------------------------------------------------

async fn start(pool: PgPool) -> std::net::SocketAddr {
    let state = AppState::new(pool.clone());
    let router = api::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
}

async fn req_status(addr: std::net::SocketAddr, method: &str, path: &str) -> u16 {
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
    let r = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{addr}"))
        .header("origin", format!("http://{addr}"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    sender.send_request(r).await.unwrap().status().as_u16()
}

#[tokio::test]
async fn admin_guard_on_updates_endpoints() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    seed_admin(&pool).await;
    let addr = start(pool).await;

    assert_eq!(req_status(addr, "GET", "/api/updates/latest").await, 401);
    assert_eq!(req_status(addr, "GET", "/api/updates/rollouts").await, 401);
    assert_eq!(
        req_status(addr, "POST", "/api/updates/rollouts/1/pause").await,
        401
    );
}

#[tokio::test]
async fn list_includes_summary_counts() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let admin = seed_admin(&pool).await;
    seed_servers(&pool, 4, "linux", "x86_64").await;
    seed_latest_release(&pool, "v0.1.0").await;

    create_rollout(
        &pool,
        CreateRolloutInput {
            version: "v0.1.0".into(),
            percent: 100,
            agent_ids: vec![],
            note: None,
        },
        Some(admin),
    )
    .await
    .unwrap();

    let summaries = list_rollouts(&pool).await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].assignments_total, 4);
    assert_eq!(summaries[0].assignments_pending, 4);
    assert_eq!(summaries[0].state, "active");
}

// Reference Value to silence unused import warnings.
#[allow(dead_code)]
fn _unused_value(_v: Value) {}
