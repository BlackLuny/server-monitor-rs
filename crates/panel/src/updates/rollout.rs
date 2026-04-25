//! Rollout state machine + assignment generation.
//!
//! State graph:
//!
//! ```text
//!        create_rollout
//!              ↓
//!           pending  ─pause→  paused  ─resume→  active  ─complete→  completed
//!              │                  │                │                    ↑
//!              └──── start ─────→ │                │                    │
//!                                 └─── abort ──→ aborted                │
//!                                                  ↑                    │
//!                                                  │       (terminal)   │
//!                                                  └────────────────────┘
//! ```
//!
//! A rollout becomes `active` automatically the first time `create_rollout`
//! commits assignments — there's no separate `start` step today. Future
//! work might add a "scheduled" state for cron-style timing.

// The `query_as::<_, (T0, T1, …)>` shape on rollout summaries trips clippy's
// type_complexity lint. Naming the tuple wouldn't make the SQL any clearer
// — the columns are positional by construction — so we silence the lint
// at the module level.
#![allow(clippy::type_complexity)]

use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::poller::{LatestRelease, ReleaseAsset};

/// What an admin sends to `POST /api/updates/rollout`.
#[derive(Debug, Deserialize)]
pub struct CreateRolloutInput {
    /// Release tag, e.g. `"v0.1.0"`. Must match the cached
    /// `settings.latest_release.tag` for now (we only ever ship the latest).
    pub version: String,
    /// 1..=100. Combined with `agent_ids`: when empty, percent samples the
    /// full eligible set; when non-empty, percent applies to that subset.
    #[serde(default = "default_percent")]
    pub percent: i32,
    #[serde(default)]
    pub agent_ids: Vec<Uuid>,
    #[serde(default)]
    pub note: Option<String>,
}

fn default_percent() -> i32 {
    100
}

/// Compact summary used in list views.
#[derive(Debug, Serialize)]
pub struct RolloutSummary {
    pub id: i64,
    pub version: String,
    pub state: String,
    pub percent: i32,
    pub created_by: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub note: Option<String>,
    pub assignments_total: i64,
    pub assignments_pending: i64,
    pub assignments_sent: i64,
    pub assignments_succeeded: i64,
    pub assignments_failed: i64,
}

/// Single-rollout view including the per-agent assignment list.
#[derive(Debug, Serialize)]
pub struct RolloutView {
    pub summary: RolloutSummary,
    pub assignments: Vec<AssignmentView>,
}

#[derive(Debug, Serialize)]
pub struct AssignmentView {
    pub agent_id: Uuid,
    pub display_name: String,
    pub target: String,
    pub state: String,
    pub last_status_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum RolloutError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("settings: {0}")]
    Settings(#[from] crate::settings::SettingsError),
    #[error("no cached release — wait for the poller's next tick")]
    NoCachedRelease,
    #[error("requested version {asked} doesn't match cached {cached}")]
    VersionMismatch { asked: String, cached: String },
    #[error("requested version {asked} is not in the cached recent_releases list")]
    VersionUnknown { asked: String },
    #[error("percent must be between 1 and 100, got {0}")]
    PercentOutOfRange(i32),
    #[error("no eligible agents matched this rollout")]
    NoEligibleAgents,
    #[error("rollout {id} not found")]
    NotFound { id: i64 },
    #[error("cannot transition {id} from {from} to {to}")]
    BadTransition {
        id: i64,
        from: String,
        to: &'static str,
    },
}

/// Convenient enum for the state machine. Stays internal so the DB CHECK
/// stays the canonical list.
#[derive(Copy, Clone)]
enum State {
    Pending,
    Active,
    Paused,
    Completed,
    Aborted,
}

impl State {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "aborted" => Self::Aborted,
            _ => return None,
        })
    }
}

#[derive(Default, Debug, Clone)]
pub struct AgentFilter {
    pub agent_ids: Vec<Uuid>,
    pub percent: i32,
}

/// Compose a Rust target triple from the hardware columns we record on
/// register. Returns `None` when we don't recognise the OS/arch combo —
/// those agents get skipped.
#[must_use]
pub fn agent_target_triple(os: &str, arch: &str) -> Option<&'static str> {
    let arch = arch.trim().to_ascii_lowercase();
    let os = os.trim().to_ascii_lowercase();
    let arch = match arch.as_str() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => return None,
    };
    match os.as_str() {
        // Linux distributions all share the musl artefact today.
        "linux" => Some(if arch == "aarch64" {
            "aarch64-unknown-linux-musl"
        } else {
            "x86_64-unknown-linux-musl"
        }),
        "macos" | "darwin" => Some(if arch == "aarch64" {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        }),
        "windows" => {
            if arch == "x86_64" {
                Some("x86_64-pc-windows-msvc")
            } else {
                None // no Windows ARM build today
            }
        }
        _ => None,
    }
}

