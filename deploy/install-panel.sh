#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# Installer for the server-monitor-rs panel.
#
# Modes:
#   - Interactive (default, run from a tty): prompts for each value.
#   - Non-interactive: pass all required values via flags + --non-interactive.
#                      Intended for Ansible / CI / automated provisioning.
#
# Produces (in ./server-monitor-rs/ by default):
#   - docker-compose.yml   (copied from deploy/docker/)
#   - .env                 (with generated secrets)
#   - Caddyfile            (only if the user opted into Caddy)
#
# Then runs `docker compose up -d` and prints next-step guidance.
# -----------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." &>/dev/null && pwd)"
COMPOSE_SRC="$REPO_ROOT/deploy/docker/docker-compose.yml"
CADDY_TEMPLATE="$REPO_ROOT/deploy/docker/Caddyfile.tmpl"

GREEN=$'\033[0;32m'
YELLOW=$'\033[0;33m'
RED=$'\033[0;31m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

info()  { printf '%s==>%s %s\n' "$GREEN" "$RESET" "$*"; }
warn()  { printf '%s!!!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
fatal() { printf '%sxxx%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || fatal "missing required tool: $1"
}

random_hex() {
    # Portable across macOS/Linux: use openssl if available, else /dev/urandom.
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex "$1"
    else
        head -c "$(( $1 * 2 ))" /dev/urandom | od -An -vtx1 | tr -d ' \n'
    fi
}

# -----------------------------------------------------------------------------
# Defaults / flags
# -----------------------------------------------------------------------------
NON_INTERACTIVE=0
SKIP_START=0
TARGET_DIR=""
DOMAIN=""
USE_CADDY="ask"            # ask | yes | no
POSTGRES_USER="monitor"
POSTGRES_DB="monitor"
POSTGRES_PASSWORD=""
JWT_SECRET=""

usage() {
    cat <<USAGE
server-monitor-rs panel installer

Common flags (any of these implies a partial answer; fill the rest interactively):
  --target-dir=DIR         Where to write docker-compose.yml + .env (default: ./server-monitor-rs)
  --domain=NAME            Public hostname for Caddy (omit + --no-caddy for plain HTTP)
  --no-caddy               Skip Caddy entirely (panel exposed on :8080 directly)
  --with-caddy             Force Caddy on (no prompt)
  --postgres-user=NAME     Default: monitor
  --postgres-db=NAME       Default: monitor
  --postgres-password=PW   If omitted in non-interactive mode, a random 24-byte hex is generated.
  --jwt-secret=HEX         If omitted, a random 32-byte hex is generated.
  --non-interactive        Never prompt; missing values either get defaults or fail.
  --skip-start             Don't run \`docker compose up -d\` after writing files.
  -h | --help              Show this message.

Examples:
  # Interactive walkthrough (legacy default):
  sudo ./install-panel.sh

  # Unattended Caddy install:
  sudo ./install-panel.sh --non-interactive --with-caddy --domain=panel.example.com

  # Plain-HTTP test rig:
  sudo ./install-panel.sh --non-interactive --no-caddy --target-dir=/opt/panel
USAGE
}

while [[ $# -gt 0 ]]; do
    arg=$1
    case "$arg" in
        --target-dir=*)         TARGET_DIR="${arg#*=}" ;;
        --target-dir)           TARGET_DIR="$2"; shift ;;
        --domain=*)             DOMAIN="${arg#*=}"; USE_CADDY="yes" ;;
        --domain)               DOMAIN="$2"; USE_CADDY="yes"; shift ;;
        --no-caddy)             USE_CADDY="no" ;;
        --with-caddy)           USE_CADDY="yes" ;;
        --postgres-user=*)      POSTGRES_USER="${arg#*=}" ;;
        --postgres-user)        POSTGRES_USER="$2"; shift ;;
        --postgres-db=*)        POSTGRES_DB="${arg#*=}" ;;
        --postgres-db)          POSTGRES_DB="$2"; shift ;;
        --postgres-password=*)  POSTGRES_PASSWORD="${arg#*=}" ;;
        --postgres-password)    POSTGRES_PASSWORD="$2"; shift ;;
        --jwt-secret=*)         JWT_SECRET="${arg#*=}" ;;
        --jwt-secret)           JWT_SECRET="$2"; shift ;;
        --non-interactive)      NON_INTERACTIVE=1 ;;
        --skip-start)           SKIP_START=1 ;;
        -h|--help)              usage; exit 0 ;;
        *)                      fatal "unknown argument: $arg (try --help)" ;;
    esac
    shift
done

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
    IFS= read -r answer </dev/tty || fatal "unable to read from tty"
    if [[ -z $answer ]]; then
        printf -v "$varname" '%s' "$default"
    else
        printf -v "$varname" '%s' "$answer"
    fi
}

prompt_yes_no() {
    local varname=$1 message=$2 default=${3:-n}
    if [[ $NON_INTERACTIVE -eq 1 ]] || ! is_tty; then
        printf -v "$varname" '%s' "$([[ $default = y ]] && echo yes || echo no)"
        return
    fi
    local answer
    printf '%s [%s/%s]: ' \
        "$message" \
        "$([[ $default = y ]] && echo Y || echo y)" \
        "$([[ $default = y ]] && echo n || echo N)"
    IFS= read -r answer </dev/tty || fatal "unable to read from tty"
    answer=${answer:-$default}
    case "$answer" in
        y|Y|yes|YES) printf -v "$varname" '%s' "yes" ;;
        *)           printf -v "$varname" '%s' "no"  ;;
    esac
}

