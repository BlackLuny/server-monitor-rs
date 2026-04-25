# Deploying server-monitor-rs

Four supported install paths, in roughly increasing order of "amount of
control you want over the host". Pick one.

| Path | Best for | Panel | Agent |
| --- | --- | --- | --- |
| `install-panel.sh` (interactive) | First-time deploys, single host | ✅ | — |
| `install-panel.sh --non-interactive` | Ansible / CI provisioning | ✅ | — |
| `docker compose --profile caddy up -d` | You already manage Docker | ✅ | — |
| `install-agent.sh` / `install-agent.ps1` | Every monitored host | — | ✅ |
| Manual binary + service unit | Air-gapped or unusual platforms | ✅ | ✅ |

The panel and agents communicate over gRPC. The panel listens on **8080
(HTTP)** + **9090 (gRPC)** — Caddy in front terminates TLS for both and
exposes the gRPC subtree at `/grpc/*`.

---

## 1. `install-panel.sh` — interactive panel install

Brings up Postgres + panel (+ optional Caddy) via `docker compose`.
Generates a fresh `JWT_SECRET` and Postgres password automatically;
walks you through Caddy / domain setup.

```sh
git clone https://github.com/BlackLuny/server-monitor-rs.git
cd server-monitor-rs
sudo ./deploy/install-panel.sh
```

Files written to `./server-monitor-rs/`:

- `docker-compose.yml` — copy of the canonical file in `deploy/docker/`
- `.env` — generated secrets (mode 0600)
- `Caddyfile` — only when Caddy is enabled

Then `docker compose logs -f` for tailing. **The first thing you do in
the browser is `/setup`** — that's where the first admin gets created.

### Non-interactive variant

For Ansible / Terraform / GitOps:

```sh
sudo ./deploy/install-panel.sh \
    --non-interactive \
    --with-caddy \
    --domain=panel.example.com \
    --target-dir=/opt/monitor-panel
```

Flags worth knowing about:

- `--no-caddy` — expose `:8080` directly. Pair with a reverse proxy you
  already trust.
- `--postgres-password=…` / `--jwt-secret=…` — pin specific secrets
  instead of letting the script generate them.
- `--skip-start` — write files but don't run `docker compose up -d`. Use
  when you want to inspect or commit the generated config first.

`--help` prints the full list.

---

## 2. Bare `docker compose`

Same containers, no script — useful when you already have a `.env`
template you manage yourself.

```sh
cd deploy/docker
cp .env.example .env
# Fill in POSTGRES_PASSWORD + JWT_SECRET (≥ 32 bytes)
docker compose up -d                # plain HTTP on :8080
docker compose --profile caddy up -d # adds Caddy + TLS termination
```

The `panel` service binds HTTP only on `127.0.0.1:8080` by default — so
Caddy can sit in front without anyone reaching the panel raw. Override
`PANEL_HTTP_BIND=0.0.0.0` in `.env` to expose it directly.

`Caddyfile.tmpl` carries the `{{DOMAIN}}` placeholder. Replace it
yourself or just use `install-panel.sh` which does the substitution.

### gRPC over TLS

Caddy reverse-proxies `/grpc/*` to the panel using cleartext HTTP/2
(`h2c`). Agents are configured with `https://<domain>/grpc` so the
client-to-Caddy hop is TLS-encrypted; panel-to-Caddy stays inside the
container network.

`flush_interval -1` in the Caddyfile keeps streaming RPCs (Web SSH,
agent heartbeats) from being buffered out.

---

## 3. `install-agent.sh` — agents on Linux / macOS

Runs on each monitored host. Detects OS + arch, fetches (or copies via
`--local-binary`) the matching binary, writes
`/etc/monitor-agent/agent.yaml`, and registers a systemd / OpenRC /
launchd service.

```sh
# One-liner via curl (only works once a release is published — see TODO):
curl -fsSL https://example.com/install-agent.sh | sudo sh -s -- \
    --endpoint=https://panel.example.com/grpc \
    --token=<join-token-from-panel>

# Local binary mode (today, before M7 ships releases):
sudo ./deploy/install-agent.sh \
    --local-binary ./monitor-agent \
    --endpoint=http://10.0.0.5:9090 \
    --token=<join-token>
```

Get the join token from the panel by clicking **"Add server"** on the
servers list — the modal also prints a fully-formed install command for
copy-paste.

Layout the script creates:

| Path (Linux) | Path (macOS) | Contents |
| --- | --- | --- |
| `/opt/monitor-agent/bin/` | `/usr/local/monitor-agent/bin/` | binary |
| `/etc/monitor-agent/` | `/usr/local/etc/monitor-agent/` | `agent.yaml` (mode 0640) |
| `/var/lib/monitor-agent/recordings/` | `/usr/local/var/monitor-agent/recordings/` | asciinema `.cast` files |
| `/var/lib/monitor-agent/logs/` | `/usr/local/var/monitor-agent/logs/` | per-service log file |

Service control:

```sh
systemctl status monitor-agent       # systemd hosts
journalctl -fu monitor-agent         # follow logs

rc-service monitor-agent status      # Alpine / OpenRC

launchctl list | grep monitor-agent  # macOS
```

`--skip-service` writes everything but doesn't start the service —
useful when you want to inspect the unit file before going live.

---

## 4. `install-agent.ps1` — Windows portable

Run **as administrator**:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
.\deploy\install-agent.ps1 `
    -Endpoint https://panel.example.com/grpc `
    -Token <join-token> `
    -LocalBinary .\monitor-agent.exe
```

Layout: `C:\ProgramData\monitor-agent\{bin,config,data,data\recordings,data\logs}`.

Registers a Windows Service (`sc.exe create monitor-agent`) by default.
Pass `-UseScheduledTask` to register a SYSTEM-context scheduled task
instead — useful on locked-down Windows builds where service creation
is restricted.

---

## 5. Manual install (advanced)

You only need this on:

- Air-gapped hosts where neither curl nor scripted package fetches work
- Platforms not covered by the install scripts (FreeBSD, illumos, …)

Steps:

1. Build or fetch a binary for the target. Cross-compile locally:
   ```sh
   cargo zigbuild --release --target x86_64-unknown-linux-musl \
       -p monitor-panel -p monitor-agent
   ```
   Or use the bundled packager:
   ```sh
   cargo xtask package --target x86_64-unknown-linux-musl
   # → dist/monitor-{panel,agent,supervisor}-…-tar.gz + SHA256SUMS
   ```
2. Copy the binary to the host (`scp`, `rsync`, USB stick, …).
3. Generate a config: `./monitor-agent configure --endpoint <url> --token <t>`.
4. Wire it into your init system. The systemd/launchd/openrc snippets
   inside `install-agent.sh` are good starting points to copy.

---

## TODO before M7 ships

- The one-liner `curl … | sh` examples above need a real default
  `--release-url`. Set it once cargo-dist publishes to GitHub Releases
  (search for `RELEASE_URL_BASE` in `install-agent.sh`).
- Same for `install-agent.ps1` (`-ReleaseUrl`).

Until then, both scripts require either `--local-binary` (Linux/macOS) /
`-LocalBinary` (Windows) or an explicit `--release-url`.
