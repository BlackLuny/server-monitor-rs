//! Developer task runner — invoked via `cargo xtask <cmd>`.
//!
//! Subcommands:
//!
//!   lint            fmt + clippy (both deny warnings)
//!   test            run the full test suite against a throwaway Postgres
//!   db up|down|reset   manage the throwaway Postgres container
//!   panel           run the panel against the throwaway Postgres
//!   dev             `db up` + `panel` in one step
//!   ci              same checks CI runs
//!   package         cross-build + tar/zip release artefacts (M6 prep for M7)

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

const PG_CONTAINER: &str = "monitor-pg-dev";
const PG_PORT: u16 = 55432;
const PG_USER: &str = "monitor";
const PG_PASSWORD: &str = "monitor";
const PG_DB: &str = "monitor";
const PG_IMAGE: &str = "postgres:16-alpine";

#[derive(Parser)]
#[command(name = "xtask", about = "server-monitor-rs developer tasks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run rustfmt --check and clippy with `-D warnings`.
    Lint,
    /// Run the full test suite against the dev Postgres (spins it up if needed).
    Test,
    /// Manage the throwaway dev Postgres container.
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Run the panel locally, pointing at the dev Postgres.
    Panel,
    /// `db up` + `panel` in one command.
    Dev,
    /// Lint + test — the same set CI enforces.
    Ci,
    /// Install + build the SvelteKit frontend so rust-embed can pick up the
    /// generated `frontend/build/` when compiling the panel.
    FrontendBuild,
    /// Package release artefacts for one or more targets.
    ///
    /// Produces `dist/monitor-{panel,agent,supervisor}-<target>.{tar.gz,zip}`
    /// plus a `dist/SHA256SUMS` covering everything in the directory. M7's
    /// release workflow consumes these directly; locally it lets you produce
    /// drop-in tarballs for `install-agent.sh --local-binary`.
    Package {
        /// One target triple, e.g. `x86_64-unknown-linux-musl`. Repeatable.
        #[arg(long)]
        target: Vec<String>,
        /// Build every supported target instead of picking individually.
        #[arg(long, conflicts_with = "target")]
        all_targets: bool,
        /// Output directory; defaults to `dist/`.
        #[arg(long, default_value = "dist")]
        out_dir: PathBuf,
        /// Skip running `pnpm build` even though the panel is one of the
        /// packaged binaries. Useful when iterating on packaging logic.
        #[arg(long)]
        skip_frontend: bool,
        /// Skip writing a SHA256SUMS file. Release CI aggregates per-runner
        /// archives into one file in a later job and prefers a single sum.
        #[arg(long)]
        no_checksums: bool,
        /// Use plain `cargo build` instead of `cargo zigbuild`. Useful on a
        /// runner that already targets its host (e.g. windows-latest building
        /// `x86_64-pc-windows-msvc`) — saves installing zig there.
        #[arg(long)]
        use_cargo: bool,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Start the dev Postgres container if not running.
    Up,
    /// Stop and remove the dev Postgres container.
    Down,
    /// Drop and recreate the dev database.
    Reset,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Lint => lint(),
        Cmd::Test => test(),
        Cmd::Db { action } => match action {
            DbAction::Up => db_up(),
            DbAction::Down => db_down(),
            DbAction::Reset => db_reset(),
        },
        Cmd::Panel => run_panel(),
        Cmd::Dev => {
            db_up()?;
            run_panel()
        }
        Cmd::Ci => {
            lint()?;
            test()
        }
        Cmd::FrontendBuild => frontend_build(),
        Cmd::Package {
            target,
            all_targets,
            out_dir,
            skip_frontend,
            no_checksums,
            use_cargo,
        } => package(
            target,
            all_targets,
            &out_dir,
            skip_frontend,
            no_checksums,
            use_cargo,
        ),
    }
}

// ---------------------------------------------------------------------------
// Packaging
// ---------------------------------------------------------------------------

