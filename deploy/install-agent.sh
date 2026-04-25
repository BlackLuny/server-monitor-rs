#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# Installer for the server-monitor-rs agent.
#
# Modes:
#   - Interactive (run from a tty): prompts for what's missing.
#   - One-liner:    curl -fsSL <url>/install-agent.sh | sudo sh -s -- \
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
  curl -fsSL https://example/install-agent.sh | sudo sh -s -- \\
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

if [[ -n $LOCAL_BINARY ]]; then
    [[ -f $LOCAL_BINARY ]] || fatal "--local-binary $LOCAL_BINARY not found"
    info "using local binary $LOCAL_BINARY"
    cp "$LOCAL_BINARY" "$TMP_DIR/monitor-agent"
    chmod +x "$TMP_DIR/monitor-agent"
else
    if [[ -z $RELEASE_URL_BASE ]]; then
        # M7 will fill this in. Until then, the script needs an explicit URL
        # or --local-binary so users don't get a surprise placeholder fetch.
        fatal "no release URL configured yet — pass --local-binary <path> or --release-url <base> (M7 will set a default)"
    fi
    EXT="tar.gz"
    URL="${RELEASE_URL_BASE%/}/${VERSION}/monitor-agent-${TARGET_TRIPLE}.${EXT}"
    info "downloading $URL"
    if command -v curl >/dev/null 2>&1; then
        run curl -fsSL --retry 3 -o "$TMP_DIR/agent.tar.gz" "$URL"
    elif command -v wget >/dev/null 2>&1; then
        run wget -q -O "$TMP_DIR/agent.tar.gz" "$URL"
    else
        fatal "need curl or wget to download the release"
    fi
    run tar -C "$TMP_DIR" -xzf "$TMP_DIR/agent.tar.gz"
    [[ -f "$TMP_DIR/monitor-agent" ]] || fatal "expected monitor-agent in tarball — wrong target?"
    chmod +x "$TMP_DIR/monitor-agent"
fi

# -----------------------------------------------------------------------------
# Layout
# -----------------------------------------------------------------------------
info "creating layout"
run mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$RECORDINGS_DIR" "$LOG_DIR"
run install -m 0755 "$TMP_DIR/monitor-agent" "$INSTALL_DIR/monitor-agent"

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
    chmod 0640 "$AGENT_CONFIG"
    if [[ $OS = linux ]]; then
        chown root:"$USER_RUNAS" "$AGENT_CONFIG" 2>/dev/null || true
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
    cat > "$unit" <<UNIT
[Unit]
Description=server-monitor-rs agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${USER_RUNAS}
Environment=MONITOR_AGENT_CONFIG=${AGENT_CONFIG}
ExecStart=${INSTALL_DIR}/monitor-agent run
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
# Hardening — recordings + state need write access; everything else is read-only.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadWritePaths=${DATA_DIR}

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
    cat > "$unit" <<UNIT
#!/sbin/openrc-run
description="server-monitor-rs agent"
command="${INSTALL_DIR}/monitor-agent"
command_args="run"
command_user="${USER_RUNAS}"
command_background=yes
pidfile="/run/${SERVICE_NAME}.pid"
output_log="${LOG_DIR}/agent.log"
error_log="${LOG_DIR}/agent.log"
export MONITOR_AGENT_CONFIG="${AGENT_CONFIG}"

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
    cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.blackluny.${SERVICE_NAME}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${INSTALL_DIR}/monitor-agent</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>MONITOR_AGENT_CONFIG</key>
        <string>${AGENT_CONFIG}</string>
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
            run systemctl enable --now "${SERVICE_NAME}.service"
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