/// Locate the cached metadata for a target version.
///
/// Looks first in `recent_releases` (the multi-version cache populated by
/// the poller); falls back to `latest_release` for back-compat with
/// installations that haven't run the new poller yet. Returns
/// `VersionUnknown` if neither cache mentions the requested tag.
async fn pick_cached_release(pool: &PgPool, version: &str) -> Result<LatestRelease, RolloutError> {
    if let Some(list) = crate::settings::get::<Vec<LatestRelease>>(pool, "recent_releases").await? {
        if let Some(found) = list.into_iter().find(|r| r.tag == version) {
            return Ok(found);
        }
        // The cache exists but doesn't contain the target. Surface that
        // explicitly so the UI can prompt the admin to wait for the next
        // poller tick or pin the asset URL manually.
        return Err(RolloutError::VersionUnknown {
            asked: version.to_owned(),
        });
    }

    match crate::settings::get::<LatestRelease>(pool, "latest_release").await? {
        Some(v) if v.tag == version => Ok(v),
        Some(v) => Err(RolloutError::VersionMismatch {
            asked: version.to_owned(),
            cached: v.tag,
        }),
        None => Err(RolloutError::NoCachedRelease),
    }
}

/// Public lookup used by the admin "/api/updates/recent" endpoint.
pub async fn list_recent_releases(pool: &PgPool) -> Result<Vec<LatestRelease>, RolloutError> {
    Ok(
        crate::settings::get::<Vec<LatestRelease>>(pool, "recent_releases")
            .await?
            .unwrap_or_default(),
    )
}

#[derive(sqlx::FromRow)]
struct EligibleAgentRow {
    agent_id: Uuid,
    hw_os: Option<String>,
    hw_arch: Option<String>,
}

/// Create a rollout, materialise its assignments, and return the new id.
///
/// Failure modes that bubble up to the API:
///   - asked version doesn't match the cached release
///   - no eligible agents survived target-triple matching
///   - percent out of range
pub async fn create_rollout(
    pool: &PgPool,
    input: CreateRolloutInput,
    created_by: Option<i64>,
) -> Result<i64, RolloutError> {
    if !(1..=100).contains(&input.percent) {
        return Err(RolloutError::PercentOutOfRange(input.percent));
    }

    let cached: LatestRelease = pick_cached_release(pool, &input.version).await?;

    // Pull the eligible agent set: anything we have OS/arch for. We don't
    // enforce online-ness here — assignments stay `pending` until the
    // agent reconnects, which is exactly the desired behaviour for a
    // rollout that overlaps with a maintenance window.
    let mut eligible: Vec<EligibleAgentRow> =
        sqlx::query_as("SELECT agent_id, hw_os, hw_arch FROM servers WHERE agent_id IS NOT NULL")
            .fetch_all(pool)
            .await?;

    if !input.agent_ids.is_empty() {
        let allow: HashSet<Uuid> = input.agent_ids.iter().copied().collect();
        eligible.retain(|r| allow.contains(&r.agent_id));
    }

    // Build (agent_id, target_triple) tuples; drop agents we can't target.
    let mut typed: Vec<(Uuid, &'static str)> = eligible
        .into_iter()
        .filter_map(|r| {
            let os = r.hw_os.as_deref().unwrap_or("");
            let arch = r.hw_arch.as_deref().unwrap_or("");
            agent_target_triple(os, arch).map(|t| (r.agent_id, t))
        })
        .collect();

    if typed.is_empty() {
        return Err(RolloutError::NoEligibleAgents);
    }

    // Apply percent sampling. Random shuffle + take so a 25% rollout picks
    // a different slice every time rather than always the lowest agent ids.
    typed.shuffle(&mut rand::thread_rng());
    let take = ((typed.len() as f64) * (input.percent as f64) / 100.0).ceil() as usize;
    let take = take.max(1).min(typed.len());
    typed.truncate(take);

    // Look up artefact URL + sha per target triple.
    let asset_index = build_asset_index(&cached.assets);

    let agent_ids_jsonb =
        serde_json::to_value(&input.agent_ids).expect("Vec<Uuid> serialises cleanly");

    let mut tx = pool.begin().await?;
    let rollout_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO update_rollouts
               (version, created_by, percent, agent_ids, state, started_at, note)
               VALUES ($1, $2, $3, $4, 'active', NOW(), $5)
               RETURNING id"#,
    )
    .bind(&input.version)
    .bind(created_by)
    .bind(input.percent)
    .bind(&agent_ids_jsonb)
    .bind(input.note.as_deref())
    .fetch_one(&mut *tx)
    .await?;

    let mut inserted_any = false;
    for (agent_id, triple) in &typed {
        let Some(asset) = asset_for_target(&asset_index, "monitor-agent", triple) else {
            tracing::warn!(%agent_id, %triple, "no agent artefact for this target — skipping");
            continue;
        };
        sqlx::query(
            r#"INSERT INTO update_assignments
                   (rollout_id, agent_id, target, artefact_url, artefact_sha256)
                   VALUES ($1, $2, $3, $4, $5)
                   ON CONFLICT (rollout_id, agent_id) DO NOTHING"#,
        )
        .bind(rollout_id)
        .bind(agent_id)
        .bind(triple)
        .bind(&asset.url)
        .bind(&asset.sha256)
        .execute(&mut *tx)
        .await?;
        inserted_any = true;
    }
    if !inserted_any {
        tx.rollback().await?;
        return Err(RolloutError::NoEligibleAgents);
    }

    tx.commit().await?;
    Ok(rollout_id)
}

