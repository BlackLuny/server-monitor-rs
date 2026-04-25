# Changelog

All notable changes to server-monitor-rs are recorded here. The format
loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/), with the agent + panel +
supervisor moving in lockstep.

## [Unreleased]

### M7.1 — Recordings
- gRPC adds `RecordingFetchRequest` (panel → agent) and
  `RecordingFetchChunk` (agent → panel) so the panel can stream a
  `.cast` over the existing channel.
- Panel exposes `GET /api/recordings/:session_id/download`; the new
  `RecordingHub` routes incoming chunks to the HTTP body stream.
- Agent serves the request from `<recordings_dir>/<session_id>.cast`,
  capped at 256 MiB and validated against path-traversal.
- Terminal page lists recent sessions with one-click `.cast` download.

### M7.2 — Cancellable updates
- Supervisor `Request::Abort { rollout_id }` cancels an in-flight
  staging download via a oneshot fired into the reqwest body await.
- Panel API `POST /api/updates/rollouts/:id/abort` now also pushes
  `PanelToAgent::UpdateAbort` to every connected agent in the set and
  marks pending/sent assignments `failed`.
- Agent forwards `UpdateAbort` to the supervisor over IPC and emits a
  final `UpdateStatus::Failed` so the rollout row updates promptly.
- Panel dispatches `UpdateAgent` on rollout create + on agent reconnect
  (catch-up for offline agents).

### M7.3 — Multi-version rollback
- Poller caches the most recent ten releases under
  `settings.recent_releases`; `latest_release` remains the first entry
  for back-compat.
- `create_rollout` accepts any tag in the cache (rollback or roll
  forward); a new `version_unknown` error replaces the previous
  `version_mismatch` for unknown tags.
- New endpoint `GET /api/updates/recent` returns the cached list.
- `/settings/updates` UI gains a version dropdown and a one-click
  "rollback to vX.Y.Z" button on every aborted/completed rollout row.

### M7.4 — Optional attestation enforcement
- Setting `attestation_required` (default `false`). When true, the
  panel includes the configured `update_repo` in
  `UpdateAgent.attestation_url`.
- Supervisor runs `gh attestation verify <archive> --repo <repo>` after
  the sha256 check; failure (or missing `gh` binary) aborts the swap.
- Documented end-to-end in `docs/release-process.md` §4a.

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
