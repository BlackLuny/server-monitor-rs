#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# Installer for the server-monitor-rs agent.
#
# Modes:
#   - Interactive (run from a tty): prompts for what's missing.
#   - One-liner:    curl -fsSL <url>/install-agent.sh | sudo bash -s -- \
#                       --endpoint=https://panel.example.com/grpc \
#                       --token=<join-token>
#   - Local binary: --local-binary /path/to/monitor-agent  (skips download)
#
# What it does:
#   1. Detects OS + arch and picks a target triple.
#   2. Either copies --local-binary or downloads a tarball from
#      `--release-url` (defaults to a placeholder until M7 ships).
#   3. Installs to:
#         linux: /opt/monitor-agent/bin/{monitor-agent,monitor-agent-supervisor}
#         macos: /usr/local/monitor-agent/bin/...
#      Recordings dir at <data>/recordings, agent.yaml at:
#         linux: /etc/monitor-agent/agent.yaml
#         macos: /usr/local/etc/monitor-agent/agent.yaml
#   4. Registers a service:
#         systemctl present  → systemd unit
#         rc-service present → OpenRC (Alpine)
#         macOS              → launchd plist
#   5. Starts the service and verifies the agent connected.
#
# Exit non-zero if any step fails — the one-liner mode relies on this.
# -----------------------------------------------------------------------------
set -euo pipefail

# -----------------------------------------------------------------------------
# Defaults & flags
# -----------------------------------------------------------------------------
ENDPOINT=""
TOKEN=""
LOCAL_BINARY=""
RELEASE_URL_BASE=""   # Set by --release-url; M7 will fill in a real default.
VERSION="latest"
HEARTBEAT="10"
INSTALL_DIR=""
CONFIG_DIR=""
DATA_DIR=""
SERVICE_NAME="monitor-agent"
NON_INTERACTIVE=0
SKIP_SERVICE=0
DRY_RUN=0
USER_RUNAS="monitor-agent"

usage() {
    cat <<USAGE
monitor-agent installer

Required:
  --endpoint=URL          Panel gRPC URL (http://… or https://…/grpc)
  --token=TOKEN           Join token from the panel "Add server" dialog

Source (pick one):
  --local-binary=PATH     Copy this binary instead of downloading.
  --release-url=BASE      Override the release tarball base URL.
  --version=VERSION       Release tag (default: latest).

Layout overrides (optional):
  --install-dir=DIR       Where the binary lives (default per-OS).
  --config-dir=DIR        Where agent.yaml lives (default per-OS).
  --data-dir=DIR          Where recordings live (default per-OS).
  --service-name=NAME     systemd / launchd job name (default: monitor-agent).

Flow:
  --heartbeat=N           Seconds between heartbeats (default 10).
  --non-interactive       Never prompt — exit non-zero if anything is missing.
  --skip-service          Don't register/enable/start a service.
  --dry-run               Print actions without doing them.

Examples:
  curl -fsSL https://example/install-agent.sh | sudo bash -s -- \\
       --endpoint=https://panel.example.com/grpc --token=abcd1234

  sudo ./install-agent.sh --local-binary ./monitor-agent \\
       --endpoint=http://10.0.0.5:9090 --token=abcd1234
USAGE
}

# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------
GREEN=$'\033[0;32m'
YELLOW=$'\033[0;33m'
RED=$'\033[0;31m'
RESET=$'\033[0m'
BOLD=$'\033[1m'

info()  { printf '%s==>%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '%s!!!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
fatal() { printf '%sxxx%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

run() {
    if [[ $DRY_RUN -eq 1 ]]; then
        printf '+ %s\n' "$*"
    else
        "$@"
    fi
}

need() {
    command -v "$1" >/dev/null 2>&1 || fatal "missing required tool: $1"
}

is_tty() { [[ -t 0 ]]; }

prompt() {
    local varname=$1 message=$2 default=${3:-}
    if [[ $NON_INTERACTIVE -eq 1 ]] || ! is_tty; then
        if [[ -z $default ]]; then
            fatal "missing required value: $message (running non-interactively)"
        fi
        printf -v "$varname" '%s' "$default"
        return
    fi
    local answer
    if [[ -n $default ]]; then
        printf '%s [%s]: ' "$message" "$default"
    else
        printf '%s: ' "$message"
    fi
    IFS= read -r answer </dev/tty || fatal "unable to read tty"
    printf -v "$varname" '%s' "${answer:-$default}"
}

# -----------------------------------------------------------------------------
# Parse args
# -----------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    arg=$1
    case "$arg" in
        --endpoint=*)        ENDPOINT="${arg#*=}" ;;
        --endpoint)          ENDPOINT="$2"; shift ;;
        --token=*)           TOKEN="${arg#*=}" ;;
        --token)             TOKEN="$2"; shift ;;
        --local-binary=*)    LOCAL_BINARY="${arg#*=}" ;;
        --local-binary)      LOCAL_BINARY="$2"; shift ;;
        --release-url=*)     RELEASE_URL_BASE="${arg#*=}" ;;
        --release-url)       RELEASE_URL_BASE="$2"; shift ;;
        --version=*)         VERSION="${arg#*=}" ;;
        --version)           VERSION="$2"; shift ;;
        --heartbeat=*)       HEARTBEAT="${arg#*=}" ;;
        --heartbeat)         HEARTBEAT="$2"; shift ;;
        --install-dir=*)     INSTALL_DIR="${arg#*=}" ;;
        --install-dir)       INSTALL_DIR="$2"; shift ;;
        --config-dir=*)      CONFIG_DIR="${arg#*=}" ;;
        --config-dir)        CONFIG_DIR="$2"; shift ;;
        --data-dir=*)        DATA_DIR="${arg#*=}" ;;
        --data-dir)          DATA_DIR="$2"; shift ;;
        --service-name=*)    SERVICE_NAME="${arg#*=}" ;;
        --service-name)      SERVICE_NAME="$2"; shift ;;
        --non-interactive)   NON_INTERACTIVE=1 ;;
        --skip-service)      SKIP_SERVICE=1 ;;
        --dry-run)           DRY_RUN=1 ;;
        -h|--help)           usage; exit 0 ;;
        *)                   fatal "unknown argument: $arg (try --help)" ;;
    esac
    shift
