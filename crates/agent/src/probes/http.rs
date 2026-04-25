//! HTTP(S) probe via reqwest.
//!
//! `target` carries the full URL (with scheme). Optional checks:
//!   - `http_method` defaults to GET; we accept any standard verb
//!   - `http_expect_code = 0` means "any 2xx is fine"
//!   - `http_expect_body` is a substring; empty = skip
//!
//! TLS verification is on by default. The reqwest client is recreated per
//! call rather than shared because probe configs can change at runtime and
//! we want a fresh state — TLS handshake + DNS cost is negligible at the
//! frequencies we run at.

use std::time::{Duration, Instant};

use monitor_proto::v1::Probe;
use reqwest::{Client, Method};

use super::ProbeOutcome;

pub async fn run(probe: &Probe, t: Duration) -> ProbeOutcome {
    let method = parse_method(&probe.http_method);
    let client = match Client::builder().timeout(t).build() {
        Ok(c) => c,
        Err(err) => return ProbeOutcome::failure(format!("client build: {err}")),
    };

    let started = Instant::now();
    let res = client.request(method, &probe.target).send().await;
    let elapsed = started.elapsed();

    let resp = match res {
        Ok(r) => r,
        Err(err) => {
            return if err.is_timeout() {
                ProbeOutcome::failure(format!("timeout after {:?}", t))
            } else {
                ProbeOutcome::failure(err.to_string())
            };
        }
    };

    let status = resp.status();
    let code = status.as_u16();
    if !is_status_ok(code, probe.http_expect_code) {
        return ProbeOutcome::failure(format!("unexpected status {code}"))
            .with_status(u32::from(code));
    }

    if !probe.http_expect_body.is_empty() {
        // We must consume the body to validate. Cap at 1 MiB so a misconfigured
        // probe pointed at a huge response doesn't blow up the agent.
        const MAX_BYTES: usize = 1 << 20;
        match read_capped(resp, MAX_BYTES).await {
            Ok(body) => {
                if !body.contains(&probe.http_expect_body) {
                    return ProbeOutcome::failure(format!(
                        "body did not contain expected substring (status {code})",
                    ))
                    .with_status(u32::from(code));
                }
            }
            Err(err) => {
                return ProbeOutcome::failure(format!("body read: {err}"))
                    .with_status(u32::from(code));
            }
        }
    }

    ProbeOutcome::success(elapsed).with_status(u32::from(code))
}

fn parse_method(s: &str) -> Method {
    if s.is_empty() {
        return Method::GET;
    }
    Method::from_bytes(s.as_bytes()).unwrap_or(Method::GET)
}

/// `expected = 0` means "any 2xx OK". Otherwise must match exactly.
fn is_status_ok(actual: u16, expected: u32) -> bool {
    if expected == 0 {
        (200..300).contains(&actual)
    } else {
        u32::from(actual) == expected
    }
}

async fn read_capped(resp: reqwest::Response, max: usize) -> Result<String, reqwest::Error> {
    // reqwest's `bytes_stream` is gated behind the `stream` feature which
    // adds extra deps; for our small cap a one-shot `bytes()` followed by a
    // truncate is fine.
    let body = resp.bytes().await?;
    let take = body.len().min(max);
    Ok(String::from_utf8_lossy(&body[..take]).into_owned())
}
