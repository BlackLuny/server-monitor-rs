//! One pty session — owns the child process, master pty, optional asciinema
//! recording, and the output coalescer.
//!
//! Three I/O fronts are juggled cooperatively:
//!   - **stdin**: bytes from the panel (`TerminalInput`) get written to the
//!     pty master in a blocking thread (portable-pty's writer is sync).
//!   - **stdout**: a blocking reader thread drains the master and pushes
//!     chunks into a tokio channel; the main task batches them under a
//!     16 ms / 32 KiB cap before emitting `TerminalOutput`.
//!   - **control**: resize / close arrive on small channels.
//!
//! Recording: when `Config.record` is true we write an [asciinema v2
//! cast](https://docs.asciinema.org/manual/asciicast/v2/) — header line +
//! one JSON line per output chunk. On clean shutdown we hash and report
//! `recording_path / size / sha256` back via `TerminalClosed` so the panel
//! can stash the metadata for later playback.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use monitor_proto::v1::{
    agent_to_panel::Payload as UpPayload, AgentToPanel, TerminalClosed, TerminalOutput,
};
use portable_pty::{CommandBuilder, PtySize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::Upstream;

/// Coalesce upstream writes — at most one TerminalOutput per ~16 ms or
/// 32 KiB, whichever fires first. Keeps paste-of-large-files from drowning
/// the gRPC stream.
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const FLUSH_BYTES: usize = 32 * 1024;

pub struct Config {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
    pub shell: Option<String>,
    pub record: bool,
    pub recording_dir: PathBuf,
}

pub async fn run(
    cfg: Config,
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u32, u32)>,
    mut close_rx: mpsc::Receiver<()>,
    upstream: Upstream,
) {
    let session_id = cfg.session_id.clone();
    let result = spawn_pty(&cfg);
    let pty = match result {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(%session_id, %err, "failed to spawn pty");
            send_closed(
                &upstream,
                TerminalClosed {
                    session_id,
                    exit_code: -1,
                    error: format!("spawn failed: {err}"),
                    recording_path: String::new(),
                    recording_size: 0,
                    recording_sha256: String::new(),
                },
            )
            .await;
            return;
        }
    };

    let SpawnedPty {
        master,
        mut child,
        mut writer,
        reader_rx,
        reader_handle,
    } = pty;

    let (mut recording, recording_path) = if cfg.record {
        match prepare_recording(
            &cfg.recording_dir,
            &cfg.session_id,
            cfg.cols,
            cfg.rows,
            cfg.shell.as_deref(),
        ) {
            Ok(rec) => {
                let path = rec.path.clone();
                (Some(rec), Some(path))
            }
            Err(err) => {
                tracing::warn!(%session_id, %err, "recording disabled — open .cast failed");
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let recording_started = Instant::now();

    let mut buf: VecDeque<u8> = VecDeque::with_capacity(FLUSH_BYTES * 2);
    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut reader_rx = reader_rx;
    let mut explicit_close = false;
    let mut io_error: Option<String> = None;
    // Wrap the reader stream end so the event loop doesn't spin once the
    // child closes the pty.
    let mut eof = false;

    loop {
        tokio::select! {
            chunk = reader_rx.recv(), if !eof => {
                match chunk {
                    Some(bytes) if !bytes.is_empty() => {
                        if let Some(rec) = recording.as_mut() {
                            rec.write_event(recording_started.elapsed(), &bytes);
                        }
                        buf.extend(bytes);
                        if buf.len() >= FLUSH_BYTES {
                            flush_buf(&mut buf, &session_id, &upstream).await;
                        }
                    }
                    Some(_) => {} // empty chunk — ignore
                    None => {
                        eof = true;
                    }
                }
            }
            data = stdin_rx.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(err) = writer.write_all(&bytes) {
                            tracing::debug!(%session_id, %err, "pty write failed");
                            io_error = Some(err.to_string());
                            break;
                        }
                        let _ = writer.flush();
                    }
                    None => break,
                }
            }
            resize = resize_rx.recv() => {
                if let Some((cols, rows)) = resize {
                    let size = PtySize {
                        rows: rows.min(u32::from(u16::MAX)) as u16,
                        cols: cols.min(u32::from(u16::MAX)) as u16,
                        pixel_width: 0,
                        pixel_height: 0,
                    };
                    if let Err(err) = master.resize(size) {
                        tracing::debug!(%session_id, %err, "pty resize failed");
                    }
                }
            }
            _ = close_rx.recv() => {
                explicit_close = true;
                let _ = child.kill();
                break;
            }
            _ = flush_timer.tick() => {
                flush_buf(&mut buf, &session_id, &upstream).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(200)), if eof => {
                // Reader is gone — give the child a moment to exit, then
                // break so we can fetch the exit code.
                break;
            }
        }
    }

    // Final drain so the panel sees the last bytes (e.g. the shell's
    // farewell `exit\n` line).
    flush_buf(&mut buf, &session_id, &upstream).await;

    // Reap. Don't block forever — if the child won't die, surface that.
    let exit_code = match wait_with_timeout(&mut child, Duration::from_millis(500)).await {
        Some(code) => code,
        None => {
            let _ = child.kill();
            -1
        }
    };

    // Best-effort: stop the reader thread.
    drop(reader_rx);
    drop(writer);
    drop(master);
    let _ = reader_handle.await;

    let (rec_path_str, rec_size, rec_sha) = match (recording.take(), recording_path) {
        (Some(rec), Some(path)) => match rec.finalize() {
            Ok((size, sha)) => (path.display().to_string(), size as i64, sha),
            Err(err) => {
                tracing::warn!(%session_id, %err, "recording finalize failed");
                (String::new(), 0, String::new())
            }
        },
        _ => (String::new(), 0, String::new()),
    };

    send_closed(
        &upstream,
        TerminalClosed {
            session_id,
            exit_code,
            error: io_error.unwrap_or_else(|| {
                if explicit_close {
                    "closed by panel".into()
                } else {
                    String::new()
                }
            }),
            recording_path: rec_path_str,
            recording_size: rec_size,
            recording_sha256: rec_sha,
        },
    )
    .await;
}