done

# -----------------------------------------------------------------------------
# OS / arch detection
# -----------------------------------------------------------------------------
UNAME_S=$(uname -s)
UNAME_M=$(uname -m)

case "$UNAME_S" in
    Linux)  OS="linux" ;;
    Darwin) OS="macos" ;;
    *)      fatal "unsupported OS: $UNAME_S (use the .ps1 installer on Windows)" ;;
esac

case "$UNAME_M" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) fatal "unsupported arch: $UNAME_M" ;;
esac

if [[ $OS = "linux" ]]; then
    TARGET_TRIPLE="${ARCH}-unknown-linux-musl"
else
    TARGET_TRIPLE="${ARCH}-apple-darwin"
fi

# Default per-OS paths.
if [[ -z $INSTALL_DIR ]]; then
    [[ $OS = linux ]] && INSTALL_DIR="/opt/monitor-agent/bin" || INSTALL_DIR="/usr/local/monitor-agent/bin"
fi
if [[ -z $CONFIG_DIR ]]; then
    [[ $OS = linux ]] && CONFIG_DIR="/etc/monitor-agent" || CONFIG_DIR="/usr/local/etc/monitor-agent"
fi
if [[ -z $DATA_DIR ]]; then
    [[ $OS = linux ]] && DATA_DIR="/var/lib/monitor-agent" || DATA_DIR="/usr/local/var/monitor-agent"
fi
RECORDINGS_DIR="$DATA_DIR/recordings"
LOG_DIR="$DATA_DIR/logs"

# -----------------------------------------------------------------------------
# Resolve required values
# -----------------------------------------------------------------------------
[[ $EUID -eq 0 ]] || fatal "must run as root (try: sudo $0 …)"

if [[ -z $ENDPOINT ]]; then
    prompt ENDPOINT "Panel endpoint (e.g. https://panel.example.com/grpc)"
fi
[[ -z $ENDPOINT ]] && fatal "endpoint is required"

if [[ -z $TOKEN ]]; then
    prompt TOKEN "Join token"
fi
[[ -z $TOKEN ]] && fatal "join token is required"

# -----------------------------------------------------------------------------
# Source binary
# -----------------------------------------------------------------------------
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

