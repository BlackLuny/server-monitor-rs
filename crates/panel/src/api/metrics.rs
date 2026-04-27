//! Time-series query endpoint: `GET /api/servers/:id/metrics?range=1h`.
//!
//! The server picks the appropriate granularity for the requested window so
//! the client never fetches hundreds of thousands of 1-Hz points. The same
//! shape works for every `range`; only the number of points and their spacing
//! change.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct MetricsQuery {
    /// Supported values: `1h`, `6h`, `24h`, `7d`, `30d`. Anything else → 1h.
    #[serde(default = "default_range")]
    pub range: String,
}

fn default_range() -> String {
    "1h".into()
}

#[derive(Serialize)]
pub struct MetricsSeries {
    pub server_id: i64,
    pub range: String,
    pub granularity: &'static str,
    pub points: Vec<MetricPoint>,
}

/// One data point on the time-series. Everything the default UI charts uses
/// lives here — more specialized views can query a more detailed endpoint
/// later if we need per-core / per-disk / per-iface history.
#[derive(Serialize)]
pub struct MetricPoint {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub cpu_pct: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub swap_used: i64,
    pub swap_total: i64,
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,
    pub disk_used: i64,
    pub disk_total: i64,
    pub net_in_bps: i64,
    pub net_out_bps: i64,
    pub process_count: i32,
    pub tcp_conn: i32,
    pub udp_conn: i32,
    pub temperature_c: f64,
}

pub async fn server_metrics(
    State(state): State<AppState>,
    Path(server_id): Path<i64>,
    Query(q): Query<MetricsQuery>,
) -> impl IntoResponse {
    let (granularity, interval) = pick_granularity(&q.range);

    let rows = sqlx::query_as::<_, MetricPointDb>(
        r#"SELECT ts,
                  cpu_pct, mem_used, mem_total, swap_used, swap_total,
                  load_1, load_5, load_15,
                  disk_used, disk_total,
                  net_in_bps, net_out_bps,
                  process_count, tcp_conn, udp_conn,
                  temperature_c
           FROM metric_snapshots
           WHERE server_id = $1
             AND granularity = $2
             AND ts >= NOW() - ($3::text)::interval
           ORDER BY ts ASC"#,
    )
    .bind(server_id)
    .bind(granularity)
    .bind(interval)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(%err, "metrics query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    code: "internal_error",
                    message: "metrics query failed".into(),
                }),
            )
                .into_response();
        }
    };

    let points: Vec<MetricPoint> = rows.into_iter().map(MetricPoint::from).collect();
    Json(MetricsSeries {
        server_id,
        range: q.range,
        granularity,
        points,
    })
    .into_response()
}

/// One row per server with the last N seconds of raw samples folded into
/// per-metric arrays. Used by the dashboard to seed each card's sparkline
/// with real history instead of the previous flat-line fallback. Cheap:
/// the table is indexed on (server_id, granularity, ts) and 60×N samples
/// fit easily in one round-trip.
#[derive(Serialize)]
pub struct SparklineRow {
    pub server_id: i64,
    pub cpu_pct: Vec<f64>,
    pub mem_pct: Vec<f64>,
    pub net_in_bps: Vec<i64>,
    pub net_out_bps: Vec<i64>,
}

#[derive(Deserialize)]
pub struct SparklinesQuery {
    /// Window in seconds. Capped at 600 so a misbehaving client can't ask
    /// for hours of raw samples on every reload.
    #[serde(default = "default_seconds")]
    pub seconds: u32,
}

fn default_seconds() -> u32 {
    60
}

pub async fn server_sparklines(
    State(state): State<AppState>,
    Query(q): Query<SparklinesQuery>,
) -> impl IntoResponse {
    let seconds = q.seconds.clamp(10, 600);
    let interval = format!("{seconds} seconds");

    let rows: Result<Vec<(i64, f64, i64, i64, i64, i64)>, _> = sqlx::query_as(
        r#"SELECT server_id, cpu_pct, mem_used, mem_total, net_in_bps, net_out_bps
              FROM metric_snapshots
             WHERE granularity = 'raw'
               AND ts >= NOW() - ($1::text)::interval
             ORDER BY server_id, ts ASC"#,
    )
    .bind(&interval)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(%err, "sparklines query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    code: "internal_error",
                    message: "sparklines query failed".into(),
                }),
            )
                .into_response();
        }
    };

    use std::collections::HashMap;
    let mut by_server: HashMap<i64, SparklineRow> = HashMap::new();
    for (server_id, cpu_pct, mem_used, mem_total, net_in_bps, net_out_bps) in rows {
        let entry = by_server.entry(server_id).or_insert_with(|| SparklineRow {
            server_id,
            cpu_pct: Vec::new(),
            mem_pct: Vec::new(),
            net_in_bps: Vec::new(),
            net_out_bps: Vec::new(),
        });
        let mem_pct = if mem_total > 0 {
            (mem_used as f64 * 100.0) / mem_total as f64
        } else {
            0.0
        };
        entry.cpu_pct.push(cpu_pct);
        entry.mem_pct.push(mem_pct);
        entry.net_in_bps.push(net_in_bps);
        entry.net_out_bps.push(net_out_bps);
    }
    let out: Vec<SparklineRow> = by_server.into_values().collect();
    Json(out).into_response()
}

/// Choose the coarsest granularity that still gives the UI dense-enough data.
fn pick_granularity(range: &str) -> (&'static str, &'static str) {
    match range {
        // raw 1 Hz samples are kept for 24h; use them for the closest views.
        "1h" => ("raw", "1 hour"),
        "6h" => ("m1", "6 hours"),
        "24h" => ("m1", "24 hours"),
        "7d" => ("m5", "7 days"),
        "30d" => ("h1", "30 days"),
        _ => ("raw", "1 hour"),
    }
}

#[derive(sqlx::FromRow)]
struct MetricPointDb {
    ts: OffsetDateTime,
    cpu_pct: f64,
    mem_used: i64,
    mem_total: i64,
    swap_used: i64,
    swap_total: i64,
    load_1: f64,
    load_5: f64,
    load_15: f64,
    disk_used: i64,
    disk_total: i64,
    net_in_bps: i64,
    net_out_bps: i64,
    process_count: i32,
    tcp_conn: i32,
    udp_conn: i32,
    temperature_c: f64,
}

impl From<MetricPointDb> for MetricPoint {
    fn from(r: MetricPointDb) -> Self {
        Self {
            ts: r.ts,
            cpu_pct: r.cpu_pct,
            mem_used: r.mem_used,
            mem_total: r.mem_total,
            swap_used: r.swap_used,
            swap_total: r.swap_total,
            load_1: r.load_1,
            load_5: r.load_5,
            load_15: r.load_15,
            disk_used: r.disk_used,
            disk_total: r.disk_total,
            net_in_bps: r.net_in_bps,
            net_out_bps: r.net_out_bps,
            process_count: r.process_count,
            tcp_conn: r.tcp_conn,
            udp_conn: r.udp_conn,
            temperature_c: r.temperature_c,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}