info "server-monitor-rs panel installer"

# --- 1. Preflight ------------------------------------------------------------
need docker
if ! docker compose version >/dev/null 2>&1; then
    fatal "'docker compose' not found. Install Docker Compose v2 (https://docs.docker.com/compose/install/)"
fi
if [[ ! -f $COMPOSE_SRC ]]; then
    fatal "expected compose file at $COMPOSE_SRC (run this script from the repo)"
fi

# --- 2. Choose install dir ---------------------------------------------------
if [[ -z $TARGET_DIR ]]; then
    prompt TARGET_DIR "Install directory" "$PWD/server-monitor-rs"
fi
mkdir -p "$TARGET_DIR"
cd "$TARGET_DIR"

# --- 3. Collect config -------------------------------------------------------
if [[ $USE_CADDY = "ask" ]]; then
    prompt_yes_no USE_CADDY "Enable Caddy for automatic HTTPS + reverse proxy?" y
fi

if [[ $USE_CADDY = yes && -z $DOMAIN ]]; then
    prompt DOMAIN "Public domain name (e.g. panel.example.com)"
    [[ -z $DOMAIN ]] && fatal "domain cannot be empty when Caddy is enabled"
fi

if [[ -z ${POSTGRES_USER:-} ]]; then
    prompt POSTGRES_USER "Postgres username" "monitor"
fi
if [[ -z ${POSTGRES_DB:-} ]]; then
    prompt POSTGRES_DB   "Postgres database" "monitor"
fi
if [[ -z $POSTGRES_PASSWORD ]]; then
    if [[ $NON_INTERACTIVE -eq 1 ]] || ! is_tty; then
        POSTGRES_PASSWORD=$(random_hex 24)
        info "generated Postgres password"
    else
        prompt_yes_no GEN_DB_PASSWORD "Generate a random Postgres password?" y
        if [[ $GEN_DB_PASSWORD = yes ]]; then
            POSTGRES_PASSWORD=$(random_hex 24)
            info "generated Postgres password"
        else
            prompt POSTGRES_PASSWORD "Postgres password" ""
            [[ -z $POSTGRES_PASSWORD ]] && fatal "password cannot be empty"
        fi
    fi
fi

if [[ -z $JWT_SECRET ]]; then
    JWT_SECRET=$(random_hex 32)
    info "generated JWT secret (32 bytes)"
fi

# HTTP bind: loopback when Caddy is in front, otherwise 0.0.0.0 so the panel
# is directly reachable on :8080.
if [[ $USE_CADDY = yes ]]; then
    PANEL_HTTP_BIND="127.0.0.1"
else
    PANEL_HTTP_BIND="0.0.0.0"
fi

# --- 4. Write files ----------------------------------------------------------
info "writing docker-compose.yml"
cp "$COMPOSE_SRC" "$TARGET_DIR/docker-compose.yml"

info "writing .env"
cat > "$TARGET_DIR/.env" <<EOF
# Generated by deploy/install-panel.sh on $(date -u +"%Y-%m-%dT%H:%M:%SZ")
POSTGRES_USER=${POSTGRES_USER}
POSTGRES_DB=${POSTGRES_DB}
POSTGRES_PASSWORD=${POSTGRES_PASSWORD}

PANEL_IMAGE=ghcr.io/lunyxiaoluny/server-monitor-panel:latest

JWT_SECRET=${JWT_SECRET}

LOG_FILTER=info,sqlx=warn
LOG_FORMAT=text

GITHUB_REPO=lunyxiaoluny/server-monitor-rs
GITHUB_TOKEN=

PANEL_HTTP_BIND=${PANEL_HTTP_BIND}
PANEL_GRPC_BIND=0.0.0.0
DOMAIN=${DOMAIN}
EOF
chmod 600 "$TARGET_DIR/.env"

if [[ $USE_CADDY = yes ]]; then
    info "writing Caddyfile for $DOMAIN"
    sed "s/{{DOMAIN}}/${DOMAIN//\//\\/}/g" "$CADDY_TEMPLATE" > "$TARGET_DIR/Caddyfile"
fi

# --- 5. Bring up the stack ---------------------------------------------------
if [[ $SKIP_START -eq 1 ]]; then
    info "skipping \`docker compose up -d\` (--skip-start)"
else
    info "starting containers"
    if [[ $USE_CADDY = yes ]]; then
        docker compose --profile caddy up -d
    else
        docker compose up -d
    fi
fi

# --- 6. Tell the user what to do next ----------------------------------------
cat <<POST

${BOLD}✅ Panel is up.${RESET}

Admin panel URL:
  $(if [[ $USE_CADDY = yes ]]; then echo "  https://${DOMAIN}"; else echo "  http://$(hostname -f 2>/dev/null || hostname):8080"; fi)

${BOLD}Next steps${RESET}
  1. Open the panel URL and create the admin account (task M3 will wire this up).
  2. In Settings → Agent connection, set the URL agents should dial:
       $(if [[ $USE_CADDY = yes ]]; then echo "  https://${DOMAIN}/grpc"; else echo "  http://<panel-host>:9090"; fi)
  3. Click "Add server" to get an install-agent command; run it on each monitored host.

Configuration saved to:
  - $TARGET_DIR/docker-compose.yml
  - $TARGET_DIR/.env          (secrets; keep this private)
$(if [[ $USE_CADDY = yes ]]; then echo "  - $TARGET_DIR/Caddyfile"; fi)

View logs:    cd $TARGET_DIR && docker compose logs -f
Stop panel:   cd $TARGET_DIR && docker compose down
POST
