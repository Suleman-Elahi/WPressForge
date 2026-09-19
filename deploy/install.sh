#!/usr/bin/env bash
# Interactive installer for a production WPressForge panel, agent, or both.
#
# Run from a release bundle that preserves the repository layout:
#   sudo deploy/install.sh
#   sudo deploy/install.sh --role all-in-one
#   sudo deploy/install.sh --role agent --non-interactive \
#     WP_AGENT_TOKEN=... WP_AGENT_PANEL_IP=203.0.113.10
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
ROLE=${INSTALL_ROLE:-}
NON_INTERACTIVE=false
PLAN_ONLY=false
OVERWRITE=${INSTALL_OVERWRITE:-false}

usage() {
  cat <<'USAGE'
Usage: sudo deploy/install.sh [options]

Install a WPressForge component from a release bundle containing deploy/,
static/, and target/release/{wp-panel,wp-agent}.

Options:
  --role panel|agent|all-in-one  Component(s) to install.
  --non-interactive              Read values only from the environment.
  --plan                         Validate the bundle and print the actions only.
  --overwrite                    Permit replacement of existing *.env files.
  -h, --help                     Show this help.

Non-interactive inputs:
  Panel: WP_PANEL_ADMIN_EMAIL, WP_PANEL_ADMIN_PASSWORD (optional),
         WP_PANEL_SECRET_KEY (optional), WP_PANEL_SMTP_HOST,
         WP_PANEL_SMTP_PORT, WP_PANEL_SMTP_USER, WP_PANEL_SMTP_PASSWORD,
         WP_PANEL_SMTP_FROM.
  Agent: WP_AGENT_TOKEN (required, 24+ characters), WP_AGENT_BIND,
         WP_AGENT_PANEL_IP (required unless bind is loopback),
         WP_AGENT_ACME_EMAIL, WP_AGENT_RESTIC_REPO, PHP_VERSIONS,
         NGINX_MAINLINE.

The agent always starts with WP_AGENT_DRY_RUN=true. Review its logged operations
before explicitly changing it to false.
USAGE
}

fail() { echo "error: $*" >&2; exit 1; }
info() { echo "==> $*"; }
warn() { echo "warning: $*" >&2; }

is_true() { [[ "${1,,}" == "true" || "${1,,}" == "yes" || "$1" == "1" ]]; }
is_loopback_bind() {
  [[ "$1" == 127.0.0.1:* || "$1" == "[::1]:"* || "$1" == localhost:* ]]
}

require_value() {
  local name=$1 value=${!1:-}
  [[ -n "$value" ]] || fail "$name is required in non-interactive mode"
}

prompt_value() {
  local variable=$1 label=$2 default=${3:-} secret=${4:-false}
  local current=${!variable:-} value
  if [[ -n "$current" ]]; then
    return
  fi
  if $NON_INTERACTIVE; then
    printf -v "$variable" '%s' "$default"
    return
  fi
  if [[ -n "$default" ]]; then
    printf '%s [%s]: ' "$label" "$default"
  else
    printf '%s: ' "$label"
  fi
  if is_true "$secret"; then
    read -r -s value
    echo
  else
    read -r value
  fi
  printf -v "$variable" '%s' "${value:-$default}"
}

confirm() {
  local message=$1 response
  if $NON_INTERACTIVE; then
    return 0
  fi
  read -r -p "$message [y/N] " response
  [[ "$response" =~ ^([Yy]|[Yy][Ee][Ss])$ ]]
}

validate_single_line() {
  local name=$1 value=$2
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] || fail "$name must be one line"
}

backup_existing_env() {
  local path=$1
  [[ -f "$path" ]] || return
  if ! is_true "$OVERWRITE"; then
    if $NON_INTERACTIVE; then
      fail "$path already exists; set INSTALL_OVERWRITE=true to replace it"
    fi
    confirm "$path exists. Back it up and replace it?" || fail "installation cancelled"
  fi
  local backup="${path}.$(date +%Y%m%d%H%M%S).bak"
  cp -p -- "$path" "$backup"
  info "backed up existing configuration to $backup"
}

verify_supported_os() {
  [[ -r /etc/os-release ]] || fail "cannot identify the operating system"
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}:${VERSION_ID:-}" in
    debian:12|ubuntu:24.04) ;;
    *) fail "supported operating systems are Debian 12 and Ubuntu 24.04 (found ${PRETTY_NAME:-unknown})" ;;
  esac
}

