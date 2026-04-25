//! Stream RPC client loop — heartbeats, metric sampling/batching, and reconnect.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use monitor_proto::{
    v1::{
        agent_service_client::AgentServiceClient, agent_to_panel::Payload as UpPayload,
        panel_to_agent::Payload as DownPayload, AgentToPanel, Heartbeat, MetricBatch,
        MetricSnapshot, ProbeBatch, ProbeResult,
    },
    AGENT_VERSION_METADATA, SERVER_TOKEN_METADATA,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::transport::Channel;

use crate::{
    collector::Collector,
    config::AgentConfig,
    probes::Scheduler,
    terminal::{self, Manager as TerminalManager},
};

/// Maximum backoff between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often a new sample is taken from the system.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often buffered samples are flushed to the panel as a `MetricBatch`.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Hard cap on the in-flight buffer so a panel outage can't OOM us.
const BUFFER_CAP: usize = 60;
/// Per-flush limit on probe results forwarded as one ProbeBatch. Tuning this
/// is mostly about latency: smaller batches → quicker visibility on the
/// panel, larger batches → fewer round trips.
const PROBE_FLUSH_MAX: usize = 200;

/// Run the Stream loop until cancellation. Each iteration opens a fresh Stream,
/// runs it to completion (either clean shutdown or error), then backs off and
/// reconnects. Cancellation fires when the shutdown future completes.
pub async fn run(
    channel: Channel,
    cfg: AgentConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let server_token = cfg
        .server_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("stream: server_token missing — need to Register first"))?;

    // Shared across reconnect attempts — keeping the same Collector preserves
    // sysinfo's delta baselines so rate metrics stay stable across sessions.
    let mut collector = Collector::new();
    let mut buffer: VecDeque<MetricSnapshot> = VecDeque::with_capacity(BUFFER_CAP);

    let started = Instant::now();
    let mut backoff = Duration::from_secs(1);
    let mut logged_stable = true;

    loop {
        if *shutdown.borrow() {
            tracing::info!("stream loop shutting down");
            return Ok(());
        }

        tracing::debug!("opening stream");
        match run_once(
            channel.clone(),
            &server_token,
            cfg.heartbeat_interval_s,
            &started,
            &mut collector,
            &mut buffer,
            shutdown.clone(),
        )
        .await
        {
            Ok(()) => {
                tracing::info!("stream closed cleanly, reconnecting");
                backoff = Duration::from_secs(1);
                logged_stable = true;
            }
            Err(err) => {
                if logged_stable {
                    tracing::warn!(%err, "stream failed, will retry");
                    logged_stable = false;
                } else {
                    tracing::debug!(%err, "stream retry still failing");
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            return Ok(());
                        }
                    }
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Open one Stream session. Returns Ok when the panel closes cleanly (end of
/// inbound stream or we receive a shutdown request), Err on transport errors.
#[allow(clippy::too_many_arguments)]
async fn run_once(
    channel: Channel,
    server_token: &str,
    heartbeat_interval_s: u64,
    process_start: &Instant,
    collector: &mut Collector,
    buffer: &mut VecDeque<MetricSnapshot>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), tonic::Status> {
    let mut client = AgentServiceClient::new(channel);

    let (up_tx, up_rx) = mpsc::channel::<AgentToPanel>(32);

    let mut req = tonic::Request::new(ReceiverStream::new(up_rx));
    req.metadata_mut()
        .insert(SERVER_TOKEN_METADATA, server_token.parse().unwrap());
    if let Ok(v) = monitor_common::VERSION.parse() {
        req.metadata_mut().insert(AGENT_VERSION_METADATA, v);
    }

    let response = client.stream(req).await?;
    let mut inbound = response.into_inner();
    tracing::info!("stream connected to panel");

    // Fresh scheduler + result rx for this connection. We don't keep state
    // across reconnects: the panel always re-sends a Sync after we
    // reconnect, and any in-flight results are best-effort.
    let (mut probe_sched, mut probe_rx) = Scheduler::new(512);

    // Terminal sessions are also fresh per connection — if the panel goes
    // away mid-session we kill the child and the user reconnects to a new
    // shell. Avoids ghost ptys outliving their bridge.
    let mut terminals = TerminalManager::new(up_tx.clone(), terminal::default_recording_dir());

    let mut heartbeat_ticker =
        tokio::time::interval(Duration::from_secs(heartbeat_interval_s.max(1)));
    heartbeat_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = heartbeat_ticker.tick().await;

    let mut sample_ticker = tokio::time::interval(SAMPLE_INTERVAL);
    sample_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = sample_ticker.tick().await;

    let mut flush_ticker = tokio::time::interval(FLUSH_INTERVAL);
    flush_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = flush_ticker.tick().await;

    // First heartbeat immediately so the panel's `last_seen_at` is fresh even
    // before the first metric flush (which waits 5s).
    send_heartbeat(&up_tx, process_start, 1).await?;

    let mut seq: u64 = 2;

    loop {
        tokio::select! {
            msg = inbound.next() => {
                match msg {
                    Some(Ok(panel_msg)) => {
                        handle_panel_msg(
                            panel_msg,
                            &mut probe_sched,
                            &mut terminals,
                            &up_tx,
                            &mut seq,
                        );
                    }
                    Some(Err(status)) => {
                        terminals.shutdown_all();
                        return Err(status);
                    }
                    None => {
                        // Best-effort final flush of any buffered samples so
                        // we don't lose the last ~5s when the panel goes away.
                        let _ = flush_metrics(&up_tx, buffer, &mut seq).await;
                        let _ = flush_probes(&up_tx, &mut probe_rx, &mut seq).await;
                        terminals.shutdown_all();
                        return Ok(());
                    }
                }
            }

            _ = heartbeat_ticker.tick() => {
                send_heartbeat(&up_tx, process_start, seq).await?;
                seq += 1;
            }

            _ = sample_ticker.tick() => {
                let snap = collector.sample();
                buffer.push_back(snap);
                if buffer.len() > BUFFER_CAP {
                    // Drop oldest to stay under cap — prefer losing stale data
                    // over OOM'ing during a long outage.
                    buffer.pop_front();
                }
            }

            _ = flush_ticker.tick() => {
                flush_metrics(&up_tx, buffer, &mut seq).await?;
                flush_probes(&up_tx, &mut probe_rx, &mut seq).await?;
            }

            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("shutdown requested, flushing and closing stream");
                    let _ = flush_metrics(&up_tx, buffer, &mut seq).await;
                    let _ = flush_probes(&up_tx, &mut probe_rx, &mut seq).await;
                    terminals.shutdown_all();
                    return Ok(());
                }
            }
        }
    }
}

fn handle_panel_msg(
    msg: monitor_proto::v1::PanelToAgent,
    sched: &mut Scheduler,
    terminals: &mut TerminalManager,
    up_tx: &mpsc::Sender<AgentToPanel>,
    seq: &mut u64,
) {
    let Some(payload) = msg.payload else { return };
    match payload {
        DownPayload::ProbeAssignmentSync(s) => {
            tracing::info!(count = s.probes.len(), "probe assignment sync");
            sched.replace_all(s.probes);
        }
        DownPayload::ProbeAssignmentDelta(d) => {
            tracing::info!(
                added = d.added.len(),
                updated = d.updated.len(),
                removed = d.removed_probe_ids.len(),
                "probe assignment delta"
            );
            sched.apply_delta(d.added, d.updated, d.removed_probe_ids);
        }
        DownPayload::TerminalOpen(open) => terminals.open(open),
        DownPayload::TerminalInput(input) => terminals.input(input),
        DownPayload::TerminalResize(rs) => terminals.resize(rs),
        DownPayload::TerminalClose(c) => terminals.close(c),
        DownPayload::Update(cmd) => {
            // The agent never replaces itself. Forwarding to the supervisor
            // happens on a separate task so the stream loop keeps reading
            // metrics + heartbeats.
            let upstream = up_tx.clone();
            let seq_for_task = *seq;
            *seq = seq.saturating_add(2); // reserve room for status frames
            tokio::spawn(async move {
                let mut local_seq = seq_for_task;
                crate::updates::handle_update(cmd, upstream, &mut local_seq).await;
            });
        }
        DownPayload::UpdateAbort(abort) => {
            // Forward to the supervisor on its own task so the stream loop
            // keeps draining metrics + heartbeats while the cancel + reply
            // round-trip happens.
            let upstream = up_tx.clone();
            let seq_for_task = *seq;
            *seq = seq.saturating_add(1);
            tokio::spawn(async move {
                let mut local_seq = seq_for_task;
                crate::updates::handle_abort(abort, upstream, &mut local_seq).await;
            });
        }
        DownPayload::RecordingFetch(req) => {
            let dir = terminals.recording_dir().to_path_buf();
            let upstream = up_tx.clone();
            // Reserve a generous slice of seq for the streamed chunks. A 256 MiB
            // recording at 64 KiB / chunk fits comfortably below 5000 frames;
            // we round up to be safe.
            let seq_for_task = *seq;
            *seq = seq.saturating_add(8192);
            tokio::spawn(async move {
                crate::recordings::serve_fetch(req.session_id, dir, upstream, seq_for_task).await;
            });
        }
        DownPayload::Ack(_) => {}
    }
}

async fn flush_probes(
    up_tx: &mpsc::Sender<AgentToPanel>,
    rx: &mut mpsc::Receiver<ProbeResult>,
    seq: &mut u64,
) -> Result<(), tonic::Status> {
    let mut results = Vec::new();
    while results.len() < PROBE_FLUSH_MAX {
        match rx.try_recv() {
            Ok(r) => results.push(r),
            Err(_) => break,
        }
    }
    if results.is_empty() {
        return Ok(());
    }
    let msg = AgentToPanel {
        seq: *seq,
        payload: Some(UpPayload::ProbeBatch(ProbeBatch { results })),
    };
    *seq += 1;
    up_tx
        .send(msg)
        .await
        .map_err(|_| tonic::Status::aborted("upstream channel closed"))
}

async fn flush_metrics(
    up_tx: &mpsc::Sender<AgentToPanel>,
    buffer: &mut VecDeque<MetricSnapshot>,
    seq: &mut u64,
) -> Result<(), tonic::Status> {
    if buffer.is_empty() {
        return Ok(());
    }
    let snapshots: Vec<MetricSnapshot> = buffer.drain(..).collect();
    let msg = AgentToPanel {
        seq: *seq,
        payload: Some(UpPayload::MetricsBatch(MetricBatch { snapshots })),
    };
    *seq += 1;
    up_tx
        .send(msg)
        .await
        .map_err(|_| tonic::Status::aborted("upstream channel closed"))
}

async fn send_heartbeat(
    tx: &mpsc::Sender<AgentToPanel>,
    process_start: &Instant,
    seq: u64,
) -> Result<(), tonic::Status> {
    let uptime_s = process_start.elapsed().as_secs();
    let msg = AgentToPanel {
        seq,
        payload: Some(UpPayload::Heartbeat(Heartbeat {
            ts_ms: now_ms(),
            uptime_s,
        })),
    };
    tx.send(msg)
        .await
        .map_err(|_| tonic::Status::aborted("upstream channel closed"))
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}
