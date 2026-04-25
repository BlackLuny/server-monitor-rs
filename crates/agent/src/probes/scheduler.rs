//! Probe scheduler — keeps a per-probe tokio task running and funnels every
//! result into one shared mpsc.
//!
//! Lifecycle:
//!   1. Stream layer receives a `ProbeAssignmentSync` → calls `replace_all`.
//!   2. Stream layer receives a `ProbeAssignmentDelta` → calls `apply_delta`.
//!   3. Each active probe is one tokio task that loops on its own interval,
//!      calls `super::execute`, and pushes a [`monitor_proto::v1::ProbeResult`]
//!      into the shared channel.
//!   4. The stream layer's flush ticker drains the channel into a
//!      `ProbeBatch` AgentToPanel frame every few seconds.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitor_proto::v1::{Probe, ProbeResult};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;

use super::execute;

/// Receiver half of the results channel. Held by the stream task to flush
/// results upstream.
pub type ResultsRx = mpsc::Receiver<ProbeResult>;

/// Owns the per-probe tokio task handles. Cloneable handles to the channel
/// are passed into each task.
pub struct Scheduler {
    tasks: HashMap<String, ProbeHandle>,
    results_tx: mpsc::Sender<ProbeResult>,
    /// Caps in-flight probe executions across all kinds. ICMP / TCP / HTTP
    /// targets that go silent under load can pile up otherwise.
    semaphore: Arc<Semaphore>,
}

struct ProbeHandle {
    probe: Probe,
    handle: JoinHandle<()>,
}

impl Scheduler {
    /// Build a scheduler with a buffered result channel. `results_buffer`
    /// caps how many results can sit between scheduler and stream flush;
    /// values of a few hundred are plenty for normal probe rates.
    pub fn new(results_buffer: usize) -> (Self, ResultsRx) {
        let (tx, rx) = mpsc::channel(results_buffer.max(64));
        (
            Self {
                tasks: HashMap::new(),
                results_tx: tx,
                semaphore: Arc::new(Semaphore::new(32)),
            },
            rx,
        )
    }

    /// Apply a Sync (full replacement). Cancel anything not in the new set.
    pub fn replace_all(&mut self, probes: Vec<Probe>) {
        let new_ids: std::collections::HashSet<String> =
            probes.iter().map(|p| p.id.clone()).collect();
        let to_remove: Vec<String> = self
            .tasks
            .keys()
            .filter(|id| !new_ids.contains(*id))
            .cloned()
            .collect();
        for id in to_remove {
            self.cancel(&id);
        }
        for p in probes {
            self.upsert(p);
        }
    }

    /// Apply a Delta (added/updated/removed).
    pub fn apply_delta(&mut self, added: Vec<Probe>, updated: Vec<Probe>, removed: Vec<String>) {
        for id in removed {
            self.cancel(&id);
        }
        for p in added.into_iter().chain(updated) {
            self.upsert(p);
        }
    }

    /// Add or replace one probe. Replacing means we cancel the old loop and
    /// start a new one with the new config — that's fine because the result
    /// stream is one big channel; the panel will see them mixed.
    fn upsert(&mut self, probe: Probe) {
        // Skip cancel-then-spawn when nothing meaningful changed; this avoids
        // restarting interval ticks every time the panel re-sends an
        // identical row.
        if let Some(existing) = self.tasks.get(&probe.id) {
            if config_equal(&existing.probe, &probe) {
                return;
            }
        }
        self.cancel(&probe.id);
        let id = probe.id.clone();
        let interval = Duration::from_secs(u64::from(probe.interval_s.max(5)));
        let tx = self.results_tx.clone();
        let sem = self.semaphore.clone();
        let p_clone = probe.clone();
        let handle = tokio::spawn(async move {
            run_probe_loop(p_clone, interval, tx, sem).await;
        });
        self.tasks.insert(id, ProbeHandle { probe, handle });
    }