/// Index assets by `(binary, target_triple)` so callers can pull the right
/// artefact + sha out without scanning the list every time. The expected
/// asset name shape is `monitor-{bin}-{triple}.{tar.gz|zip}`.
fn build_asset_index(assets: &[ReleaseAsset]) -> HashMap<(String, String), &ReleaseAsset> {
    let mut idx = HashMap::new();
    for asset in assets {
        if let Some((bin, triple)) = parse_asset_name(&asset.name) {
            idx.insert((bin, triple), asset);
        }
    }
    idx
}

fn asset_for_target<'a>(
    idx: &HashMap<(String, String), &'a ReleaseAsset>,
    bin: &str,
    triple: &str,
) -> Option<&'a ReleaseAsset> {
    idx.get(&(bin.to_owned(), triple.to_owned())).copied()
}

/// `monitor-agent-x86_64-unknown-linux-musl.tar.gz` → ("monitor-agent", "x86_64-unknown-linux-musl")
fn parse_asset_name(name: &str) -> Option<(String, String)> {
    let stripped = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".zip"))?;
    // Find where the target triple starts. Triples always begin with one
    // of these arches, so we can scan for the first occurrence.
    for arch in ["x86_64-", "aarch64-", "i686-"] {
        if let Some(idx) = stripped.find(arch) {
            let (bin, triple) = stripped.split_at(idx);
            let bin = bin.trim_end_matches('-');
            if !bin.is_empty() && !triple.is_empty() {
                return Some((bin.to_owned(), triple.to_owned()));
            }
        }
    }
    None
}

