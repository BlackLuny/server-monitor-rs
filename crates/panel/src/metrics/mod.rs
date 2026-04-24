//! Metric ingestion and query helpers.
//!
//! [`ingest_batch`] writes a batch of raw agent samples into `metric_snapshots`
//! in a single multi-row INSERT. The roll-up task (in [`rollup`]) later
//! aggregates these into `m1`, `m5`, and `h1` granularity rows.

pub mod rollup;

use monitor_proto::v1::{DiskUsage, MetricSnapshot, NetUsage};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, QueryBuilder};
use time::OffsetDateTime;

const GRANULARITY_RAW: &str = "raw";

/// Insert raw agent samples. Existing rows with the same `(server_id, ts)`
/// are left alone (duplicate heartbeats from a fast reconnect must not
/// clobber good data).
pub async fn ingest_batch(
    pool: &PgPool,
    server_id: i64,
    snapshots: &[MetricSnapshot],
) -> sqlx::Result<u64> {
    if snapshots.is_empty() {
        return Ok(0);
    }

    let mut qb = QueryBuilder::new(
        "INSERT INTO metric_snapshots \
         (server_id, granularity, ts, \
          cpu_pct, cpu_per_core, \
          mem_used, mem_total, swap_used, swap_total, \
          load_1, load_5, load_15, \
          disk_used, disk_total, disks, \
          net_in_bps, net_out_bps, net_in_total, net_out_total, nets, \
          process_count, tcp_conn, udp_conn, \
          temperature_c, gpu_pct) ",
    );

    qb.push_values(snapshots.iter(), |mut row, s| {
        row.push_bind(server_id)
            .push_bind(GRANULARITY_RAW)
            .push_bind(ts_from_ms(s.ts_ms))
            .push_bind(s.cpu_pct)
            .push_bind(Value::Array(
                s.cpu_pct_per_core
                    .iter()
                    .map(|v| {
                        serde_json::Number::from_f64(*v)
                            .map(Value::Number)
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ))
            .push_bind(i64::try_from(s.mem_used).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.mem_total).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.swap_used).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.swap_total).unwrap_or(i64::MAX))
            .push_bind(s.load_1)
            .push_bind(s.load_5)
            .push_bind(s.load_15)
            .push_bind(i64::try_from(s.disk_used).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.disk_total).unwrap_or(i64::MAX))
            .push_bind(disks_to_json(&s.disks))
            .push_bind(i64::try_from(s.net_in_bps).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.net_out_bps).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.net_in_total).unwrap_or(i64::MAX))
            .push_bind(i64::try_from(s.net_out_total).unwrap_or(i64::MAX))
            .push_bind(nets_to_json(&s.nets))
            .push_bind(i32::try_from(s.process_count).unwrap_or(i32::MAX))
            .push_bind(i32::try_from(s.tcp_conn).unwrap_or(i32::MAX))
            .push_bind(i32::try_from(s.udp_conn).unwrap_or(i32::MAX))
            .push_bind(s.temperature_c)
            .push_bind(s.gpu_pct);
    });

    qb.push(" ON CONFLICT (server_id, granularity, ts) DO NOTHING");

    let result = qb.build().execute(pool).await?;
    Ok(result.rows_affected())
}

/// Convenience wrapper for single-sample payloads.
pub async fn ingest_one(
    pool: &PgPool,
    server_id: i64,
    snapshot: &MetricSnapshot,
) -> sqlx::Result<u64> {
    ingest_batch(pool, server_id, std::slice::from_ref(snapshot)).await
}

fn ts_from_ms(ts_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
}

#[derive(Serialize)]
struct DiskDetail<'a> {
    mount: &'a str,
    fstype: &'a str,
    used: u64,
    total: u64,
    read_bps: u64,
    write_bps: u64,
}

#[derive(Serialize)]
struct NetDetail<'a> {
    name: &'a str,
    rx_bps: u64,
    tx_bps: u64,
    rx_total: u64,
    tx_total: u64,
}

fn disks_to_json(disks: &[DiskUsage]) -> Value {
    let v: Vec<DiskDetail<'_>> = disks
        .iter()
        .map(|d| DiskDetail {
            mount: &d.mount,
            fstype: &d.fstype,
            used: d.used,
            total: d.total,
            read_bps: d.read_bps,
            write_bps: d.write_bps,
        })
        .collect();
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn nets_to_json(nets: &[NetUsage]) -> Value {
    let v: Vec<NetDetail<'_>> = nets
        .iter()
        .map(|n| NetDetail {
            name: &n.name,
            rx_bps: n.rx_bps,
            tx_bps: n.tx_bps,
            rx_total: n.rx_total,
            tx_total: n.tx_total,
        })
        .collect();
    serde_json::to_value(v).unwrap_or(Value::Null)
}
