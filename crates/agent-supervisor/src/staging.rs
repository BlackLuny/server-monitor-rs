//! Download + verify + extract a release archive into `versions/<v>/`.
//!
//! Behavioural contract:
//!   - `https://` URL only — refuses anything else.
//!   - Streams to a temp file inside `versions/<v>.partial/` and computes
//!     a sha256 incrementally. Mismatched hash → wipe the partial dir.
//!   - Extracts `.tar.gz` (linux/macOS) or `.zip` (windows) into the
//!     final `versions/<v>/` directory atomically (rename from partial).
//!   - Returns the path to the new agent binary on success.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    #[error("only https URLs are accepted (got {0})")]
    InsecureUrl(String),
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream returned status {0}")]
    BadStatus(u16),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("archive: {0}")]
    Archive(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("aborted")]
    Cancelled,
    #[error("attestation: {0}")]
    Attestation(String),
}

pub struct Staged {
    /// Final directory at `versions/<version>/`. Kept on the struct so
    /// callers + tests can inspect the layout — the supervisor reads
    /// `agent_binary` directly during the swap.
    #[allow(dead_code)]
    pub version_dir: PathBuf,
    /// Path to the new agent binary inside that directory.
    pub agent_binary: PathBuf,
}

pub async fn stage(
    versions_root: &Path,
    version: &str,
    asset_url: &str,
    expected_sha256: &str,
) -> Result<Staged, StagingError> {
    stage_cancellable(versions_root, version, asset_url, expected_sha256, "", None).await
}

/// Like [`stage`], but accepts a oneshot the caller fires to abort the
/// in-flight download. Cancellation is best-effort: the partial dir is
/// cleaned up; the caller decides whether to retry.
///
/// `attestation_repo`: when non-empty, the supervisor runs
/// `gh attestation verify <archive> --repo <repo>` after the sha256 check.
/// Empty disables attestation verification (the default for backward
/// compat with installs that haven't enabled it).
pub async fn stage_cancellable(
    versions_root: &Path,
    version: &str,
    asset_url: &str,
    expected_sha256: &str,
    attestation_repo: &str,
    cancel: Option<oneshot::Receiver<()>>,
) -> Result<Staged, StagingError> {
    if !asset_url.starts_with("https://") {
        return Err(StagingError::InsecureUrl(asset_url.to_owned()));
    }

    let version_dir = versions_root.join(version);
    let partial_dir = versions_root.join(format!("{version}.partial"));
    let _ = std::fs::remove_dir_all(&partial_dir);
    std::fs::create_dir_all(&partial_dir)?;

    let archive_name = asset_url.rsplit('/').next().unwrap_or("archive");
    let archive_path = partial_dir.join(archive_name);

    let download = download_and_hash(asset_url, &archive_path, expected_sha256, cancel).await;
    if let Err(err) = download {
        let _ = std::fs::remove_dir_all(&partial_dir);
        return Err(err);
    }

    if !attestation_repo.is_empty() {
        if let Err(err) = verify_attestation(&archive_path, attestation_repo).await {
            let _ = std::fs::remove_dir_all(&partial_dir);
            return Err(err);
        }
    }
    extract_archive(&archive_path, &partial_dir)?;
    // The archive packed by `xtask package` contains a single top-level
    // directory `monitor-agent-<triple>/`. Find it.
    let inner = first_subdir(&partial_dir)?;
    let agent_in_inner = locate_agent_binary(&inner)?;

    if version_dir.exists() {
        // Rare — typically the orchestrator wouldn't ask twice — but if
        // somebody re-stages we replace cleanly.
        std::fs::remove_dir_all(&version_dir)?;
    }
    std::fs::rename(&inner, &version_dir)?;
    let _ = std::fs::remove_dir_all(&partial_dir);

    let agent_path = version_dir.join(
        agent_in_inner
            .file_name()
            .ok_or_else(|| StagingError::Archive("agent binary path empty".into()))?,
    );
    if !agent_path.exists() {
        return Err(StagingError::Archive(format!(
            "agent binary missing after extract: {}",
            agent_path.display()
        )));
    }
    set_executable(&agent_path)?;
    Ok(Staged {
        version_dir,
        agent_binary: agent_path,
    })
}

