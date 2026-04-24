//! Background roll-up job.
//!
//! The agent writes 1-Hz samples into `metric_snapshots` with `granularity='raw'`.
//! This task periodically aggregates those into coarser buckets so that
//! long-range queries stay fast without having to scan hours of 1-second rows.
//!
//! Retention:
//!   - raw — 24h
//!   - m1  — 30d
//!   - m5  — 180d
//!   - h1  — 1y
//!
//! Strategy: every tick is idempotent. We rescan a window that is wider than
//! the tick interval so a missed tick or restart doesn't drop a bucket; the
//! `ON CONFLICT DO UPDATE` clause makes a second pass over the same bucket a
//! no-op on the already-aggregated row and a refinement on still-growing ones.

use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;

const TICK: Duration = Duration::from_secs(60);

/// Spawn the roll-up task. Exits when `shutdown` fires.
pub fn spawn(pool: PgPool, shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(pool, shutdown))
}

async fn run(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // First tick fires immediately; delay that one so we don't race startup
    // work on the main thread.
    let _ = ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(err) = tick_once(&pool).await {
                    tracing::warn!(%err, "roll-up tick failed — will retry next minute");
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("roll-up task stopping");
                    return;
                }
            }
        }
    }
}

async fn tick_once(pool: &PgPool) -> sqlx::Result<()> {
    // Aggregate the three tiers in order so m5 can read freshly-written m1.
    roll_raw_to_m1(pool).await?;
    roll_m1_to_m5(pool).await?;
    roll_m5_to_h1(pool).await?;
    prune(pool).await?;
    Ok(())
}

/// raw → m1: aggregate the last 2 minutes of 1 Hz samples into 1-minute buckets.
async fn roll_raw_to_m1(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query(AGG_RAW_TO_M1).execute(pool).await?;
    Ok(())
}

/// m1 → m5: aggregate the last 10 minutes of 1-minute buckets into 5-minute ones.
async fn roll_m1_to_m5(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query(AGG_M1_TO_M5).execute(pool).await?;
    Ok(())
}

/// m5 → h1: aggregate the last 2 hours of 5-minute buckets into 1-hour ones.
async fn roll_m5_to_h1(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query(AGG_M5_TO_H1).execute(pool).await?;
    Ok(())
}

