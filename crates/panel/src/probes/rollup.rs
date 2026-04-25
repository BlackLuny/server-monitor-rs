//! Periodic rollup of probe results from raw → m1 → m5 → h1.
//!
//! Each tier records `success_rate` (fraction in [0,1]), `latency_us` (mean
//! over successes), p50/p95 latency, and a representative `error` (the most
//! recent failure message in the bucket, NULL if none). The retention is
//! more generous than metrics because probe rows are tiny:
//!   raw 7d / m1 90d / m5 180d / h1 2y.

use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;

const TICK: Duration = Duration::from_secs(60);

pub fn spawn(pool: PgPool, shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(pool, shutdown))
}

async fn run(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = ticker.tick().await; // burn first tick

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(err) = tick_once(&pool).await {
                    tracing::warn!(%err, "probe rollup tick failed");
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("probe rollup task stopping");
                    return;
                }
            }
        }
    }
}

async fn tick_once(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query(AGG_RAW_TO_M1).execute(pool).await?;
    sqlx::query(AGG_M1_TO_M5).execute(pool).await?;
    sqlx::query(AGG_M5_TO_H1).execute(pool).await?;
    prune(pool).await?;
    Ok(())
}

async fn prune(pool: &PgPool) -> sqlx::Result<()> {
    for (granularity, interval) in [
        ("raw", "7 days"),
        ("m1", "90 days"),
        ("m5", "180 days"),
        ("h1", "2 years"),
    ] {
        sqlx::query(
            "DELETE FROM probe_results \
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
// success_rate aggregates over all rows; latency_us aggregates over rows
// where `ok = true` (failures don't contribute to "how fast is it when it
// works"). PERCENTILE_CONT inside FILTER (...) handles missing successes
// cleanly — the percentile expression returns NULL when no rows match.
// ---------------------------------------------------------------------------

const AGG_RAW_TO_M1: &str = r#"
INSERT INTO probe_results
    (probe_id, agent_id, granularity, ts,
     ok, latency_us, latency_us_p50, latency_us_p95,
     success_rate, sample_count, status_code, error)
SELECT
    probe_id,
    agent_id,
    'm1',
    date_trunc('minute', ts)                              AS bucket_ts,
    BOOL_OR(ok)                                           AS ok,
    COALESCE((AVG(latency_us) FILTER (WHERE ok))::bigint, 0) AS latency_us,
    PERCENTILE_CONT(0.5)  WITHIN GROUP (ORDER BY latency_us)
        FILTER (WHERE ok)::bigint                         AS latency_us_p50,
    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_us)
        FILTER (WHERE ok)::bigint                         AS latency_us_p95,
    AVG(CASE WHEN ok THEN 1.0 ELSE 0.0 END)               AS success_rate,
    COUNT(*)::int                                         AS sample_count,
    MAX(status_code)                                      AS status_code,
    (ARRAY_AGG(error ORDER BY ts DESC) FILTER (WHERE error IS NOT NULL))[1] AS error
FROM probe_results
WHERE granularity = 'raw'
  AND ts >= date_trunc('minute', NOW() - INTERVAL '5 minutes')
  AND ts <  date_trunc('minute', NOW())
GROUP BY probe_id, agent_id, date_trunc('minute', ts)
ON CONFLICT (probe_id, agent_id, granularity, ts) DO UPDATE SET
    ok             = EXCLUDED.ok,
    latency_us     = EXCLUDED.latency_us,
    latency_us_p50 = EXCLUDED.latency_us_p50,
    latency_us_p95 = EXCLUDED.latency_us_p95,
    success_rate   = EXCLUDED.success_rate,
    sample_count   = EXCLUDED.sample_count,
    status_code    = EXCLUDED.status_code,
    error          = EXCLUDED.error;
"#;

const AGG_M1_TO_M5: &str = r#"
INSERT INTO probe_results
    (probe_id, agent_id, granularity, ts,
     ok, latency_us, latency_us_p50, latency_us_p95,
     success_rate, sample_count, status_code, error)
SELECT
    probe_id,
    agent_id,
    'm5',
    (date_trunc('hour', ts) + INTERVAL '5 minutes'
        * FLOOR(EXTRACT(minute FROM ts) / 5))             AS bucket_ts,
    BOOL_OR(ok),
    COALESCE((AVG(latency_us) FILTER (WHERE ok))::bigint, 0),
    -- Compute percentile of *bucket* p50/p95 from the m1 tier — close enough
    -- without re-scanning raw.
    AVG(latency_us_p50)::bigint,
    AVG(latency_us_p95)::bigint,
    AVG(success_rate),
    SUM(sample_count)::int,
    MAX(status_code),
    (ARRAY_AGG(error ORDER BY ts DESC) FILTER (WHERE error IS NOT NULL))[1]
FROM probe_results
WHERE granularity = 'm1'
  AND ts >= NOW() - INTERVAL '30 minutes'
GROUP BY probe_id, agent_id,
         (date_trunc('hour', ts) + INTERVAL '5 minutes'
             * FLOOR(EXTRACT(minute FROM ts) / 5))
ON CONFLICT (probe_id, agent_id, granularity, ts) DO UPDATE SET
    ok             = EXCLUDED.ok,
    latency_us     = EXCLUDED.latency_us,
    latency_us_p50 = EXCLUDED.latency_us_p50,
    latency_us_p95 = EXCLUDED.latency_us_p95,
    success_rate   = EXCLUDED.success_rate,
    sample_count   = EXCLUDED.sample_count,
    status_code    = EXCLUDED.status_code,
    error          = EXCLUDED.error;
"#;

const AGG_M5_TO_H1: &str = r#"
INSERT INTO probe_results
    (probe_id, agent_id, granularity, ts,
     ok, latency_us, latency_us_p50, latency_us_p95,
     success_rate, sample_count, status_code, error)
SELECT
    probe_id,
    agent_id,
    'h1',
    date_trunc('hour', ts)                                AS bucket_ts,
    BOOL_OR(ok),
    COALESCE((AVG(latency_us) FILTER (WHERE ok))::bigint, 0),
    AVG(latency_us_p50)::bigint,
    AVG(latency_us_p95)::bigint,
    AVG(success_rate),
    SUM(sample_count)::int,
    MAX(status_code),
    (ARRAY_AGG(error ORDER BY ts DESC) FILTER (WHERE error IS NOT NULL))[1]
FROM probe_results
WHERE granularity = 'm5'
  AND ts >= NOW() - INTERVAL '6 hours'
GROUP BY probe_id, agent_id, date_trunc('hour', ts)
ON CONFLICT (probe_id, agent_id, granularity, ts) DO UPDATE SET
    ok             = EXCLUDED.ok,
    latency_us     = EXCLUDED.latency_us,
    latency_us_p50 = EXCLUDED.latency_us_p50,
    latency_us_p95 = EXCLUDED.latency_us_p95,
    success_rate   = EXCLUDED.success_rate,
    sample_count   = EXCLUDED.sample_count,
    status_code    = EXCLUDED.status_code,
    error          = EXCLUDED.error;
"#;
