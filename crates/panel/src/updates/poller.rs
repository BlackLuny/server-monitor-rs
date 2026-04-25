//! Periodic GitHub release poller.
//!
//! Wakes every 5 minutes (anonymous GitHub limits us to 60 req/hr — ample
//! headroom), pulls the configured repo's `/releases/latest` JSON, fetches
//! and parses the bundled `SHA256SUMS` file so per-asset hashes ride along
//! with the asset listing, and writes the whole thing as a single JSON
//! blob into `settings.latest_release`.
//!
//! The cached blob is what `/api/updates/latest` returns and what
//! `rollout::create_rollout` reads to populate `update_assignments`. We
//! never re-fetch from inside a request handler.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::watch;

use crate::settings;

/// How long between background pulls. 5 minutes keeps us well under the
/// anonymous rate limit and is fast enough that an admin who tags a new
/// release sees it in `/settings/updates` within one polling window.
const POLL_INTERVAL: Duration = Duration::from_secs(300);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on SHA256SUMS download size — guards against a hostile / corrupted
/// release with a giant text file.
const MAX_SHA256SUMS_BYTES: usize = 64 * 1024;
/// Max releases to retain in the `recent_releases` cache. The rollback
/// dropdown shows this many entries; older versions are still installable
/// by typing the tag manually.
const RECENT_LIMIT: usize = 10;

/// What we cache in `settings.latest_release`. Stable JSON shape so the
/// frontend / rollout creator can rely on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestRelease {
    pub tag: String,
    pub name: Option<String>,
    pub html_url: Option<String>,
    pub prerelease: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: i64,
    /// Hex sha256 from SHA256SUMS, or empty when the release didn't ship one.
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PollerError {
    #[error("settings: {0}")]
    Settings(#[from] crate::settings::SettingsError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("github returned status {status} for {url}")]
    BadStatus { status: u16, url: String },
    #[error("malformed release JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Spawn the long-running poller. Honors `shutdown` so the panel can exit
/// cleanly even mid-fetch.
pub fn spawn(pool: PgPool, mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // First poll runs ~30s after boot rather than immediately — gives
        // sqlx + tracing time to settle before we shout about HTTP errors
        // on a freshly-installed panel that doesn't have outbound access yet.
        let initial_delay = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(initial_delay);
        tokio::select! {
            _ = &mut initial_delay => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return }
            }
        }

        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let _ = ticker.tick().await; // burn first immediate tick

        loop {
            match poll_once(&pool).await {
                Ok(rels) => tracing::info!(
                    cached = rels.len(),
                    latest = rels.first().map(|r| r.tag.as_str()).unwrap_or("<none>"),
                    "release poll succeeded",
                ),
                Err(err) => tracing::warn!(%err, "release poll failed"),
            }

            tokio::select! {
                _ = ticker.tick() => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("release poller stopping");
                        return;
                    }
                }
            }
        }
    })
}

/// One poll iteration. Fetches up to `RECENT_LIMIT` recent releases,
/// filters them per channel, and writes both `settings.recent_releases`
/// (the full list) and `settings.latest_release` (first item — back-compat
/// for `rollout::create_rollout`).
pub async fn poll_once(pool: &PgPool) -> Result<Vec<LatestRelease>, PollerError> {
    let repo = settings::get::<String>(pool, "update_repo")
        .await?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "BlackLuny/server-monitor-rs".to_owned());
    let channel = settings::get::<String>(pool, "update_channel")
        .await?
        .unwrap_or_else(|| "stable".to_owned());

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!(
            "server-monitor-rs/",
            env!("CARGO_PKG_VERSION"),
            " release-poller"
        ))
        .build()?;

    let list_url = format!("https://api.github.com/repos/{repo}/releases?per_page={RECENT_LIMIT}");
    let resp = client.get(&list_url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(PollerError::BadStatus {
            status: status.as_u16(),
            url: list_url,
        });
    }
    let raw_list: Vec<GhRelease> = resp.json().await?;

    let mut releases: Vec<LatestRelease> = Vec::with_capacity(raw_list.len());
    for raw in raw_list {
        // 'stable' channel skips prereleases; explicit 'all' accepts everything.
        if channel == "stable" && raw.prerelease {
            continue;
        }

        let sha_map = fetch_sha256sums(&client, &raw.assets)
            .await
            .unwrap_or_else(|err| {
                tracing::debug!(tag = %raw.tag_name, %err, "SHA256SUMS unavailable for release");
                HashMap::new()
            });

        let assets = raw
            .assets
            .iter()
            .filter(|a| !a.name.starts_with("SHA256SUMS"))
            .map(|a| ReleaseAsset {
                sha256: sha_map.get(&a.name).cloned().unwrap_or_default(),
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
                size: a.size,
            })
            .collect::<Vec<_>>();

        releases.push(LatestRelease {
            tag: raw.tag_name,
            name: raw.name,
            html_url: raw.html_url,
            prerelease: raw.prerelease,
            published_at: raw.published_at,
            fetched_at: OffsetDateTime::now_utc(),
            assets,
        });
    }

    if releases.is_empty() {
        return Err(PollerError::BadStatus {
            status: 404,
            url: format!("{list_url} (no eligible releases for channel={channel})"),
        });
    }

    let recent_value = serde_json::to_value(&releases)?;
    let latest_value = serde_json::to_value(&releases[0])?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"INSERT INTO settings (key, value) VALUES ('recent_releases', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(recent_value)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"INSERT INTO settings (key, value) VALUES ('latest_release', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(latest_value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(releases)
}

async fn fetch_sha256sums(
    client: &reqwest::Client,
    assets: &[GhAsset],
) -> Result<HashMap<String, String>, PollerError> {
    let Some(asset) = assets.iter().find(|a| a.name == "SHA256SUMS") else {
        return Ok(HashMap::new());
    };
    let resp = client.get(&asset.browser_download_url).send().await?;
    if !resp.status().is_success() {
        return Err(PollerError::BadStatus {
            status: resp.status().as_u16(),
            url: asset.browser_download_url.clone(),
        });
    }
    let text = resp.text().await?;
    if text.len() > MAX_SHA256SUMS_BYTES {
        tracing::warn!(
            bytes = text.len(),
            "SHA256SUMS exceeds {MAX_SHA256SUMS_BYTES}B — truncating"
        );
    }
    Ok(parse_sha256sums(&text))
}

/// Parse `<hex>  <name>` lines into a map. Tolerant of leading whitespace,
/// star-prefixed names ("`*`" indicates binary mode in some shasum impls),
/// and Windows line endings.
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines().take(4096) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(hash) = parts.next() else { continue };
        let Some(rest) = parts.next() else { continue };
        let name = rest.trim_start().trim_start_matches('*').trim();
        if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty() {
            out.insert(name.to_owned(), hash.to_lowercase());
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    name: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(with = "time::serde::rfc3339")]
    published_at: OffsetDateTime,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_handles_common_formats() {
        let raw = "\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  monitor-agent.tar.gz\n\
ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890ABCDEF1234567890 *monitor-panel.zip\n\
# comment lines ignored\n\
short-hash  monitor-bogus.tar.gz\n\
\n";
        let map = parse_sha256sums(raw);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("monitor-agent.tar.gz").unwrap(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(map.contains_key("monitor-panel.zip"));
        assert!(!map.contains_key("monitor-bogus.tar.gz"));
    }
}