struct SpawnedPty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader_rx: mpsc::Receiver<Vec<u8>>,
    reader_handle: tokio::task::JoinHandle<()>,
}

fn spawn_pty(cfg: &Config) -> Result<SpawnedPty, std::io::Error> {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: cfg.rows.max(1),
            cols: cfg.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let cmd = build_command(cfg.shell.as_deref());
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    drop(pair.slave); // child has its own end now

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // The pty master read API is blocking. Push it to a dedicated thread so
    // it doesn't park a tokio worker. A bounded channel adds backpressure
    // when the panel-side flush stalls.
    let (chunk_tx, chunk_rx) = mpsc::channel::<Vec<u8>>(64);
    let reader_handle = tokio::task::spawn_blocking(move || {
        let mut tmp = [0u8; 4096];
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if chunk_tx.blocking_send(tmp[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(SpawnedPty {
        master: pair.master,
        child,
        writer,
        reader_rx: chunk_rx,
        reader_handle,
    })
}

fn build_command(shell_override: Option<&str>) -> CommandBuilder {
    let shell_cmd = shell_override
        .map(str::to_owned)
        .or_else(default_shell)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                "cmd.exe".to_owned()
            }
            #[cfg(not(windows))]
            {
                "/bin/sh".to_owned()
            }
        });
    let mut cmd = CommandBuilder::new(shell_cmd);
    // Inherit a sane locale so curses apps render correctly on remote
    // sessions. Specific overrides win because the supervisor / systemd unit
    // can preset them in the environment.
    if std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    if let Some(home) = std::env::var_os("HOME") {
        cmd.cwd(home);
    }
    // Forward the agent's PATH / HOME / LANG so the shell behaves like a
    // login session would.
    for key in ["PATH", "HOME", "LANG", "USER", "LOGNAME"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd
}

#[cfg(unix)]
fn default_shell() -> Option<String> {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty())
}

#[cfg(not(unix))]
fn default_shell() -> Option<String> {
    None
}

async fn flush_buf(buf: &mut VecDeque<u8>, session_id: &str, upstream: &Upstream) {
    if buf.is_empty() {
        return;
    }
    let bytes: Vec<u8> = buf.drain(..).collect();
    let msg = AgentToPanel {
        seq: 0,
        payload: Some(UpPayload::TerminalOutput(TerminalOutput {
            session_id: session_id.to_owned(),
            data: bytes,
        })),
    };
    if upstream.send(msg).await.is_err() {
        tracing::debug!(%session_id, "upstream closed during terminal flush");
    }
}

async fn send_closed(upstream: &Upstream, closed: TerminalClosed) {
    let _ = upstream
        .send(AgentToPanel {
            seq: 0,
            payload: Some(UpPayload::TerminalClosed(closed)),
        })
        .await;
}

async fn wait_with_timeout(
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    budget: Duration,
) -> Option<i32> {
    let start = Instant::now();
    while start.elapsed() < budget {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(status_to_code(status));
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => return None,
        }
    }
    None
}

fn status_to_code(status: portable_pty::ExitStatus) -> i32 {
    if status.success() {
        0
    } else {
        i32::try_from(status.exit_code()).unwrap_or(-1)
    }
}

// ----------------------------------------------------------------------------
// Asciinema v2 recording
// ----------------------------------------------------------------------------

struct Recorder {
    file: std::fs::File,
    path: PathBuf,
    bytes_written: u64,
    hasher: Sha256,
}

impl Recorder {
    fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .read(true)
            .open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            bytes_written: 0,
            hasher: Sha256::new(),
        })
    }

    fn write_header(&mut self, cols: u16, rows: u16, shell: Option<&str>) -> std::io::Result<()> {
        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "env": {
                "SHELL": shell.unwrap_or(""),
                "TERM": "xterm-256color",
            },
        });
        let line = format!("{header}\n");
        self.write_raw(line.as_bytes())
    }

    fn write_event(&mut self, since_start: Duration, data: &[u8]) {
        // asciinema events are JSON arrays: [seconds, "o", string-data].
        // Use a lossy UTF-8 conversion so binary control sequences still
        // record (they round-trip as replacement chars, which is fine for
        // playback fidelity in practice).
        let secs = since_start.as_secs_f64();
        let payload = String::from_utf8_lossy(data);
        let line = match serde_json::to_string(&serde_json::json!([secs, "o", payload])) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut full = line.into_bytes();
        full.push(b'\n');
        if let Err(err) = self.write_raw(&full) {
            tracing::debug!(%err, "recording write failed");
        }
    }

    fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        self.bytes_written += bytes.len() as u64;
        Ok(())
    }

    fn finalize(mut self) -> std::io::Result<(u64, String)> {
        self.file.flush()?;
        let digest = self.hasher.finalize();
        Ok((self.bytes_written, hex(&digest)))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// `prepare_recording` keeps create-dir + open + header-write in one place
// and hands the live recorder to the run loop.
fn prepare_recording(
    dir: &Path,
    session_id: &str,
    cols: u16,
    rows: u16,
    shell: Option<&str>,
) -> std::io::Result<Recorder> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{session_id}.cast"));
    let mut rec = Recorder::open(&path)?;
    rec.write_header(cols, rows, shell)?;
    Ok(rec)
}