/// Per-target packaging blueprint.
struct TargetSpec {
    triple: &'static str,
    /// File extension for the resulting archive.
    archive: &'static str,
    /// Suffix appended to each binary on this platform.
    bin_suffix: &'static str,
    /// Which binaries to ship for this target. Panel is omitted on Windows
    /// because we don't need a Windows panel build today.
    bins: &'static [&'static str],
}

const ALL_TARGETS: &[TargetSpec] = &[
    TargetSpec {
        triple: "x86_64-unknown-linux-musl",
        archive: "tar.gz",
        bin_suffix: "",
        bins: &["monitor-panel", "monitor-agent", "monitor-agent-supervisor"],
    },
    TargetSpec {
        triple: "aarch64-unknown-linux-musl",
        archive: "tar.gz",
        bin_suffix: "",
        bins: &["monitor-panel", "monitor-agent", "monitor-agent-supervisor"],
    },
    TargetSpec {
        triple: "x86_64-apple-darwin",
        archive: "tar.gz",
        bin_suffix: "",
        bins: &["monitor-panel", "monitor-agent", "monitor-agent-supervisor"],
    },
    TargetSpec {
        triple: "aarch64-apple-darwin",
        archive: "tar.gz",
        bin_suffix: "",
        bins: &["monitor-panel", "monitor-agent", "monitor-agent-supervisor"],
    },
    // Windows: agent + supervisor only — the panel's Caddy / sqlx / docker
    // story doesn't have a Windows audience yet.
    TargetSpec {
        triple: "x86_64-pc-windows-msvc",
        archive: "zip",
        bin_suffix: ".exe",
        bins: &["monitor-agent", "monitor-agent-supervisor"],
    },
];