fetch_archive() {
    local artefact=$1
    local url="${RELEASE_URL_BASE%/}/${VERSION}/${artefact}-${TARGET_TRIPLE}.tar.gz"
    info "downloading $url"
    if command -v curl >/dev/null 2>&1; then
        run curl -fsSL --retry 3 -o "$TMP_DIR/${artefact}.tar.gz" "$url"
    elif command -v wget >/dev/null 2>&1; then
        run wget -q -O "$TMP_DIR/${artefact}.tar.gz" "$url"
    else
        fatal "need curl or wget to download the release"
    fi
    local extract_dir="$TMP_DIR/${artefact}-extracted"
    run mkdir -p "$extract_dir"
    run tar -C "$extract_dir" -xzf "$TMP_DIR/${artefact}.tar.gz"
    local found
    found=$(find "$extract_dir" -name "$artefact" -type f | head -1)
    [[ -n $found ]] || fatal "expected $artefact in tarball — wrong target?"
    cp "$found" "$TMP_DIR/$artefact"
    chmod +x "$TMP_DIR/$artefact"
}

if [[ -n $LOCAL_BINARY ]]; then
    [[ -f $LOCAL_BINARY ]] || fatal "--local-binary $LOCAL_BINARY not found"
    info "using local binary $LOCAL_BINARY"
    cp "$LOCAL_BINARY" "$TMP_DIR/monitor-agent"
    chmod +x "$TMP_DIR/monitor-agent"
    # Supervisor sits next to the agent binary in the local-binary path so
    # M7 self-update works without a network fetch.
    if [[ -f "$(dirname "$LOCAL_BINARY")/monitor-agent-supervisor" ]]; then
        cp "$(dirname "$LOCAL_BINARY")/monitor-agent-supervisor" "$TMP_DIR/monitor-agent-supervisor"
        chmod +x "$TMP_DIR/monitor-agent-supervisor"
    else
        warn "monitor-agent-supervisor not found next to --local-binary; self-update will be unavailable"
    fi
else
    if [[ -z $RELEASE_URL_BASE ]]; then
        fatal "no release URL configured yet — pass --local-binary <path> or --release-url <base>"
    fi
    fetch_archive monitor-agent
    fetch_archive monitor-agent-supervisor
fi

# -----------------------------------------------------------------------------
# Layout
# -----------------------------------------------------------------------------
info "creating layout"
VERSIONS_DIR="$DATA_DIR/versions"
RUNTIME_DIR="/run/monitor-agent"
[[ $OS = macos ]] && RUNTIME_DIR="$DATA_DIR/run"
run mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$RECORDINGS_DIR" "$LOG_DIR" "$VERSIONS_DIR" "$RUNTIME_DIR"
run install -m 0755 "$TMP_DIR/monitor-agent" "$INSTALL_DIR/monitor-agent"
if [[ -f "$TMP_DIR/monitor-agent-supervisor" ]]; then
    run install -m 0755 "$TMP_DIR/monitor-agent-supervisor" "$INSTALL_DIR/monitor-agent-supervisor"
fi
SUPERVISOR_SOCKET="$RUNTIME_DIR/supervisor.sock"

# -----------------------------------------------------------------------------
# Service user (Linux only — macOS launchd runs as root unless told otherwise)
# -----------------------------------------------------------------------------
if [[ $OS = linux && $SKIP_SERVICE -eq 0 ]]; then
    if ! id "$USER_RUNAS" >/dev/null 2>&1; then
        info "creating system user $USER_RUNAS"
        if command -v useradd >/dev/null 2>&1; then
            run useradd --system --no-create-home --shell /usr/sbin/nologin "$USER_RUNAS"
        elif command -v adduser >/dev/null 2>&1; then
            # busybox adduser (Alpine).
            run adduser -S -H -D -s /sbin/nologin "$USER_RUNAS"
        else
            warn "no useradd/adduser found — running as root"
            USER_RUNAS="root"
        fi
    fi
    run chown -R "$USER_RUNAS:" "$DATA_DIR"
    # Recordings dir needs group-readable so a per-team operator can fetch
    # casts via SSH without sudo.
    run chmod 0750 "$RECORDINGS_DIR"
fi