pub async fn list_rollouts(pool: &PgPool) -> Result<Vec<RolloutSummary>, RolloutError> {
    let rows: Vec<(
        i64,
        String,
        String,
        i32,
        Option<i64>,
        OffsetDateTime,
        Option<String>,
    )> = sqlx::query_as(
        r#"SELECT id, version, state, percent, created_by, created_at, note
              FROM update_rollouts
              ORDER BY created_at DESC
              LIMIT 50"#,
    )
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, version, state, percent, created_by, created_at, note) in rows {
        out.push(
            load_summary_with(
                pool, id, version, state, percent, created_by, created_at, note,
            )
            .await?,
        );
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn load_summary_with(
    pool: &PgPool,
    id: i64,
    version: String,
    state: String,
    percent: i32,
    created_by: Option<i64>,
    created_at: OffsetDateTime,
    note: Option<String>,
) -> Result<RolloutSummary, RolloutError> {
    let counts: (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = sqlx::query_as(
        r#"SELECT
               COUNT(*),
               COUNT(*) FILTER (WHERE state = 'pending'),
               COUNT(*) FILTER (WHERE state = 'sent'),
               COUNT(*) FILTER (WHERE state = 'succeeded'),
               COUNT(*) FILTER (WHERE state = 'failed')
              FROM update_assignments
              WHERE rollout_id = $1"#,
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(RolloutSummary {
        id,
        version,
        state,
        percent,
        created_by,
        created_at,
        note,
        assignments_total: counts.0.unwrap_or(0),
        assignments_pending: counts.1.unwrap_or(0),
        assignments_sent: counts.2.unwrap_or(0),
        assignments_succeeded: counts.3.unwrap_or(0),
        assignments_failed: counts.4.unwrap_or(0),
    })
}

pub async fn get_rollout(pool: &PgPool, id: i64) -> Result<RolloutView, RolloutError> {
    let head: Option<(
        i64,
        String,
        String,
        i32,
        Option<i64>,
        OffsetDateTime,
        Option<String>,
    )> = sqlx::query_as(
        r#"SELECT id, version, state, percent, created_by, created_at, note
              FROM update_rollouts
              WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((id, version, state, percent, created_by, created_at, note)) = head else {
        return Err(RolloutError::NotFound { id });
    };
    let summary = load_summary_with(
        pool, id, version, state, percent, created_by, created_at, note,
    )
    .await?;

    let rows: Vec<(Uuid, String, String, String, Option<String>, OffsetDateTime)> = sqlx::query_as(
        r#"SELECT a.agent_id, COALESCE(s.display_name, ''), a.target,
                  a.state, a.last_status_message, a.updated_at
              FROM update_assignments a
              LEFT JOIN servers s ON s.agent_id = a.agent_id
              WHERE a.rollout_id = $1
              ORDER BY s.display_name NULLS LAST, a.agent_id"#,
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let assignments = rows
        .into_iter()
        .map(
            |(agent_id, display_name, target, state, last_status_message, updated_at)| {
                AssignmentView {
                    agent_id,
                    display_name,
                    target,
                    state,
                    last_status_message,
                    updated_at,
                }
            },
        )
        .collect();
    Ok(RolloutView {
        summary,
        assignments,
    })
}

pub async fn pause_rollout(pool: &PgPool, id: i64) -> Result<(), RolloutError> {
    transition(pool, id, &[State::Active], "paused").await
}
pub async fn resume_rollout(pool: &PgPool, id: i64) -> Result<(), RolloutError> {
    transition(pool, id, &[State::Paused], "active").await
}
pub async fn abort_rollout(pool: &PgPool, id: i64) -> Result<(), RolloutError> {
    transition(
        pool,
        id,
        &[State::Pending, State::Active, State::Paused],
        "aborted",
    )
    .await
}

async fn transition(
    pool: &PgPool,
    id: i64,
    allowed_from: &[State],
    to: &'static str,
) -> Result<(), RolloutError> {
    let current: Option<(String,)> =
        sqlx::query_as("SELECT state FROM update_rollouts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some((current,)) = current else {
        return Err(RolloutError::NotFound { id });
    };
    let parsed = State::parse(&current).ok_or_else(|| RolloutError::BadTransition {
        id,
        from: current.clone(),
        to,
    })?;
    let allowed = allowed_from
        .iter()
        .any(|s| std::mem::discriminant(s) == std::mem::discriminant(&parsed));
    if !allowed {
        return Err(RolloutError::BadTransition {
            id,
            from: current,
            to,
        });
    }
    let finished_clause = if matches!(to, "aborted" | "completed") {
        ", finished_at = NOW()"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE update_rollouts SET state = $1{} WHERE id = $2",
        finished_clause
    );
    sqlx::query(&sql).bind(to).bind(id).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_triple_mapping() {
        assert_eq!(
            agent_target_triple("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            agent_target_triple("Linux", "amd64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            agent_target_triple("macos", "arm64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            agent_target_triple("darwin", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            agent_target_triple("windows", "x86_64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert!(agent_target_triple("windows", "aarch64").is_none()); // no win arm
        assert!(agent_target_triple("freebsd", "x86_64").is_none());
    }

    #[test]
    fn parse_asset_names() {
        assert_eq!(
            parse_asset_name("monitor-agent-x86_64-unknown-linux-musl.tar.gz").unwrap(),
            ("monitor-agent".into(), "x86_64-unknown-linux-musl".into())
        );
        assert_eq!(
            parse_asset_name("monitor-agent-supervisor-aarch64-apple-darwin.tar.gz").unwrap(),
            (
                "monitor-agent-supervisor".into(),
                "aarch64-apple-darwin".into()
            )
        );
        assert_eq!(
            parse_asset_name("monitor-agent-x86_64-pc-windows-msvc.zip").unwrap(),
            ("monitor-agent".into(), "x86_64-pc-windows-msvc".into())
        );
        assert!(parse_asset_name("README.md").is_none());
        assert!(parse_asset_name("SHA256SUMS").is_none());
    }
}