fn package(
    targets: Vec<String>,
    all: bool,
    out_dir: &Path,
    skip_frontend: bool,
    no_checksums: bool,
    use_cargo: bool,
) -> Result<()> {
    let selected: Vec<&'static TargetSpec> = if all {
        ALL_TARGETS.iter().collect()
    } else if targets.is_empty() {
        bail!("pass at least one --target or --all-targets");
    } else {
        targets
            .iter()
            .map(|t| {
                ALL_TARGETS.iter().find(|s| s.triple == t).with_context(|| {
                    format!(
                        "unsupported target: {t} (known: {})",
                        ALL_TARGETS
                            .iter()
                            .map(|s| s.triple)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            })
            .collect::<Result<_>>()?
    };

    std::fs::create_dir_all(out_dir).with_context(|| format!("create {}", out_dir.display()))?;

    // Need the panel? Run pnpm build once up front (rust-embed bakes it in).
    let needs_frontend = !skip_frontend
        && selected
            .iter()
            .any(|s| s.bins.contains(&"monitor-panel"));
    if needs_frontend {
        frontend_build()?;
    }

    for spec in &selected {
        eprintln!("\n=== packaging {} ===", spec.triple);
        cross_build(spec, use_cargo)?;
        archive_target(spec, out_dir)?;
    }

    if !no_checksums {
        write_checksums(out_dir)?;
    }
    eprintln!("\nDone. Artefacts in {}.", out_dir.display());
    Ok(())
}

fn cross_build(spec: &TargetSpec, use_cargo: bool) -> Result<()> {
    // `cargo zigbuild` handles musl + macOS sysroot quirks; on a runner that
    // already targets its host triple natively, plain `cargo build` is
    // simpler and avoids needing zig in the toolchain.
    let subcommand = if use_cargo { "build" } else { "zigbuild" };
    let mut args: Vec<&str> = vec![subcommand, "--release", "--target", spec.triple];
    for bin in spec.bins {
        args.push("-p");
        // every bin is also the package name in this workspace.
        args.push(bin);
    }
    run("cargo", &args)
}

fn archive_target(spec: &TargetSpec, out_dir: &Path) -> Result<()> {
    let release_dir = PathBuf::from("target").join(spec.triple).join("release");
    for bin in spec.bins {
        let bin_path = release_dir.join(format!("{bin}{}", spec.bin_suffix));
        if !bin_path.exists() {
            bail!("expected binary at {}", bin_path.display());
        }

        let archive_name = format!("{bin}-{}.{}", spec.triple, spec.archive);
        let archive_path = out_dir.join(&archive_name);
        eprintln!("  → {}", archive_name);

        // Each archive contains: <bin> + LICENSE-* + README.md.
        // Layout inside the archive: a flat directory `<bin>-<triple>/`.
        let staging = out_dir
            .join("staging")
            .join(format!("{bin}-{}", spec.triple));
        if staging.exists() {
            std::fs::remove_dir_all(&staging).ok();
        }
        std::fs::create_dir_all(&staging)?;

        std::fs::copy(&bin_path, staging.join(format!("{bin}{}", spec.bin_suffix)))?;
        for support in ["README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE"] {
            if Path::new(support).exists() {
                std::fs::copy(support, staging.join(support))?;
            }
        }

        match spec.archive {
            "tar.gz" => {
                run(
                    "tar",
                    &[
                        "-C",
                        &staging.parent().unwrap().display().to_string(),
                        "-czf",
                        &archive_path.display().to_string(),
                        &staging.file_name().unwrap().to_string_lossy(),
                    ],
                )?;
            }
            "zip" => {
                // The host might be macOS — `zip` is stock there. Fall back
                // to `python -m zipfile` if missing.
                let staging_dir = staging.file_name().unwrap().to_string_lossy().into_owned();
                let parent = staging.parent().unwrap().to_path_buf();
                if Command::new("zip").arg("-v").output().is_ok() {
                    let original = std::env::current_dir()?;
                    std::env::set_current_dir(&parent)?;
                    let res = run(
                        "zip",
                        &[
                            "-r",
                            "-q",
                            &archive_path.display().to_string(),
                            &staging_dir,
                        ],
                    );
                    std::env::set_current_dir(original)?;
                    res?;
                } else {
                    run(
                        "python3",
                        &[
                            "-m",
                            "zipfile",
                            "-c",
                            &archive_path.display().to_string(),
                            &staging.display().to_string(),
                        ],
                    )?;
                }
            }
            other => bail!("unsupported archive format: {other}"),
        }
    }

    // Clean staging once all bins for this target are archived.
    let staging_root = out_dir.join("staging");
    if staging_root.exists() {
        std::fs::remove_dir_all(&staging_root).ok();
    }
    Ok(())
}

fn write_checksums(out_dir: &Path) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .with_context(|| format!("readdir {}", out_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with("SHA256SUMS"))
                .unwrap_or(false)
                && p.is_file()
        })
        .collect();
    entries.sort();
    if entries.is_empty() {
        return Ok(());
    }
    let sums_path = out_dir.join("SHA256SUMS");
    let mut out = String::new();
    for path in &entries {
        let bytes = std::fs::read(path)?;
        let digest = sha256(&bytes);
        let name = path.file_name().unwrap().to_string_lossy();
        out.push_str(&format!("{digest}  {name}\n"));
    }
    std::fs::write(&sums_path, out)?;
    eprintln!("  → SHA256SUMS");
    Ok(())
}

