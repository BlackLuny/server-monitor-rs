//! Integration tests for the dev-only `POST /api/servers` endpoint.

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

#[tokio::test]
async fn create_fails_when_endpoint_not_configured() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    let addr = start(pool).await;

    let client = reqwest_stub::Client::new();
    let resp = client
        .post(&format!("http://{addr}/api/servers"))
        .json(&json!({ "display_name": "alpha" }))
        .send()
        .await;
    assert_eq!(resp.status, 400);
    let body: Value = resp.json();
    assert_eq!(body["code"], "agent_endpoint_not_configured");
}

#[tokio::test]
async fn create_rejects_empty_display_name() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    // Configure endpoint so we don't short-circuit on that error.
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'agent_endpoint'")
        .bind(Value::String("https://panel.example.com".into()))
        .execute(&pool)
        .await
        .unwrap();
    let addr = start(pool).await;

    let client = reqwest_stub::Client::new();
    let resp = client
        .post(&format!("http://{addr}/api/servers"))
        .json(&json!({ "display_name": "   " }))
        .send()
        .await;
    assert_eq!(resp.status, 400);
    let body: Value = resp.json();
    assert_eq!(body["code"], "display_name_required");
}

#[tokio::test]
async fn create_succeeds_and_returns_install_command() {
    let Some(db) = db_url() else { return };
    let pool = fresh_pool(&db).await;
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'agent_endpoint'")
        .bind(Value::String("https://panel.example.com/grpc".into()))
        .execute(&pool)
        .await
        .unwrap();
    let addr = start(pool.clone()).await;

    let client = reqwest_stub::Client::new();
    let resp = client
        .post(&format!("http://{addr}/api/servers"))
        .json(&json!({ "display_name": "alpha" }))
        .send()
        .await;
    assert_eq!(resp.status, 201);
    let body: Value = resp.json();
    let agent_id = body["agent_id"].as_str().unwrap();
    let join_token = body["join_token"].as_str().unwrap();
    let install = body["install_command"].as_str().unwrap();

    assert!(uuid::Uuid::parse_str(agent_id).is_ok());
    assert_eq!(join_token.len(), 43);
    assert!(install.contains("https://panel.example.com/grpc"));
    assert!(install.contains(join_token));
    assert!(install.contains("--token="));

    // Row shape sanity.
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

/// Tiny blocking-friendly HTTP client wrapper to avoid pulling `reqwest` just
/// for these tests. Sits directly on top of `hyper` + `hyper_util`.
mod reqwest_stub {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use serde::de::DeserializeOwned;
    use tokio::net::TcpStream;

    pub struct Client;
    impl Client {
        pub fn new() -> Self {
            Self
        }
        pub fn post(&self, url: &str) -> RequestBuilder {
            RequestBuilder {
                url: url.to_owned(),
                body: None,
            }
        }
    }

    pub struct RequestBuilder {
        url: String,
        body: Option<Vec<u8>>,
    }

    impl RequestBuilder {
        pub fn json<T: serde::Serialize>(mut self, v: &T) -> Self {
            self.body = Some(serde_json::to_vec(v).unwrap());
            self
        }
        pub async fn send(self) -> Response {
            let uri: hyper::Uri = self.url.parse().unwrap();
            let host = uri.host().unwrap();
            let port = uri.port_u16().unwrap_or(80);
            let stream = TcpStream::connect((host, port)).await.unwrap();
            let (mut sender, conn) =
                hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
                    .await
                    .unwrap();
            tokio::spawn(async move {
                let _ = conn.await;
            });

            let body = self.body.unwrap_or_default();
            let req = hyper::Request::builder()
                .method("POST")
                .uri(uri.path())
                .header("host", format!("{host}:{port}"))
                .header("content-type", "application/json")
                .header("content-length", body.len())
                .body(Full::new(Bytes::from(body)))
                .unwrap();
            let res = sender.send_request(req).await.unwrap();
            let (parts, body) = res.into_parts();
            let body_bytes = body.collect().await.unwrap().to_bytes().to_vec();
            Response {
                status: parts.status.as_u16(),
                body: body_bytes,
            }
        }
    }

    pub struct Response {
        pub status: u16,
        pub body: Vec<u8>,
    }
    impl Response {
        pub fn json<T: DeserializeOwned>(&self) -> T {
            serde_json::from_slice(&self.body).unwrap()
        }
    }
}