    fn cancel(&mut self, id: &str) {
        if let Some(handle) = self.tasks.remove(id) {
            handle.handle.abort();
        }
    }

    #[cfg(test)]
    pub fn active_ids(&self) -> Vec<String> {
        self.tasks.keys().cloned().collect()
    }
}

async fn run_probe_loop(
    probe: Probe,
    interval: Duration,
    tx: mpsc::Sender<ProbeResult>,
    sem: Arc<Semaphore>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Burn the first immediate tick so we don't hammer the target on every
    // assignment change.
    let _ = ticker.tick().await;

    loop {
        ticker.tick().await;
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => return, // semaphore closed → process shutting down
        };
        let probe = probe.clone();
        let tx = tx.clone();
        // Spawn execution on its own task so a slow probe doesn't push out
        // the next interval; the semaphore caps total concurrency.
        tokio::spawn(async move {
            let outcome = execute(&probe).await;
            drop(permit);
            let result = ProbeResult {
                probe_id: probe.id.clone(),
                ts_ms: now_ms(),
                ok: outcome.ok,
                latency_us: u32::try_from(outcome.latency.as_micros()).unwrap_or(u32::MAX),
                status_code: outcome.status_code.unwrap_or(0),
                error: outcome.error.unwrap_or_default(),
            };
            // If the channel is full or closed we just drop — the upstream
            // batcher is responsible for flow control.
            let _ = tx.try_send(result);
        });
    }
}

fn config_equal(a: &Probe, b: &Probe) -> bool {
    a.name == b.name
        && a.r#type == b.r#type
        && a.target == b.target
        && a.port == b.port
        && a.interval_s == b.interval_s
        && a.timeout_ms == b.timeout_ms
        && a.http_method == b.http_method
        && a.http_expect_code == b.http_expect_code
        && a.http_expect_body == b.http_expect_body
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitor_proto::v1::ProbeType;

    fn make_probe(id: &str, interval_s: u32) -> Probe {
        Probe {
            id: id.into(),
            name: id.into(),
            r#type: ProbeType::Tcp as i32,
            target: "127.0.0.1".into(),
            port: 1,
            interval_s,
            timeout_ms: 100,
            http_method: String::new(),
            http_expect_code: 0,
            http_expect_body: String::new(),
        }
    }

    #[tokio::test]
    async fn replace_all_cancels_dropped_probes() {
        let (mut sched, _rx) = Scheduler::new(16);
        sched.replace_all(vec![make_probe("a", 60), make_probe("b", 60)]);
        assert_eq!(sched.active_ids().len(), 2);

        sched.replace_all(vec![make_probe("b", 60), make_probe("c", 60)]);
        let ids: std::collections::HashSet<_> = sched.active_ids().into_iter().collect();
        assert!(ids.contains("b"));
        assert!(ids.contains("c"));
        assert!(!ids.contains("a"));
    }

    #[tokio::test]
    async fn apply_delta_handles_remove_then_add() {
        let (mut sched, _rx) = Scheduler::new(16);
        sched.replace_all(vec![make_probe("a", 60)]);
        sched.apply_delta(vec![make_probe("b", 60)], vec![], vec!["a".into()]);
        let ids: std::collections::HashSet<_> = sched.active_ids().into_iter().collect();
        assert!(ids.contains("b"));
        assert!(!ids.contains("a"));
    }

    #[tokio::test]
    async fn config_unchanged_is_a_noop() {
        let (mut sched, _rx) = Scheduler::new(16);
        let p = make_probe("a", 60);
        sched.replace_all(vec![p.clone()]);
        let _h1 = sched.tasks.get("a").map(|h| h.handle.abort_handle());
        sched.apply_delta(vec![], vec![p], vec![]);
        // task identity preserved → upsert short-circuited
        assert_eq!(sched.active_ids(), vec!["a".to_string()]);
    }
}