// SHA-256 in 80 lines using only std — saves a workspace dep just for xtask.
fn sha256(input: &[u8]) -> String {
    use std::convert::TryInto;
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded: Vec<u8> = input.to_vec();
    let bit_len = (input.len() as u64) * 8;
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, slot) in w.iter_mut().enumerate().take(16) {
            *slot = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

fn frontend_build() -> Result<()> {
    let frontend = std::path::Path::new("frontend");
    if !frontend.exists() {
        bail!("expected `frontend/` directory at the workspace root");
    }
    // Honor CI semantics: `--frozen-lockfile` so a drifting lockfile fails
    // the build loudly rather than silently applying updates.
    let status = Command::new("pnpm")
        .args(["install", "--frozen-lockfile"])
        .current_dir(frontend)
        .status()
        .context("pnpm install")?;
    expect_success(status, "pnpm install")?;
    let status = Command::new("pnpm")
        .args(["build"])
        .current_dir(frontend)
        .status()
        .context("pnpm build")?;
    expect_success(status, "pnpm build")
}

fn lint() -> Result<()> {
    run("cargo", &["fmt", "--all", "--", "--check"])?;
    run(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn test() -> Result<()> {
    db_up()?;
    let url = dev_db_url();
    let status = Command::new("cargo")
        .args(["test", "--workspace", "--all-targets"])
        .env("TEST_DATABASE_URL", &url)
        .status()
        .context("cargo test")?;
    expect_success(status, "cargo test")
}

fn db_up() -> Result<()> {
    if container_running(PG_CONTAINER)? {
        return Ok(());
    }
    // Remove a stopped container with the same name (happens after `db down`).
    let _ = Command::new("docker")
        .args(["rm", "-f", PG_CONTAINER])
        .output();

    run(
        "docker",
        &[
            "run",
            "-d",
            "--rm",
            "--name",
            PG_CONTAINER,
            "-e",
            &format!("POSTGRES_USER={PG_USER}"),
            "-e",
            &format!("POSTGRES_PASSWORD={PG_PASSWORD}"),
            "-e",
            &format!("POSTGRES_DB={PG_DB}"),
            "-p",
            &format!("{PG_PORT}:5432"),
            PG_IMAGE,
        ],
    )?;
    wait_for_postgres()
}

fn db_down() -> Result<()> {
    if !container_running(PG_CONTAINER)? {
        return Ok(());
    }
    run("docker", &["rm", "-f", PG_CONTAINER])
}

fn db_reset() -> Result<()> {
    db_up()?;
    let sql = format!(
        "DROP SCHEMA public CASCADE; CREATE SCHEMA public; GRANT ALL ON SCHEMA public TO {PG_USER};"
    );
    run(
        "docker",
        &[
            "exec",
            PG_CONTAINER,
            "psql",
            "-U",
            PG_USER,
            "-d",
            PG_DB,
            "-c",
            &sql,
        ],
    )
}

fn run_panel() -> Result<()> {
    db_up()?;
    let status = Command::new("cargo")
        .args(["run", "--bin", "monitor-panel"])
        .env("MONITOR_DATABASE__URL", dev_db_url())
        .env(
            "MONITOR_JWT__SECRET",
            "dev-only-secret-please-change-0123456789abcdef",
        )
        .env("MONITOR_LOG__FILTER", "info,sqlx=warn")
        .status()
        .context("cargo run --bin monitor-panel")?;
    expect_success(status, "monitor-panel")
}

fn container_running(name: &str) -> Result<bool> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name=^{name}$"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .context("docker ps")?;
    let names = String::from_utf8_lossy(&out.stdout);
    Ok(names.lines().any(|l| l.trim() == name))
}

fn wait_for_postgres() -> Result<()> {
    for _ in 0..30 {
        let status = Command::new("docker")
            .args([
                "exec",
                PG_CONTAINER,
                "pg_isready",
                "-U",
                PG_USER,
                "-d",
                PG_DB,
            ])
            .status();
        if let Ok(s) = status {
            if s.success() {
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    bail!("postgres container didn't become ready in 30s");
}

fn dev_db_url() -> String {
    format!("postgres://{PG_USER}:{PG_PASSWORD}@127.0.0.1:{PG_PORT}/{PG_DB}")
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    eprintln!("+ {cmd} {}", args.join(" "));
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("running {cmd}"))?;
    expect_success(status, cmd)
}

fn expect_success(status: ExitStatus, cmd: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{cmd} failed with {status}");
    }
}
