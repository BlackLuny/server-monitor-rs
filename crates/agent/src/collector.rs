//! Periodic system-metrics sampler.
//!
//! A [`Collector`] owns a `sysinfo::System` plus companion `Disks`, `Networks`,
//! and `Components` instances; each call to [`Collector::sample`] refreshes
//! them and emits a single [`MetricSnapshot`] ready for the wire.
//!
//! Network rates are derived from deltas maintained by sysinfo's own refresh
//! machinery: `NetworkData::received()` and `transmitted()` return bytes seen
//! since the previous refresh, so dividing by elapsed time gives bps.

use std::time::Instant;

use monitor_proto::v1::{DiskUsage, MetricSnapshot, NetUsage};
use sysinfo::{Components, Disks, Networks, System};

/// Long-lived sampler. Construct once per agent run and call [`sample`] each tick.
pub struct Collector {
    system: System,
    disks: Disks,
    networks: Networks,
    components: Components,
    last_refresh: Instant,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    /// Prime the sampler by taking an initial reading. The first call to
    /// [`sample`] will then return a real sample rather than all-zero rates.
    #[must_use]
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        // Refresh CPU twice so the next tick reports real utilization numbers
        // (sysinfo needs a delta to compute percentage).
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        system.refresh_cpu_all();

        Self {
            system,
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            last_refresh: Instant::now(),
        }
    }

    /// Take a fresh sample. Must be called periodically (≥ 1 Hz) for rates to
    /// reflect the time since the previous call rather than since process
    /// start.
    pub fn sample(&mut self) -> MetricSnapshot {
        // Network and disk state must be refreshed *before* we read their
        // delta-accumulators, otherwise we'd be reading stale values.
        self.networks.refresh();
        self.disks.refresh();
        self.components.refresh();
        self.system.refresh_cpu_all();
        self.system.refresh_memory();
        // `remove_dead_processes: true` keeps the count accurate even when
        // short-lived processes exit between ticks.
        self.system
            .refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let now = Instant::now();
        let elapsed_s = (now - self.last_refresh).as_secs_f64().max(0.001);
        self.last_refresh = now;

        // ----- CPU -----
        let cpu_pct = f64::from(self.system.global_cpu_usage());
        let cpu_pct_per_core: Vec<f64> = self
            .system
            .cpus()
            .iter()
            .map(|c| f64::from(c.cpu_usage()))
            .collect();

        // ----- Memory -----
        let mem_used = self.system.used_memory();
        let mem_total = self.system.total_memory();
        let swap_used = self.system.used_swap();
        let swap_total = self.system.total_swap();

        // ----- Load avg (0 on Windows per sysinfo docs) -----
        let load = System::load_average();

        // ----- Disks -----
        // Skip pseudo filesystems (tmpfs / devtmpfs / overlay / …) and
        // dedupe by backing device, otherwise the totals balloon — see
        // `hardware::physical_disks` for the same trap on the Register
        // side.
        let mut disks_detail: Vec<DiskUsage> = Vec::with_capacity(self.disks.list().len());
        let mut disk_used_total: u64 = 0;
        let mut disk_total_total: u64 = 0;
        for d in crate::hardware::physical_disks(&self.disks) {
            let total = d.total_space();
            let avail = d.available_space();
            let used = total.saturating_sub(avail);
            disk_used_total = disk_used_total.saturating_add(used);
            disk_total_total = disk_total_total.saturating_add(total);
            disks_detail.push(DiskUsage {
                mount: d.mount_point().display().to_string(),
                fstype: d.file_system().to_string_lossy().into_owned(),
                used,
                total,
                // sysinfo's disk IO rates are not yet stable cross-platform;
                // surface 0 until we wire a platform-specific probe (M2+).
                read_bps: 0,
                write_bps: 0,
            });
        }

        // ----- Network -----
        let mut nets_detail: Vec<NetUsage> = Vec::new();
        let mut net_in_bps: u64 = 0;
        let mut net_out_bps: u64 = 0;
        let mut net_in_total: u64 = 0;
        let mut net_out_total: u64 = 0;
        for (name, data) in self.networks.iter() {
            // Skip loopback to avoid double-counting agent↔panel traffic on the
            // local machine. Everything else is summed into the aggregate.
            let is_loopback = name == "lo" || name.starts_with("lo0");
            let rx_bytes_delta = data.received();
            let tx_bytes_delta = data.transmitted();
            let rx_bps = ((rx_bytes_delta as f64) / elapsed_s) as u64;
            let tx_bps = ((tx_bytes_delta as f64) / elapsed_s) as u64;
            let rx_total = data.total_received();
            let tx_total = data.total_transmitted();

            if !is_loopback {
                net_in_bps = net_in_bps.saturating_add(rx_bps);
                net_out_bps = net_out_bps.saturating_add(tx_bps);
                net_in_total = net_in_total.saturating_add(rx_total);
                net_out_total = net_out_total.saturating_add(tx_total);
            }
            nets_detail.push(NetUsage {
                name: name.to_string(),
                rx_bps,
                tx_bps,
                rx_total,
                tx_total,
            });
        }

        // ----- Counters -----
        let process_count = u32::try_from(self.system.processes().len()).unwrap_or(u32::MAX);
        let (tcp_conn, udp_conn) = sock_counts().unwrap_or((0, 0));

        // ----- Sensors -----
        let temperature_c = self
            .components
            .list()
            .iter()
            .map(|c| f64::from(c.temperature()))
            .filter(|t| t.is_finite() && *t > 0.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let temperature_c = if temperature_c.is_finite() {
            temperature_c
        } else {
            -1.0
        };

        MetricSnapshot {
            ts_ms: now_ms(),
            cpu_pct,
            cpu_pct_per_core,
            mem_used,
            mem_total,
            swap_used,
            swap_total,
            load_1: load.one,
            load_5: load.five,
            load_15: load.fifteen,
            disk_used: disk_used_total,
            disk_total: disk_total_total,
            disks: disks_detail,
            net_in_bps,
            net_out_bps,
            net_in_total,
            net_out_total,
            nets: nets_detail,
            process_count,
            tcp_conn,
            udp_conn,
            temperature_c,
            gpu_pct: -1.0,
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

/// Best-effort (TCP, UDP) socket counts. `None` on platforms without an
/// inexpensive counter source — caller falls back to `(0, 0)`.
#[cfg(target_os = "linux")]
fn sock_counts() -> Option<(u32, u32)> {
    // `/proc/net/sockstat` is a tiny file that the kernel already computes
    // counters for — much cheaper than parsing `/proc/net/{tcp,udp,tcp6,udp6}`.
    let raw = std::fs::read_to_string("/proc/net/sockstat").ok()?;
    let mut tcp = 0u32;
    let mut udp = 0u32;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("TCP: inuse ") {
            tcp = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("UDP: inuse ") {
            udp = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    Some((tcp, udp))
}

#[cfg(not(target_os = "linux"))]
fn sock_counts() -> Option<(u32, u32)> {
    // macOS and Windows would need `netstat`-style probes which are more
    // expensive than the Linux sockstat. Defer to M2+.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sample_is_non_degenerate() {
        let mut c = Collector::new();
        let s = c.sample();
        // After priming + initial sample, we should see real totals even if
        // all rates are still zero.
        assert!(s.mem_total > 0, "expected positive total memory");
        assert!(
            !s.cpu_pct_per_core.is_empty(),
            "expected at least one CPU core"
        );
        assert!(s.ts_ms > 0);
    }

    #[test]
    fn back_to_back_samples_advance_counters() {
        let mut c = Collector::new();
        let first = c.sample();
        // Give the kernel time to tick some counters.
        std::thread::sleep(Duration::from_millis(250));
        let second = c.sample();
        assert!(second.ts_ms >= first.ts_ms);
        // At least one of total bytes should advance on a machine with any
        // background traffic; don't assert equality to avoid flakiness on
        // fully-quiet CI, just make sure the sampler didn't regress.
        assert!(second.net_in_total >= first.net_in_total);
        assert!(second.net_out_total >= first.net_out_total);
    }
}
