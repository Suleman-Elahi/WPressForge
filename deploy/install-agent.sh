#!/usr/bin/env bash
# Prepares a clean Ubuntu 24.04 / Debian 12 host to run wp-agent.
#
#   WP_AGENT_TOKEN=<token from the panel> ./install-agent.sh
#
# Installs Docker, Nginx, MariaDB, Restic and certbot, lays out /var/www, then
# installs the agent as a systemd service. The agent starts in dry-run mode:
# flip WP_AGENT_DRY_RUN=false in /etc/wp-panel/agent.env once you have reviewed
# the logged commands.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "run as root" >&2
  exit 1
fi

: "${WP_AGENT_TOKEN:?set WP_AGENT_TOKEN to the token shown by the panel}"
if [[ ${#WP_AGENT_TOKEN} -lt 24 ]]; then
  echo "WP_AGENT_TOKEN must be at least 24 characters" >&2
  exit 1
fi
WP_AGENT_BIND="${WP_AGENT_BIND:-0.0.0.0:8443}"
WP_AGENT_PANEL_IP="${WP_AGENT_PANEL_IP:-}"
WP_AGENT_ACME_EMAIL="${WP_AGENT_ACME_EMAIL:-}"
WP_AGENT_RESTIC_REPO="${WP_AGENT_RESTIC_REPO:-}"
PHP_VERSIONS="${PHP_VERSIONS:-8.2 8.3 8.4}"
# Distro Nginx (Debian 12: 1.22, Ubuntu 24.04: 1.24) has no HTTP/3 and no
# `http2 on;`. The agent detects this and renders compatible config either way,
# so mainline is optional. Set NGINX_MAINLINE=true to get HTTP/3 support.
NGINX_MAINLINE="${NGINX_MAINLINE:-false}"

if [[ "$NGINX_MAINLINE" != "true" && "$NGINX_MAINLINE" != "false" ]]; then
  echo "NGINX_MAINLINE must be true or false" >&2
  exit 1
fi
for version in $PHP_VERSIONS; do
  if [[ ! "$version" =~ ^[0-9]+\.[0-9]+$ ]]; then
    echo "invalid PHP version: $version" >&2
    exit 1
  fi
done

for value in "$WP_AGENT_BIND" "$WP_AGENT_TOKEN" "$WP_AGENT_PANEL_IP" "$WP_AGENT_ACME_EMAIL" "$WP_AGENT_RESTIC_REPO"; do
  if [[ "$value" == *$'\n'* || "$value" == *$'\r'* ]]; then
    echo "configuration values must be single-line" >&2
    exit 1
  fi
done

if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}:${VERSION_ID:-}" in
    debian:12|ubuntu:24.04) ;;
    *)
      echo "unsupported OS: ${PRETTY_NAME:-unknown}; supported: Debian 12, Ubuntu 24.04" >&2
      exit 1
      ;;
  esac
fi

echo "==> installing packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  ca-certificates curl gnupg mariadb-server restic certbot \
  docker.io docker-compose-plugin ufw

if [[ "$NGINX_MAINLINE" == "true" ]]; then
  echo "==> nginx from nginx.org (mainline: HTTP/3, http2 directive)"
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://nginx.org/keys/nginx_signing.key |
    gpg --dearmor -o /etc/apt/keyrings/nginx.gpg
  distro=$(. /etc/os-release && echo "$ID")
  codename=$(. /etc/os-release && echo "$VERSION_CODENAME")
  echo "deb [signed-by=/etc/apt/keyrings/nginx.gpg] https://nginx.org/packages/mainline/${distro} ${codename} nginx" \
    > /etc/apt/sources.list.d/nginx.list
  apt-get update -qq
fi
apt-get install -y --no-install-recommends nginx

echo "==> directory layout"
install -d -m 0755 /var/www /var/www/acme /var/lib/wp-agent
# Cache root: one subdirectory per site, created by nginx on first use.
install -d -m 0755 -o www-data -g www-data /var/cache/nginx
install -d -m 0750 /etc/wp-panel

