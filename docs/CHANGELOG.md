# Changelog

All notable changes to server-monitor-rs are recorded here. The format
loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/), with the agent + panel +
supervisor moving in lockstep.

## [Unreleased]

Nothing yet — track in-flight work in
[the milestone roadmap](../README.md#roadmap) until the next tag.

## [0.2.1] — 2026-04-27

Bug-fix release. Schema unchanged from `v0.2.0` apart from the new
`panel_public_url` setting (auto-seeded by the existing migration
folder); all existing installs upgrade with no manual steps. Agents
should self-update via the panel rollout flow once the panel is on
`v0.2.1` and the poller has cached the new release.

### Add-server flow
- Split `agent_endpoint` (gRPC dial URL) from `panel_public_url` (HTTP
  base for fetching `install-agent.sh`). The two were conflated, so the
  panel-generated curl command pointed at the gRPC port and got
  `HTTP/0.9 when not allowed`.
- Embed `install-agent.sh` and `install-agent.ps1` in the panel binary
  and serve them at `/install-agent.{sh,ps1}`. Previously no route
  existed and curl fell through to the SPA `index.html`.
- Generate `sudo bash -s --` (not `sh`); the script uses bash-isms and
  Debian's `/bin/sh` is dash.
- Inject `--release-url` and `--version` derived from `update_repo` +
  cached `latest_release` so the install can download tarballs without
  the operator passing them by hand.
- New `GET /api/servers/:id/install` and `POST /api/servers/:id/install/rotate`
  let admins re-view (or rotate, with consequences) the install command
  after the create modal closes. Surfaced as a per-row `install` action
  on `/settings/servers`.

### Install script (`deploy/install-agent.sh`)
- Add `${CONFIG_DIR}` to systemd `ReadWritePaths`. Without it the
  agent's post-Register `cfg.save()` failed with EROFS, the panel
  committed `server_token` server-side but the agent never persisted it,
  and every restart looped on `invalid or already-used join_token`.
- `chown ${CONFIG_DIR}` and `agent.yaml` to `USER_RUNAS` so the agent
  user can rewrite its own credentials (atomic `.tmp` + rename needs
  write on the directory).
- Force `systemctl restart` after `daemon-reload`. `enable --now` is a
  no-op for an already-running service, so re-runs never picked up the
  newly-written `agent.yaml`.

See [issue #1](https://github.com/BlackLuny/server-monitor-rs/issues/1)
for the longer-term plan to move agent runtime credentials out of `/etc`
entirely.

### Agent metrics
- `disk_bytes` (Register) and `disk_total` (per-tick) now skip pseudo
  filesystems (`tmpfs`, `devtmpfs`, `overlay`, …) and dedupe by backing
  device. Previously the same physical disk was counted once per mount
  entry (cloud VMs expose `/dev/vda{,1,14,15}` plus several tmpfs); a
  20 GB host showed up as 97.7 GB.

## [0.2.0] — 2026-04-25

Builds on `v0.1.0` with the four follow-ups that close the M7 loop. Schema
unchanged; the supervisor handles `Request::Abort` in addition to the
existing `Update`. New `settings.recent_releases` is populated by the
poller on next tick; older installs auto-upgrade with no migration.

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

[Unreleased]: https://github.com/BlackLuny/server-monitor-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/BlackLuny/server-monitor-rs/releases/tag/v0.2.0
[0.1.0]: https://github.com/BlackLuny/server-monitor-rs/releases/tag/v0.1.0
