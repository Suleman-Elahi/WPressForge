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
WP_AGENT_BIND="${WP_AGENT_BIND:-0.0.0.0:8443}"
PHP_VERSIONS="${PHP_VERSIONS:-8.2 8.3 8.4}"

echo "==> installing packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  ca-certificates curl gnupg nginx mariadb-server restic certbot \
  docker.io docker-compose-plugin ufw

echo "==> directory layout"
install -d -m 0755 /var/www /var/www/acme /var/cache/nginx /var/lib/wp-agent
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

cat > /etc/wp-panel/agent.env <<ENV
WP_AGENT_BIND=${WP_AGENT_BIND}
WP_AGENT_TOKEN=${WP_AGENT_TOKEN}
WP_AGENT_SITES_ROOT=/var/www
WP_AGENT_NGINX_DIR=/etc/nginx/sites-enabled
WP_AGENT_STATE=/var/lib/wp-agent/state.json
WP_AGENT_UID_BASE=10001
# Review the logged commands, then set this to false.
WP_AGENT_DRY_RUN=true
# WP_AGENT_RESTIC_REPO=s3:s3.amazonaws.com/my-wp-backups
# WP_AGENT_ACME_EMAIL=ops@example.com
WP_AGENT_LOG=info
ENV
chmod 0600 /etc/wp-panel/agent.env

echo "==> systemd unit"
install -m 0644 "$(dirname "$0")/systemd/wp-agent.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now wp-agent

echo "==> firewall"
ufw allow 80/tcp
ufw allow 443/tcp
echo "NOTE: expose ${WP_AGENT_BIND} only to the panel (private network or 'ufw allow from <panel-ip>')."

systemctl --no-pager status wp-agent | head -n 5
echo "done. Attach this server in the panel with the token you supplied."