echo "==> nginx global config"
install -m 0644 "$(dirname "$0")/nginx/wp-panel-global.conf" /etc/nginx/conf.d/
rm -f /etc/nginx/sites-enabled/default
nginx -t && systemctl reload nginx

echo "==> php-fpm images"
for version in $PHP_VERSIONS; do
  docker build \
    --build-arg "PHP_VERSION=${version}" \
    -t "wp-panel/php-fpm:${version}" \
    "$(dirname "$0")/docker/php-fpm"
done

echo "==> agent binary"
install -m 0755 "$(dirname "$0")/../target/release/wp-agent" /usr/local/bin/wp-agent

umask 077
cat > /etc/wp-panel/agent.env <<ENV
WP_AGENT_BIND=${WP_AGENT_BIND}
WP_AGENT_TOKEN=${WP_AGENT_TOKEN}
WP_AGENT_SITES_ROOT=/var/www
WP_AGENT_NGINX_DIR=/etc/nginx/sites-enabled
WP_AGENT_CACHE_ROOT=/var/cache/nginx
WP_AGENT_STATE=/var/lib/wp-agent/state.json
WP_AGENT_UID_BASE=10001
# Review the logged commands, then set this to false.
WP_AGENT_DRY_RUN=true
ENV
[[ -n "$WP_AGENT_RESTIC_REPO" ]] && printf 'WP_AGENT_RESTIC_REPO=%s\n' "$WP_AGENT_RESTIC_REPO" >> /etc/wp-panel/agent.env
[[ -n "$WP_AGENT_ACME_EMAIL" ]] && printf 'WP_AGENT_ACME_EMAIL=%s\n' "$WP_AGENT_ACME_EMAIL" >> /etc/wp-panel/agent.env
printf '%s\n' 'WP_AGENT_LOG=info' >> /etc/wp-panel/agent.env
chmod 0600 /etc/wp-panel/agent.env

echo "==> systemd unit"
install -m 0644 "$(dirname "$0")/systemd/wp-agent.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now wp-agent

echo "==> firewall"
ufw allow 80/tcp
ufw allow 443/tcp
if [[ -n "$WP_AGENT_PANEL_IP" ]]; then
  ufw allow from "$WP_AGENT_PANEL_IP" to any port 8443 proto tcp
  echo "Agent port 8443 is restricted to ${WP_AGENT_PANEL_IP}."
elif [[ "$WP_AGENT_BIND" == 127.0.0.1:* || "$WP_AGENT_BIND" == "[::1]:"* || "$WP_AGENT_BIND" == localhost:* ]]; then
  echo "Agent is loopback-only at ${WP_AGENT_BIND}; no UFW rule was added for 8443."
else
  echo "NOTE: expose ${WP_AGENT_BIND} only to the panel (private network or 'ufw allow from <panel-ip>')."
fi

systemctl --no-pager status wp-agent | head -n 5

echo "==> agent TLS certificate fingerprint"
# The panel pins this value; without it an https agent cannot be attached.
if [[ -f /var/lib/wp-agent/agent.crt ]]; then
  fp=$(openssl x509 -in /var/lib/wp-agent/agent.crt -noout -fingerprint -sha256 |
       sed 's/.*=//')
  echo "  sha256:${fp}"
  echo "  ^ paste this into the panel's \"Agent certificate fingerprint\" field"
else
  echo "  not generated yet; run: journalctl -u wp-agent | grep fingerprint"
fi

echo "==> detected web server capabilities"
# The agent logs the same probe at startup; print it here so the operator knows
# what the generated vhosts will contain.
nginx -v 2>&1
if nginx -V 2>&1 | grep -q -- --with-http_v3_module; then
  echo "  HTTP/3: available"
else
  echo "  HTTP/3: not available (re-run with NGINX_MAINLINE=true to enable)"
fi
if nginx -V 2>&1 | grep -qi brotli; then
  echo "  brotli: available"
else
  echo "  brotli: not available (gzip only; ngx_brotli must be compiled in)"
fi

echo "done. Attach this server in the panel with the token you supplied."
