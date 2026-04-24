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
    }
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
