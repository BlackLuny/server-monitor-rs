//! Stream RPC client loop — heartbeats, metric sampling/batching, and reconnect.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use monitor_proto::{
    v1::{
        agent_service_client::AgentServiceClient, agent_to_panel::Payload as UpPayload,
        AgentToPanel, Heartbeat, MetricBatch, MetricSnapshot,
    },
    SERVER_TOKEN_METADATA,
};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::transport::Channel;

use crate::{collector::Collector, config::AgentConfig};

/// Maximum backoff between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often a new sample is taken from the system.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often buffered samples are flushed to the panel as a `MetricBatch`.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Hard cap on the in-flight buffer so a panel outage can't OOM us.
const BUFFER_CAP: usize = 60;

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

    let response = client.stream(req).await?;
    let mut inbound = response.into_inner();
    tracing::info!("stream connected to panel");

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
                        tracing::debug!(?panel_msg, "panel → agent");
                    }
                    Some(Err(status)) => return Err(status),
                    None => {
                        // Best-effort final flush of any buffered samples so
                        // we don't lose the last ~5s when the panel goes away.
                        let _ = flush_metrics(&up_tx, buffer, &mut seq).await;
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
            }

            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("shutdown requested, flushing and closing stream");
                    let _ = flush_metrics(&up_tx, buffer, &mut seq).await;
                    return Ok(());
                }
            }
        }
    }
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
