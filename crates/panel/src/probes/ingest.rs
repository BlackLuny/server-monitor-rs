//! Persist probe results pushed by an agent.
//!
//! Each row is `(probe_id, agent_id, granularity='raw', ts)`. We tolerate
//! duplicates from a fast reconnect via `ON CONFLICT DO NOTHING` — same
//! contract as `metrics::ingest_batch`.

use monitor_proto::v1::ProbeResult;
use sqlx::{PgPool, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

const GRANULARITY_RAW: &str = "raw";

/// Ingest one batch of probe results from a given agent. The proto carries
/// `probe_id` as a string (forward-compat) but our DB uses BIGSERIAL — the
/// agent only sends ids it received from us, so they round-trip cleanly.
/// Malformed ids are skipped with a debug log.
pub async fn ingest_batch(
    pool: &PgPool,
    agent_id: Uuid,
    results: &[ProbeResult],
) -> sqlx::Result<u64> {
    if results.is_empty() {
        return Ok(0);
    }

    // Pre-parse ids and drop any that don't round-trip — the agent must have
    // received them from us, so anything that doesn't parse is either stale
    // or a bug; either way we don't want it in the DB.
    let parsed: Vec<(i64, &ProbeResult)> = results
        .iter()
        .filter_map(|r| match r.probe_id.parse::<i64>() {
            Ok(id) => Some((id, r)),
            Err(_) => {
                tracing::debug!(probe_id = %r.probe_id, "ignoring non-integer probe id");
                None
            }
        })
        .collect();
    if parsed.is_empty() {
        return Ok(0);
    }

    let mut qb = QueryBuilder::new(
        "INSERT INTO probe_results \
         (probe_id, agent_id, granularity, ts, ok, latency_us, \
          success_rate, sample_count, status_code, error) ",
    );

    qb.push_values(parsed.iter(), |mut row, (probe_id, r)| {
        row.push_bind(*probe_id)
            .push_bind(agent_id)
            .push_bind(GRANULARITY_RAW)
            .push_bind(ts_from_ms(r.ts_ms))
            .push_bind(r.ok)
            .push_bind(i64::from(r.latency_us))
            .push_bind(if r.ok { 1.0_f64 } else { 0.0_f64 })
            .push_bind(1_i32)
            .push_bind(if r.status_code == 0 {
                None
            } else {
                Some(i32::try_from(r.status_code).unwrap_or(i32::MAX))
            })
            .push_bind(if r.error.is_empty() {
                None
            } else {
                Some(r.error.as_str())
            });
    });

    qb.push(" ON CONFLICT (probe_id, agent_id, granularity, ts) DO NOTHING");

    let result = qb.build().execute(pool).await?;
    Ok(result.rows_affected())
}

fn ts_from_ms(ts_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
}