# -----------------------------------------------------------------------------
# agent.yaml — generated via the binary's own `configure` subcommand so
# format stays canonical even when fields evolve.
# -----------------------------------------------------------------------------
AGENT_CONFIG="$CONFIG_DIR/agent.yaml"
info "writing $AGENT_CONFIG"
run env MONITOR_AGENT_CONFIG="$AGENT_CONFIG" "$INSTALL_DIR/monitor-agent" \
    configure --endpoint "$ENDPOINT" --token "$TOKEN" --heartbeat "$HEARTBEAT"

if [[ $DRY_RUN -eq 0 ]]; then
    # The agent rewrites its own config after first Register (to persist
    # server_token). Atomic save = write `<file>.tmp` then rename, which
    # needs write permission on the *directory* and the *file*. Owning
    # both as USER_RUNAS is the simplest policy that makes that work
    # without granting world-write. See issue #1 for the longer-term plan
    # to move runtime credentials out of /etc entirely.
    chmod 0640 "$AGENT_CONFIG"
    if [[ $OS = linux ]]; then
        chown "$USER_RUNAS:$USER_RUNAS" "$AGENT_CONFIG" 2>/dev/null || true
        chown "$USER_RUNAS:$USER_RUNAS" "$CONFIG_DIR" 2>/dev/null || true
        chmod 0750 "$CONFIG_DIR" 2>/dev/null || true
    fi
fi

# -----------------------------------------------------------------------------
# Service registration
# -----------------------------------------------------------------------------
detect_init() {
    if [[ $OS = macos ]]; then
        echo "launchd"
    elif command -v systemctl >/dev/null 2>&1 && [[ -d /run/systemd/system ]]; then
        echo "systemd"
    elif command -v rc-service >/dev/null 2>&1; then
        echo "openrc"
    else
        echo "none"
    fi
}

write_systemd_unit() {
    local unit="/etc/systemd/system/${SERVICE_NAME}.service"
    info "writing $unit"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "+ cat > $unit"
        return
    fi
    # ExecStart prefers the supervisor when it's installed — that's the
    # path that supports M7 self-update. Falls back to running the agent
    # directly when only the agent binary was shipped.
    local exec_start
    if [[ -x "${INSTALL_DIR}/monitor-agent-supervisor" ]]; then
        exec_start="${INSTALL_DIR}/monitor-agent-supervisor --root ${DATA_DIR} --agent-binary ${INSTALL_DIR}/monitor-agent --ipc-path ${SUPERVISOR_SOCKET} -- run"
    else
        exec_start="${INSTALL_DIR}/monitor-agent run"
    fi
    cat > "$unit" <<UNIT
[Unit]
Description=server-monitor-rs agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${USER_RUNAS}
Environment=MONITOR_AGENT_CONFIG=${AGENT_CONFIG}
Environment=MONITOR_SUPERVISOR_IPC=${SUPERVISOR_SOCKET}
RuntimeDirectory=monitor-agent
RuntimeDirectoryMode=0750
ExecStart=${exec_start}
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
# Hardening — recordings + state need write access; everything else is read-only.
# CONFIG_DIR has to be writable too: after the first successful Register the
# agent persists the returned server_token back to agent.yaml. Without this
# the write fails with EROFS, the panel-side DB is updated but the agent
# can't save the new credential, and every restart loops on
# "invalid or already-used join_token".
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=${DATA_DIR} ${CONFIG_DIR}

[Install]
WantedBy=multi-user.target
UNIT
}

write_openrc_unit() {
    local unit="/etc/init.d/${SERVICE_NAME}"
    info "writing $unit"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "+ cat > $unit"
        return
    fi
    local cmd args
    if [[ -x "${INSTALL_DIR}/monitor-agent-supervisor" ]]; then
        cmd="${INSTALL_DIR}/monitor-agent-supervisor"
        args="--root ${DATA_DIR} --agent-binary ${INSTALL_DIR}/monitor-agent --ipc-path ${SUPERVISOR_SOCKET} -- run"
    else
        cmd="${INSTALL_DIR}/monitor-agent"
        args="run"
    fi
    cat > "$unit" <<UNIT
#!/sbin/openrc-run
description="server-monitor-rs agent"
command="${cmd}"
command_args="${args}"
command_user="${USER_RUNAS}"
command_background=yes
pidfile="/run/${SERVICE_NAME}.pid"
output_log="${LOG_DIR}/agent.log"
error_log="${LOG_DIR}/agent.log"
export MONITOR_AGENT_CONFIG="${AGENT_CONFIG}"
export MONITOR_SUPERVISOR_IPC="${SUPERVISOR_SOCKET}"

depend() {
    need net
}
UNIT
    chmod +x "$unit"
}

