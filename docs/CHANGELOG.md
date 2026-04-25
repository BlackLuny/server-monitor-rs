# Changelog

All notable changes to server-monitor-rs are recorded here. The format
loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/), with the agent + panel +
supervisor moving in lockstep.

## [Unreleased]

Nothing yet — track in-flight work in
[the milestone roadmap](../README.md#roadmap) until the next tag.

## [0.1.0] — 2026-04-25

First public release. Includes everything from M1 → M7 phase 3.

### Panel
- Postgres-backed control plane with HTTP + gRPC servers (Axum + tonic).
- SvelteKit dashboard embedded via `rust-embed`.
- First-run `/setup` wizard, login + TOTP + audit log.
- Live metrics via WebSocket, four-tier roll-up (raw / 1m / 5m / 1h).
- ICMP / TCP / HTTP probes with default-on + per-agent override matrix.
- Web SSH terminal (xterm.js bridged to the agent's pty).
- GitHub release poller + rollout state machine for self-update
  orchestration.

### Agent
- Cross-platform metric collector + heartbeat (sysinfo).
- ICMP / TCP / HTTP probe runner with per-probe interval scheduler.
- Web-SSH pty endpoint with optional asciinema recording.
- IPC client for forwarding `UpdateAgent` to the supervisor.

### Supervisor
- Long-lived service-manager target; restarts agent with backoff.
- IPC server (unix socket / named pipe) for self-update commands.
- A/B swap with per-version directory, last-known-good rollback, and
  retention pruning to 3 versions.

### Deployment
- `deploy/install-panel.sh` (interactive + non-interactive modes).
- `deploy/install-agent.sh` for Linux / macOS, registers a
  systemd / OpenRC / launchd service that runs the supervisor.
- `deploy/install-agent.ps1` for Windows (Service or Scheduled Task).
- `deploy/docker/`: Compose stack with optional Caddy + automatic TLS.
- `cargo xtask package` packages 14 archives + SHA256SUMS for releases.

### CI
- `.github/workflows/release.yml`: tag-driven build matrix, Sigstore
  build-provenance attestations, GitHub Release publishing.

[Unreleased]: https://github.com/BlackLuny/server-monitor-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/BlackLuny/server-monitor-rs/releases/tag/v0.1.0