async fn download_and_hash(
    url: &str,
    out: &Path,
    expected: &str,
    cancel: Option<oneshot::Receiver<()>>,
) -> Result<(), StagingError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    // Wrap the download in a select arm so an Abort can interrupt it. We
    // bind the cancel receiver to a future that resolves once when triggered;
    // when the caller passes None we substitute a future that never resolves.
    let cancel_fut = async move {
        match cancel {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancel_fut);

    let resp = tokio::select! {
        biased;
        () = &mut cancel_fut => return Err(StagingError::Cancelled),
        res = client.get(url).send() => res?,
    };
    if !resp.status().is_success() {
        return Err(StagingError::BadStatus(resp.status().as_u16()));
    }
    let bytes = tokio::select! {
        biased;
        () = &mut cancel_fut => return Err(StagingError::Cancelled),
        res = resp.bytes() => res?,
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(StagingError::Checksum {
            expected: expected.to_owned(),
            actual,
        });
    }
    let mut f = std::fs::File::create(out)?;
    f.write_all(&bytes)?;
    Ok(())
}

fn extract_archive(archive: &Path, dest: &Path) -> Result<(), StagingError> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StagingError::Archive("archive path missing".into()))?;
    if name.ends_with(".tar.gz") {
        let f = std::fs::File::open(archive)?;
        let dec = flate2::read::GzDecoder::new(f);
        let mut t = tar::Archive::new(dec);
        t.unpack(dest)
            .map_err(|e| StagingError::Archive(format!("tar: {e}")))?;
        Ok(())
    } else if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z =
            zip::ZipArchive::new(f).map_err(|e| StagingError::Archive(format!("zip: {e}")))?;
        for i in 0..z.len() {
            let mut entry = z
                .by_index(i)
                .map_err(|e| StagingError::Archive(format!("zip entry: {e}")))?;
            let path = match entry.enclosed_name() {
                Some(p) => dest.join(p),
                None => continue,
            };
            if entry.is_dir() {
                std::fs::create_dir_all(&path)?;
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = std::fs::File::create(&path)?;
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(StagingError::Io)?;
                out.write_all(&buf)?;
            }
        }
        Ok(())
    } else {
        Err(StagingError::Archive(format!(
            "unsupported archive extension: {name}"
        )))
    }
}

fn first_subdir(dir: &Path) -> Result<PathBuf, StagingError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            return Ok(entry.path());
        }
    }
    Err(StagingError::Archive(
        "archive contained no directory entries".into(),
    ))
}

fn locate_agent_binary(dir: &Path) -> Result<PathBuf, StagingError> {
    let candidates = if cfg!(windows) {
        vec!["monitor-agent.exe", "agent.exe"]
    } else {
        vec!["monitor-agent", "agent"]
    };
    for c in candidates {
        let p = dir.join(c);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(StagingError::Archive(format!(
        "no monitor-agent binary in {}",
        dir.display()
    )))
}

/// Verify a release archive's Sigstore attestation by shelling out to the
/// `gh` CLI. Hosts that opt in (via `settings.attestation_required = true`)
/// must have `gh` on PATH; we deliberately fail closed rather than skip
/// silently — the whole point of opting in is that an unverified swap is a
/// bug, not a warning.
async fn verify_attestation(archive: &Path, repo: &str) -> Result<(), StagingError> {
    let output = tokio::process::Command::new("gh")
        .arg("attestation")
        .arg("verify")
        .arg(archive)
        .arg("--repo")
        .arg(repo)
        .output()
        .await
        .map_err(|err| {
            StagingError::Attestation(format!(
                "failed to invoke `gh`: {err}; install GitHub CLI 2.49+ or unset attestation_required"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StagingError::Attestation(format!(
            "`gh attestation verify` failed: {}",
            stderr.trim()
        )));
    }
    tracing::info!(archive = %archive.display(), repo, "attestation verified");
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), StagingError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), StagingError> {
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