write_launchd_plist() {
    local plist="/Library/LaunchDaemons/com.blackluny.${SERVICE_NAME}.plist"
    info "writing $plist"
    if [[ $DRY_RUN -eq 1 ]]; then
        echo "+ cat > $plist"
        return
    fi
    local prog_args
    if [[ -x "${INSTALL_DIR}/monitor-agent-supervisor" ]]; then
        prog_args="<string>${INSTALL_DIR}/monitor-agent-supervisor</string>
        <string>--root</string><string>${DATA_DIR}</string>
        <string>--agent-binary</string><string>${INSTALL_DIR}/monitor-agent</string>
        <string>--ipc-path</string><string>${SUPERVISOR_SOCKET}</string>
        <string>--</string><string>run</string>"
    else
        prog_args="<string>${INSTALL_DIR}/monitor-agent</string>
        <string>run</string>"
    fi
    cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.blackluny.${SERVICE_NAME}</string>
    <key>ProgramArguments</key>
    <array>
        ${prog_args}
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>MONITOR_AGENT_CONFIG</key>
        <string>${AGENT_CONFIG}</string>
        <key>MONITOR_SUPERVISOR_IPC</key>
        <string>${SUPERVISOR_SOCKET}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${LOG_DIR}/agent.log</string>
    <key>StandardErrorPath</key>
    <string>${LOG_DIR}/agent.log</string>
</dict>
</plist>
PLIST
    chown root:wheel "$plist"
    chmod 0644 "$plist"
}

if [[ $SKIP_SERVICE -eq 1 ]]; then
    info "skipping service setup (--skip-service)"
else
    INIT=$(detect_init)
    case "$INIT" in
        systemd)
            write_systemd_unit
            run systemctl daemon-reload
            run systemctl enable "${SERVICE_NAME}.service"
            # `enable --now` is a no-op for already-running services. On a
            # re-run that's exactly the wrong thing — the daemon stays on
            # the previous agent.yaml (cached in memory) and never sees the
            # freshly-rewritten join_token. Force a restart so re-installs
            # actually pick up new credentials / endpoint changes.
            run systemctl restart "${SERVICE_NAME}.service"
            ;;
        openrc)
            write_openrc_unit
            run rc-update add "$SERVICE_NAME" default
            run rc-service "$SERVICE_NAME" start
            ;;
        launchd)
            write_launchd_plist
            run launchctl load "/Library/LaunchDaemons/com.blackluny.${SERVICE_NAME}.plist"
            ;;
        none)
            warn "no supported init system detected — start manually:"
            warn "  $INSTALL_DIR/monitor-agent run"
            ;;
    esac
fi

# -----------------------------------------------------------------------------
# Verify
# -----------------------------------------------------------------------------
if [[ $DRY_RUN -eq 0 && $SKIP_SERVICE -eq 0 ]]; then
    info "waiting up to 10s for first heartbeat to land"
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if "$INSTALL_DIR/monitor-agent" --config "$AGENT_CONFIG" self-check >/dev/null 2>&1; then
            info "agent healthy"
            break
        fi
        sleep 1
    done
fi

cat <<POST

${BOLD}✅ monitor-agent installed.${RESET}

Binary:    $INSTALL_DIR/monitor-agent
Config:    $AGENT_CONFIG  (chmod 0640)
Records:   $RECORDINGS_DIR
Logs:      $LOG_DIR/agent.log

$(case "$(detect_init)" in
    systemd)  echo "Manage:   systemctl status $SERVICE_NAME";;
    openrc)   echo "Manage:   rc-service $SERVICE_NAME status";;
    launchd)  echo "Manage:   launchctl list | grep $SERVICE_NAME";;
    *)        echo "Manage:   start/stop manually";;
esac)
Tail:      $(case "$(detect_init)" in
    systemd) echo "journalctl -fu $SERVICE_NAME";;
    *)       echo "tail -F $LOG_DIR/agent.log";;
esac)

If the agent doesn't show up in the panel within ~30 seconds, check the
logs and confirm \`endpoint\` in $AGENT_CONFIG points at the panel's gRPC
listener (NOT the HTTP UI).
POST
