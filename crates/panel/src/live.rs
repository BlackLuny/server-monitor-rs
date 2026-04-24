//! Live metrics fan-out bus.
//!
//! Agents push `MetricSnapshot`s through the gRPC Stream RPC; for each one we
//! persist the row (see [`crate::metrics::ingest_batch`]) **and** publish a
//! compact summary here. WebSocket subscribers (see [`crate::api::ws`]) relay
//! that to the browser so dashboards update within the heartbeat cadence.

use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::broadcast;

/// Max in-flight updates a slow subscriber may fall behind before we drop
/// older ones. Real-time data — stale points are cheap to skip.
const BROADCAST_BUFFER: usize = 256;

/// A condensed metric snapshot suitable for pushing to the UI. Mirrors the
/// fields the dashboard card renders; detail (per-core, per-disk, per-iface)
/// is only fetched on demand when a detail page is open.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename = "metric")]
pub struct LiveUpdate {
    pub server_id: i64,
    /// Panel-side visibility flag; WS handlers drop this row when serving
    /// anonymous guests and the server is private.
    pub hidden_from_guest: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub cpu_pct: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub net_in_bps: i64,
    pub net_out_bps: i64,
    pub load_1: f64,
}

/// Shared broadcast handle. Cloneable; each WS connection subscribes once and
/// receives every subsequent update until it disconnects.
#[derive(Clone)]
pub struct LiveBus {
    tx: broadcast::Sender<LiveUpdate>,
}

impl Default for LiveBus {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveBus {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BROADCAST_BUFFER);
        Self { tx }
    }

    /// Publish a new update. Silently drops if no subscribers — broadcast
    /// channels surface that as `Err` on `send`, which is fine to ignore.
    pub fn publish(&self, update: LiveUpdate) {
        let _ = self.tx.send(update);
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LiveUpdate> {
        self.tx.subscribe()
    }
}
