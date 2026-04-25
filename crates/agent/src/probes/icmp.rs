//! ICMP probe via `surge-ping`, with a system `ping` fallback.
//!
//! The surge-ping path opens a SOCK_DGRAM ICMP socket which on Linux requires
//! the kernel sysctl `net.ipv4.ping_group_range` to include the running
//! group, or the agent process having `cap_net_raw`. macOS allows
//! unprivileged ICMP sockets out of the box; Windows behaves the same via
//! IcmpSendEcho2 wrapped by surge-ping.
//!
//! When the socket open fails (or any other unexpected error), we fall back
//! to executing the system `ping` binary. That ships with every supported
//! OS and is almost always available, even on heavily locked-down hosts.

use std::time::{Duration, Instant};

use monitor_proto::v1::Probe;
use surge_ping::{Client, Config, IcmpPacket, PingIdentifier, PingSequence};

use super::ProbeOutcome;

pub async fn run(probe: &Probe, timeout: Duration) -> ProbeOutcome {
    match try_unprivileged(&probe.target, timeout).await {
        Ok(outcome) => outcome,
        Err(why) => {
            tracing::debug!(target = %probe.target, %why, "ICMP socket unavailable, falling back to /usr/bin/ping");
            try_system_ping(&probe.target, timeout).await
        }
    }
}

async fn try_unprivileged(target: &str, timeout: Duration) -> Result<ProbeOutcome, String> {
    // Resolve manually so we can keep the literal IP and avoid double-resolve.
    let addr = resolve(target).await.map_err(|e| e.to_string())?;

    // Build the random identifier *before* we await — `rand::thread_rng`
    // returns a non-Send handle, and the surrounding scheduler spawns this
    // future onto a multi-thread runtime.
    use rand::Rng;
    let id = rand::thread_rng().gen::<u16>();

    let client = Client::new(&Config::default()).map_err(|e| e.to_string())?;
    let mut pinger = client.pinger(addr, PingIdentifier(id)).await;
    pinger.timeout(timeout);

    let payload = b"server-monitor-rs";
    let started = Instant::now();
    match pinger.ping(PingSequence(0), payload).await {
        Ok((IcmpPacket::V4(_), elapsed)) | Ok((IcmpPacket::V6(_), elapsed)) => {
            // surge-ping reports its own elapsed; trust it.
            Ok(ProbeOutcome::success(elapsed))
        }
        Err(surge_ping::SurgeError::Timeout { .. }) => Ok(ProbeOutcome::failure(format!(
            "timeout after {:?}",
            started.elapsed()
        ))),
        Err(other) => Err(other.to_string()),
    }
}

async fn try_system_ping(target: &str, timeout: Duration) -> ProbeOutcome {
    use tokio::process::Command;

    let secs = timeout.as_secs().max(1).to_string();
    let millis = timeout.as_millis().to_string();
    #[cfg(target_os = "windows")]
    let args: [&str; 4] = ["-n", "1", "-w", &millis];
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let args: [&str; 4] = ["-c", "1", "-W", &millis];
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
    let args: [&str; 4] = ["-c", "1", "-W", &secs];

    let started = Instant::now();
    let res = Command::new("ping").args(args).arg(target).output().await;
    let _ = (&secs, &millis); // explicit reads silence cfg-conditional unused warnings

    match res {
        Ok(out) if out.status.success() => ProbeOutcome::success(started.elapsed()),
        Ok(out) => {
            let msg = String::from_utf8_lossy(&out.stderr);
            let trimmed = msg.trim();
            ProbeOutcome::failure(if trimmed.is_empty() {
                format!("ping exited with {:?}", out.status.code())
            } else {
                trimmed.to_owned()
            })
        }
        Err(err) => ProbeOutcome::failure(format!("failed to spawn ping: {err}")),
    }
}

async fn resolve(target: &str) -> std::io::Result<std::net::IpAddr> {
    use tokio::net::lookup_host;
    if let Ok(ip) = target.parse() {
        return Ok(ip);
    }
    // Append :0 because lookup_host expects host:port. We only need the IP
    // half, so the port is irrelevant.
    let mut addrs = lookup_host(format!("{target}:0")).await?;
    addrs
        .next()
        .map(|sa| sa.ip())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no DNS records"))
}