verify_bundle() {
  [[ -f "$SCRIPT_DIR/install-agent.sh" ]] || fail "missing deploy/install-agent.sh"
  [[ -f "$SCRIPT_DIR/systemd/wp-panel.service" ]] || fail "missing panel systemd unit"
  [[ -f "$SCRIPT_DIR/systemd/wp-agent.service" ]] || fail "missing agent systemd unit"
  [[ -d "$REPO_ROOT/static" ]] || fail "missing static/ directory"
  case "$ROLE" in
    panel) [[ -f "$REPO_ROOT/target/release/wp-panel" ]] || fail "missing target/release/wp-panel" ;;
    agent) [[ -f "$REPO_ROOT/target/release/wp-agent" ]] || fail "missing target/release/wp-agent" ;;
    all-in-one)
      [[ -f "$REPO_ROOT/target/release/wp-panel" ]] || fail "missing target/release/wp-panel"
      [[ -f "$REPO_ROOT/target/release/wp-agent" ]] || fail "missing target/release/wp-agent" ;;
  esac
}

choose_role() {
  if [[ -n "$ROLE" ]]; then
    return
  fi
  if $NON_INTERACTIVE; then
    fail "INSTALL_ROLE or --role is required in non-interactive mode"
  fi
  echo "Choose the installation role:"
  select choice in "Panel only" "Agent only" "Panel and agent on this host"; do
    case "$REPLY" in
      1) ROLE=panel; break ;;
      2) ROLE=agent; break ;;
      3) ROLE=all-in-one; break ;;
      *) echo "Enter 1, 2, or 3." ;;
    esac
  done
}

write_panel_env() {
  local env_file=/etc/wp-panel/panel.env
  backup_existing_env "$env_file"
  umask 077
  cat > "$env_file" <<ENV
WP_PANEL_BIND=127.0.0.1:8080
WP_PANEL_DB=/var/lib/wp-panel/data/panel.db
WP_PANEL_STATIC=/opt/wp-panel/static
WP_PANEL_SECRET_KEY=${WP_PANEL_SECRET_KEY}
WP_PANEL_ADMIN_EMAIL=${WP_PANEL_ADMIN_EMAIL}
WP_PANEL_DEMO_DATA=false
WP_PANEL_SECURE_COOKIES=true
WP_PANEL_WORKERS=4
ENV
  [[ -n "${WP_PANEL_ADMIN_PASSWORD:-}" ]] && printf 'WP_PANEL_ADMIN_PASSWORD=%s\n' "$WP_PANEL_ADMIN_PASSWORD" >> "$env_file"
  [[ -n "${WP_PANEL_SMTP_HOST:-}" ]] && printf 'WP_PANEL_SMTP_HOST=%s\n' "$WP_PANEL_SMTP_HOST" >> "$env_file"
  [[ -n "${WP_PANEL_SMTP_PORT:-}" ]] && printf 'WP_PANEL_SMTP_PORT=%s\n' "$WP_PANEL_SMTP_PORT" >> "$env_file"
  [[ -n "${WP_PANEL_SMTP_USER:-}" ]] && printf 'WP_PANEL_SMTP_USER=%s\n' "$WP_PANEL_SMTP_USER" >> "$env_file"
  [[ -n "${WP_PANEL_SMTP_PASSWORD:-}" ]] && printf 'WP_PANEL_SMTP_PASSWORD=%s\n' "$WP_PANEL_SMTP_PASSWORD" >> "$env_file"
  [[ -n "${WP_PANEL_SMTP_FROM:-}" ]] && printf 'WP_PANEL_SMTP_FROM=%s\n' "$WP_PANEL_SMTP_FROM" >> "$env_file"
  printf '%s\n' 'WP_PANEL_LOG=info,sqlx=warn,tower_http=info' >> "$env_file"
  chmod 0600 "$env_file"
}

