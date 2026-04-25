//! Network probe workers + scheduler.
//!
//! The agent receives a `ProbeAssignmentSync` (or running `Delta`) over the
//! gRPC stream and runs every active probe on its own tokio interval. Each
//! tick produces a single `ProbeResult` which the scheduler funnels into a
//! shared mpsc channel; the stream layer batches and flushes those upstream
//! every few seconds.
//!
//! Each probe kind owns its execution module (icmp / tcp / http) so they can
//! be tested + tweaked independently. They share a single `ProbeOutcome`
//! shape so the scheduler doesn't have to know which kind ran.

pub mod http;
pub mod icmp;
pub mod scheduler;
pub mod tcp;

use std::time::Duration;

use monitor_proto::v1::Probe;

pub use scheduler::{ResultsRx, Scheduler};

/// Result of one probe attempt, kind-agnostic.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub latency: Duration,
    pub status_code: Option<u32>,
    pub error: Option<String>,
}

impl ProbeOutcome {
    pub fn success(latency: Duration) -> Self {
        Self {
            ok: true,
            latency,
            status_code: None,
            error: None,
        }
    }
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            latency: Duration::ZERO,
            status_code: None,
            error: Some(error.into()),
        }
    }
    pub fn with_status(mut self, code: u32) -> Self {
        self.status_code = Some(code);
        self
    }
}

/// Dispatch one probe execution. Workers must respect `probe.timeout_ms`
/// internally — the scheduler does *not* wrap them in an outer timeout
/// because the probe libraries already understand their own units (e.g.
/// reqwest does header timeouts, surge-ping has its own).
pub async fn execute(probe: &Probe) -> ProbeOutcome {
    let timeout = Duration::from_millis(u64::from(probe.timeout_ms.max(100)));
    match monitor_proto::v1::ProbeType::try_from(probe.r#type).unwrap_or_default() {
        monitor_proto::v1::ProbeType::Icmp => icmp::run(probe, timeout).await,
        monitor_proto::v1::ProbeType::Tcp => tcp::run(probe, timeout).await,
        monitor_proto::v1::ProbeType::Http => http::run(probe, timeout).await,
        monitor_proto::v1::ProbeType::Unspecified => {
            ProbeOutcome::failure("probe type unspecified")
        }
    }
}