async fn prune(pool: &PgPool) -> sqlx::Result<()> {
    // One statement per `sqlx::query` call — prepared-statement protocol
    // rejects multiple semicolon-separated commands.
    for (granularity, interval) in [
        ("raw", "24 hours"),
        ("m1", "30 days"),
        ("m5", "180 days"),
        ("h1", "365 days"),
    ] {
        sqlx::query(
            "DELETE FROM metric_snapshots \
             WHERE granularity = $1 AND ts < NOW() - ($2::text)::interval",
        )
        .bind(granularity)
        .bind(interval)
        .execute(pool)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Aggregation SQL.
//
// For gauge metrics (cpu_pct, load, mem, swap, disk used/total, temperature)
// we take the average over the bucket — that's what the UI plots.
// For monotonic counters (net_*_total) we take MAX which is equivalent to
// "the last value" since they only grow.
// For rate metrics (net_*_bps) the average is correct because bps values are
// themselves per-second rates computed over their bucket.
// Nested JSONB fields (cpu_per_core, disks, nets) are NULL at aggregated tiers;
// the UI shows detail only at raw resolution.
// ---------------------------------------------------------------------------
const AGG_RAW_TO_M1: &str = r#"
INSERT INTO metric_snapshots
    (server_id, granularity, ts,
     cpu_pct, mem_used, mem_total, swap_used, swap_total,
     load_1, load_5, load_15,
     disk_used, disk_total,
     net_in_bps, net_out_bps, net_in_total, net_out_total,
     process_count, tcp_conn, udp_conn,
     temperature_c, gpu_pct)
SELECT
    server_id,
    'm1',
    date_trunc('minute', ts)      AS bucket_ts,
    AVG(cpu_pct),
    (AVG(mem_used))::bigint,  (AVG(mem_total))::bigint,
    (AVG(swap_used))::bigint, (AVG(swap_total))::bigint,
    AVG(load_1), AVG(load_5), AVG(load_15),
    (AVG(disk_used))::bigint, (AVG(disk_total))::bigint,
    (AVG(net_in_bps))::bigint,  (AVG(net_out_bps))::bigint,
    MAX(net_in_total),          MAX(net_out_total),
    (AVG(process_count))::int,  (AVG(tcp_conn))::int, (AVG(udp_conn))::int,
    AVG(temperature_c), AVG(gpu_pct)
FROM metric_snapshots
WHERE granularity = 'raw'
  AND ts >= date_trunc('minute', NOW() - INTERVAL '5 minutes')
  AND ts <  date_trunc('minute', NOW())
GROUP BY server_id, date_trunc('minute', ts)
ON CONFLICT (server_id, granularity, ts) DO UPDATE SET
    cpu_pct       = EXCLUDED.cpu_pct,
    mem_used      = EXCLUDED.mem_used,
    mem_total     = EXCLUDED.mem_total,
    swap_used     = EXCLUDED.swap_used,
    swap_total    = EXCLUDED.swap_total,
    load_1        = EXCLUDED.load_1,
    load_5        = EXCLUDED.load_5,
    load_15       = EXCLUDED.load_15,
    disk_used     = EXCLUDED.disk_used,
    disk_total    = EXCLUDED.disk_total,
    net_in_bps    = EXCLUDED.net_in_bps,
    net_out_bps   = EXCLUDED.net_out_bps,
    net_in_total  = EXCLUDED.net_in_total,
    net_out_total = EXCLUDED.net_out_total,
    process_count = EXCLUDED.process_count,
    tcp_conn      = EXCLUDED.tcp_conn,
    udp_conn      = EXCLUDED.udp_conn,
    temperature_c = EXCLUDED.temperature_c,
    gpu_pct       = EXCLUDED.gpu_pct;
"#;

const AGG_M1_TO_M5: &str = r#"
INSERT INTO metric_snapshots
    (server_id, granularity, ts,
     cpu_pct, mem_used, mem_total, swap_used, swap_total,
     load_1, load_5, load_15,
     disk_used, disk_total,
     net_in_bps, net_out_bps, net_in_total, net_out_total,
     process_count, tcp_conn, udp_conn,
     temperature_c, gpu_pct)
SELECT
    server_id,
    'm5',
    (date_trunc('hour', ts) + INTERVAL '5 minutes'
        * FLOOR(EXTRACT(minute FROM ts) / 5)) AS bucket_ts,
    AVG(cpu_pct),
    (AVG(mem_used))::bigint,  (AVG(mem_total))::bigint,
    (AVG(swap_used))::bigint, (AVG(swap_total))::bigint,
    AVG(load_1), AVG(load_5), AVG(load_15),
    (AVG(disk_used))::bigint, (AVG(disk_total))::bigint,
    (AVG(net_in_bps))::bigint,  (AVG(net_out_bps))::bigint,
    MAX(net_in_total),          MAX(net_out_total),
    (AVG(process_count))::int,  (AVG(tcp_conn))::int, (AVG(udp_conn))::int,
    AVG(temperature_c), AVG(gpu_pct)
FROM metric_snapshots
-- Process m1 rows from the last 30 minutes. We look slightly past the current
-- 5-minute boundary; the UPSERT below keeps this idempotent.
WHERE granularity = 'm1'
  AND ts >= NOW() - INTERVAL '30 minutes'
GROUP BY server_id,
         (date_trunc('hour', ts) + INTERVAL '5 minutes'
             * FLOOR(EXTRACT(minute FROM ts) / 5))
ON CONFLICT (server_id, granularity, ts) DO UPDATE SET
    cpu_pct       = EXCLUDED.cpu_pct,
    mem_used      = EXCLUDED.mem_used,
    mem_total     = EXCLUDED.mem_total,
    swap_used     = EXCLUDED.swap_used,
    swap_total    = EXCLUDED.swap_total,
    load_1        = EXCLUDED.load_1,
    load_5        = EXCLUDED.load_5,
    load_15       = EXCLUDED.load_15,
    disk_used     = EXCLUDED.disk_used,
    disk_total    = EXCLUDED.disk_total,
    net_in_bps    = EXCLUDED.net_in_bps,
    net_out_bps   = EXCLUDED.net_out_bps,
    net_in_total  = EXCLUDED.net_in_total,
    net_out_total = EXCLUDED.net_out_total,
    process_count = EXCLUDED.process_count,
    tcp_conn      = EXCLUDED.tcp_conn,
    udp_conn      = EXCLUDED.udp_conn,
    temperature_c = EXCLUDED.temperature_c,
    gpu_pct       = EXCLUDED.gpu_pct;
"#;

const AGG_M5_TO_H1: &str = r#"
INSERT INTO metric_snapshots
    (server_id, granularity, ts,
     cpu_pct, mem_used, mem_total, swap_used, swap_total,
     load_1, load_5, load_15,
     disk_used, disk_total,
     net_in_bps, net_out_bps, net_in_total, net_out_total,
     process_count, tcp_conn, udp_conn,
     temperature_c, gpu_pct)
SELECT
    server_id,
    'h1',
    date_trunc('hour', ts) AS bucket_ts,
    AVG(cpu_pct),
    (AVG(mem_used))::bigint,  (AVG(mem_total))::bigint,
    (AVG(swap_used))::bigint, (AVG(swap_total))::bigint,
    AVG(load_1), AVG(load_5), AVG(load_15),
    (AVG(disk_used))::bigint, (AVG(disk_total))::bigint,
    (AVG(net_in_bps))::bigint,  (AVG(net_out_bps))::bigint,
    MAX(net_in_total),          MAX(net_out_total),
    (AVG(process_count))::int,  (AVG(tcp_conn))::int, (AVG(udp_conn))::int,
    AVG(temperature_c), AVG(gpu_pct)
FROM metric_snapshots
-- Process m5 rows from the last 6 hours (wide enough to handle missed ticks).
WHERE granularity = 'm5'
  AND ts >= NOW() - INTERVAL '6 hours'
GROUP BY server_id, date_trunc('hour', ts)
ON CONFLICT (server_id, granularity, ts) DO UPDATE SET
    cpu_pct       = EXCLUDED.cpu_pct,
    mem_used      = EXCLUDED.mem_used,
    mem_total     = EXCLUDED.mem_total,
    swap_used     = EXCLUDED.swap_used,
    swap_total    = EXCLUDED.swap_total,
    load_1        = EXCLUDED.load_1,
    load_5        = EXCLUDED.load_5,
    load_15       = EXCLUDED.load_15,
    disk_used     = EXCLUDED.disk_used,
    disk_total    = EXCLUDED.disk_total,
    net_in_bps    = EXCLUDED.net_in_bps,
    net_out_bps   = EXCLUDED.net_out_bps,
    net_in_total  = EXCLUDED.net_in_total,
    net_out_total = EXCLUDED.net_out_total,
    process_count = EXCLUDED.process_count,
    tcp_conn      = EXCLUDED.tcp_conn,
    udp_conn      = EXCLUDED.udp_conn,
    temperature_c = EXCLUDED.temperature_c,
    gpu_pct       = EXCLUDED.gpu_pct;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use monitor_proto::v1::MetricSnapshot;
    use sqlx::postgres::PgPoolOptions;

    async fn fresh_pool() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
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
            .connect(&url)
            .await
            .ok()?;
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Some(pool)
    }

    async fn seed_server(pool: &PgPool) -> i64 {
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO servers (display_name, agent_id, server_token) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind("rollup-test")
        .bind(uuid::Uuid::new_v4())
        .bind("tkn")
        .fetch_one(pool)
        .await
        .unwrap();
        id
    }

    fn sample(ts_ms: i64, cpu: f64) -> MetricSnapshot {
        MetricSnapshot {
            ts_ms,
            cpu_pct: cpu,
            cpu_pct_per_core: vec![],
            mem_used: 1,
            mem_total: 10,
            swap_used: 0,
            swap_total: 0,
            load_1: cpu / 100.0,
            load_5: 0.0,
            load_15: 0.0,
            disk_used: 0,
            disk_total: 0,
            disks: vec![],
            net_in_bps: 0,
            net_out_bps: 0,
            net_in_total: 0,
            net_out_total: 0,
            nets: vec![],
            process_count: 0,
            tcp_conn: 0,
            udp_conn: 0,
            temperature_c: -1.0,
            gpu_pct: -1.0,
        }
    }

    #[tokio::test]
    async fn raw_to_m1_aggregates_average() {
        let Some(pool) = fresh_pool().await else {
            return;
        };
        let server_id = seed_server(&pool).await;

        // Write 6 raw samples at ~10s intervals, all within the SAME minute
        // that is now in the past (NOW - 90s..NOW - 30s). Use fixed ts so the
        // aggregation sees them inside a single completed minute bucket.
        //
        // We start from NOW() - 90s and go backwards in 10s increments,
        // staying inside one minute boundary.
        let now_ms: i64 = sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM NOW()) * 1000)::bigint")
            .fetch_one(&pool)
            .await
            .unwrap();
        // Align to a minute boundary in the past and add 10s offsets.
        let bucket_start_ms = ((now_ms - 90_000) / 60_000) * 60_000;
        let snapshots: Vec<MetricSnapshot> = (0i64..6)
            .map(|i| sample(bucket_start_ms + i * 10_000, 10.0 + (i as f64) * 10.0))
            .collect();
        super::super::ingest_batch(&pool, server_id, &snapshots)
            .await
            .unwrap();

        // Run the roll-up.
        tick_once(&pool).await.unwrap();

        let (cpu_avg,): (f64,) = sqlx::query_as(
            "SELECT cpu_pct FROM metric_snapshots WHERE server_id = $1 AND granularity = 'm1'",
        )
        .bind(server_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        // AVG of (10,20,30,40,50,60) = 35
        assert!((cpu_avg - 35.0).abs() < 0.01, "got cpu_avg = {cpu_avg}");
    }
}