install_panel() {
  prompt_value WP_PANEL_ADMIN_EMAIL "Bootstrap administrator email" "admin@localhost"
  prompt_value WP_PANEL_SECRET_KEY "Panel encryption key (leave blank to generate)" ""
  if [[ -z "${WP_PANEL_SECRET_KEY:-}" ]]; then
    WP_PANEL_SECRET_KEY=$(openssl rand -base64 32)
    GENERATED_PANEL_KEY=true
  else
    GENERATED_PANEL_KEY=false
  fi
  prompt_value WP_PANEL_ADMIN_PASSWORD "Bootstrap administrator password (leave blank to generate)" "" true
  if [[ -z "${WP_PANEL_ADMIN_PASSWORD:-}" ]]; then
    WP_PANEL_ADMIN_PASSWORD=$(openssl rand -base64 24)
    GENERATED_ADMIN_PASSWORD=true
  else
    GENERATED_ADMIN_PASSWORD=false
  fi
  prompt_value WP_PANEL_SMTP_HOST "SMTP host (optional)" ""
  if [[ -n "${WP_PANEL_SMTP_HOST:-}" ]]; then
    prompt_value WP_PANEL_SMTP_PORT "SMTP port" "587"
    prompt_value WP_PANEL_SMTP_USER "SMTP user (optional)" ""
    prompt_value WP_PANEL_SMTP_PASSWORD "SMTP password (optional)" "" true
    prompt_value WP_PANEL_SMTP_FROM "SMTP from address (optional)" ""
  fi
  for value in "$WP_PANEL_ADMIN_EMAIL" "$WP_PANEL_SECRET_KEY" "$WP_PANEL_ADMIN_PASSWORD" "${WP_PANEL_SMTP_HOST:-}" "${WP_PANEL_SMTP_PASSWORD:-}"; do
    validate_single_line "panel configuration value" "$value"
  done

  if $PLAN_ONLY; then
    info "would install panel packages, binary, static assets, configuration, and systemd service"
    info "panel will listen only on 127.0.0.1:8080; configure its Nginx TLS vhost after DNS is ready"
    return
  fi

  info "installing panel prerequisites"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y --no-install-recommends ca-certificates curl nginx certbot sqlite3

  if ! id -u wp-panel >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin wp-panel
  fi
  install -d -o wp-panel -g wp-panel -m 0750 /opt/wp-panel /opt/wp-panel/static
  install -d -o wp-panel -g wp-panel -m 0750 /var/lib/wp-panel/data
  install -d -o root -g root -m 0750 /etc/wp-panel
  install -o wp-panel -g wp-panel -m 0755 "$REPO_ROOT/target/release/wp-panel" /opt/wp-panel/wp-panel
  cp -a "$REPO_ROOT/static/." /opt/wp-panel/static/
  chown -R wp-panel:wp-panel /opt/wp-panel/static
  write_panel_env
  install -m 0644 "$SCRIPT_DIR/systemd/wp-panel.service" /etc/systemd/system/wp-panel.service
  systemctl daemon-reload
  systemctl enable --now wp-panel
  curl -fsS http://127.0.0.1:8080/healthz | grep -qx 'ok' || fail "panel health check failed; inspect: journalctl -u wp-panel -n 50 --no-pager"

  echo
  info "panel is running on 127.0.0.1:8080"
  if is_true "$GENERATED_PANEL_KEY"; then
    echo "Panel encryption key (store this outside the server): $WP_PANEL_SECRET_KEY"
  fi
  if is_true "$GENERATED_ADMIN_PASSWORD"; then
    echo "Bootstrap administrator password (shown once): $WP_PANEL_ADMIN_PASSWORD"
  else
    echo "Bootstrap administrator password was supplied and is not displayed."
  fi
  echo "After signing in, remove WP_PANEL_ADMIN_PASSWORD from /etc/wp-panel/panel.env and restart wp-panel."
  echo "Next: configure the Nginx TLS vhost from plans/DEPLOYMENT.md §5 before exposing the panel."
}

