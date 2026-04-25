//! TCP connect probe.
//!
//! Time the SYN→SYN-ACK round trip by measuring how long
//! `tokio::net::TcpStream::connect` takes. Anything past `timeout_ms` is
//! reported as a failure with a synthetic timeout error.

use std::time::{Duration, Instant};

use monitor_proto::v1::Probe;
use tokio::{net::TcpStream, time::timeout};

use super::ProbeOutcome;

pub async fn run(probe: &Probe, t: Duration) -> ProbeOutcome {
    if probe.port == 0 {
        return ProbeOutcome::failure("tcp probe needs a non-zero port");
    }
    let dst = format!("{}:{}", probe.target, probe.port);
    let started = Instant::now();
    match timeout(t, TcpStream::connect(&dst)).await {
        Ok(Ok(stream)) => {
            // Drop the connection immediately — we only care about the
            // handshake time. Some servers (e.g. SMTP on port 25) write a
            // banner; we don't read it.
            drop(stream);
            ProbeOutcome::success(started.elapsed())
        }
        Ok(Err(err)) => ProbeOutcome::failure(err.to_string()),
        Err(_) => ProbeOutcome::failure(format!("connect timeout after {:?}", t)),
    }
}
