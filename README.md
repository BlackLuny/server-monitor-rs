# server-monitor-rs

A lightweight, self-hosted server monitoring panel written in Rust.
Inspired by [komari](https://github.com/komari-monitor/komari) and [nezha](https://github.com/nezhahq/nezha).

## Features (planned)

- Docker one-click deployment (panel + postgres + optional Caddy)
- Admin backend + configurable guest view
- Multi-server monitoring with groups, tags, and search
- Full base metrics: CPU / memory / disk / network / load / processes / temperature / GPU
- Network probes: ICMP / TCP Ping / HTTP(S), multi-location capable
- Agent-panel worker architecture (agent dials in over gRPC, NAT-friendly)
- Admin web SSH terminal via agent forwarding (portable-pty, no server-side SSH port required)
- Automatic agent updates via GitHub Releases with supervisor-based rollback
- Cross-platform agents: Linux / Windows / macOS × amd64 / arm64

## Status

🚧 Under active construction — see task list for milestones.

## Workspace layout

```
crates/
  proto/             # Shared protobuf + tonic-generated code
  common/            # Shared types, config, utilities
  panel/             # Panel binary (HTTP API + gRPC server + web)
  agent/             # Agent binary (runs on monitored servers)
  agent-supervisor/  # Supervisor binary (manages agent lifecycle + updates)
frontend/            # SvelteKit web UI
deploy/              # Docker, Caddy, install scripts
```

## License

Dual-licensed under either of
- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option.