install_agent() {
  local default_bind panel_ip
  if [[ "$ROLE" == all-in-one ]]; then
    default_bind=127.0.0.1:8443
  else
    default_bind=0.0.0.0:8443
  fi
  prompt_value WP_AGENT_BIND "Agent bind address" "$default_bind"
  prompt_value WP_AGENT_TOKEN "Agent shared token (leave blank to generate)" "" true
  if [[ -z "${WP_AGENT_TOKEN:-}" ]]; then
    $NON_INTERACTIVE && fail "WP_AGENT_TOKEN is required in non-interactive mode"
    WP_AGENT_TOKEN=$(openssl rand -hex 32)
    GENERATED_AGENT_TOKEN=true
  else
    GENERATED_AGENT_TOKEN=false
  fi
  [[ ${#WP_AGENT_TOKEN} -ge 24 ]] || fail "WP_AGENT_TOKEN must be at least 24 characters"
  if ! is_loopback_bind "$WP_AGENT_BIND"; then
    prompt_value WP_AGENT_PANEL_IP "Panel IP permitted to reach port 8443" ""
    require_value WP_AGENT_PANEL_IP
  fi
  prompt_value WP_AGENT_ACME_EMAIL "ACME email (optional)" ""
  prompt_value WP_AGENT_RESTIC_REPO "Default Restic repository (optional)" ""
  prompt_value PHP_VERSIONS "PHP versions to build" "8.2 8.3 8.4"
  prompt_value NGINX_MAINLINE "Install mainline Nginx (HTTP/3; true/false)" "false"
  [[ "$NGINX_MAINLINE" == true || "$NGINX_MAINLINE" == false ]] || fail "NGINX_MAINLINE must be true or false"
  for version in $PHP_VERSIONS; do
    [[ "$version" =~ ^[0-9]+\.[0-9]+$ ]] || fail "invalid PHP version: $version"
  done
  for value in "$WP_AGENT_BIND" "$WP_AGENT_TOKEN" "${WP_AGENT_PANEL_IP:-}" "${WP_AGENT_ACME_EMAIL:-}" "${WP_AGENT_RESTIC_REPO:-}"; do
    validate_single_line "agent configuration value" "$value"
  done

  if $PLAN_ONLY; then
    info "would install Docker, Nginx, MariaDB, Restic, certbot, PHP images, and the wp-agent service"
    if is_loopback_bind "$WP_AGENT_BIND"; then
      info "agent API would remain loopback-only at $WP_AGENT_BIND"
    else
      info "would permit only $WP_AGENT_PANEL_IP to reach agent port 8443 via UFW"
    fi
    return
  fi

  backup_existing_env /etc/wp-panel/agent.env
  WP_AGENT_TOKEN="$WP_AGENT_TOKEN" \
  WP_AGENT_BIND="$WP_AGENT_BIND" \
  WP_AGENT_PANEL_IP="${WP_AGENT_PANEL_IP:-}" \
  WP_AGENT_ACME_EMAIL="${WP_AGENT_ACME_EMAIL:-}" \
  WP_AGENT_RESTIC_REPO="${WP_AGENT_RESTIC_REPO:-}" \
  PHP_VERSIONS="$PHP_VERSIONS" \
  NGINX_MAINLINE="$NGINX_MAINLINE" \
  "$SCRIPT_DIR/install-agent.sh"

  if is_true "$GENERATED_AGENT_TOKEN"; then
    echo "Agent shared token (save it to attach this server): $WP_AGENT_TOKEN"
  fi
  if is_loopback_bind "$WP_AGENT_BIND"; then
    echo "Attach this local agent as https://127.0.0.1:8443 using the printed fingerprint."
  else
    echo "Port 8443 is restricted to $WP_AGENT_PANEL_IP; attach using the agent's reachable HTTPS URL and printed fingerprint."
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --role) ROLE=${2:-}; shift 2 ;;
    --non-interactive) NON_INTERACTIVE=true; shift ;;
    --plan) PLAN_ONLY=true; shift ;;
    --overwrite) OVERWRITE=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
done

choose_role
case "$ROLE" in panel|agent|all-in-one) ;; *) fail "--role must be panel, agent, or all-in-one" ;; esac
if ! $PLAN_ONLY && [[ $EUID -ne 0 ]]; then
  fail "run as root (for example: sudo deploy/install.sh)"
fi
if ! $PLAN_ONLY; then
  verify_supported_os
fi
verify_bundle

if $PLAN_ONLY; then
  info "valid release bundle for role '$ROLE' at $REPO_ROOT"
fi

case "$ROLE" in
  panel) install_panel ;;
  agent) install_agent ;;
  all-in-one)
    warn "all-in-one places a public panel and a root-privileged agent on one host. Keep the panel behind a VPN or IP allowlist."
    $NON_INTERACTIVE || confirm "Continue with the all-in-one installation?" || fail "installation cancelled"
    install_agent
    install_panel
    ;;
esac

if $PLAN_ONLY; then
  info "plan complete; no changes were made"
fi
