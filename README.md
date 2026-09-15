# WPressForge

Open-source WordPress server manager: a control plane for hosting WordPress on
your own Linux servers — a self-hosted alternative to RunCloud, GridPane and
EasyEngine. Rust + Axum + Askama + HTMX for the panel, a Rust daemon on each
node, and battle-tested pieces underneath: Docker, Nginx, MariaDB, Let's
Encrypt, Restic, WP-CLI.

The project ships two binaries: **`wp-panel`** (the control plane) and
**`wp-agent`** (the node agent).

Architecture and roadmap: [`# Open-Source GridPane Alternative — Rev.md`](./%23%20Open-Source%20GridPane%20Alternative%20%E2%80%94%20Rev.md)

```text
            Browser
               │  server-rendered HTML, ~10 KB CSS, one small JS file
               ▼
         ┌───────────┐
         │ wp-panel  │  Axum · Askama · HTMX · SQLite · job workers
         └─────┬─────┘
               │  typed operations over an authenticated HTTP API
      ┌────────┴────────┐
      ▼                 ▼
 ┌──────────┐     ┌──────────┐
 │ wp-agent │ ... │ wp-agent │   root on the node: Docker, Nginx, MariaDB,
 └────┬─────┘     └──────────┘   TLS, Restic, WP-CLI
      │
   PHP-FPM container per site (own UID, own CPU/RAM limits)
```

## Layout

```text
crates/
  common/   models + panel↔agent protocol (the only shared contract)
  panel/    web UI, JSON API, auth, SQLite, job workers
  agent/    privileged node operations
migrations/ SQLite schema for the panel
templates/  Askama templates (server-rendered HTML)
static/     CSS, ~2 KB of JS, vendored HTMX
deploy/     systemd units, Nginx globals, PHP image, agent installer
```

## Run it locally

```bash
# Panel with demo data on http://127.0.0.1:8080
WP_PANEL_ADMIN_PASSWORD=devpassword cargo run -p wp-panel

# In another shell: a dry-run agent that logs commands instead of executing them
WP_AGENT_TOKEN=local-development-token-000001 \
WP_AGENT_BIND=127.0.0.1:8443 \
WP_AGENT_STATE=data/agent-state.json \
cargo run -p wp-agent
```

Sign in with `admin@localhost` (or `WP_PANEL_ADMIN_EMAIL`). On first run the
panel creates the admin account, and logs a generated password once if you did
not supply one. `just dev` and `just agent` wrap both commands.

Attach the local agent under **Servers → Attach server** with
`http://127.0.0.1:8443` and the token above, then create a site. The agent
reports every step back to the panel; in dry-run mode it performs no changes to
the host.

## Production install

The short version is below; [`docs/DEPLOYMENT.md`](./docs/DEPLOYMENT.md) has the
full production guide — service accounts, hardened env files, reverse proxy,
TLS pinning, firewalls, hardening checklist and rollback. If you want the panel
and the sites on **one** server, read [`docs/SINGLE-HOST.md`](./docs/SINGLE-HOST.md)
instead.

1. Panel: build with `cargo build --release`, copy `wp-panel`, `templates/` is
   compiled in, `static/` is served from disk. Use
   `deploy/systemd/wp-panel.service` and put it behind Nginx with TLS.
   Set `WP_PANEL_SECURE_COOKIES=true` and `WP_PANEL_DEMO_DATA=false`.
2. Node: `WP_AGENT_TOKEN=... deploy/install-agent.sh` on a clean Ubuntu 24.04 or
   Debian 12 host. It installs Docker, Nginx, MariaDB, Restic and certbot, builds
   the PHP-FPM images, and starts the agent as a systemd service in dry-run mode.
3. Attach the node in the panel, flip `WP_AGENT_DRY_RUN=false`, create a site.

Expose only 80/443 publicly. The agent's port should be reachable from the panel
only (private network, VPN, or a firewall rule for the panel's IP).

## How it works

**Everything is a job.** Site creation, PHP switches, backups and certificate
issuance are rows in `jobs`, executed by workers, reported step by step, and
visible live in the UI. Interrupted jobs are marked failed on restart instead of
silently vanishing.

**The panel never runs shell commands on a node.** It sends typed operations
(`create_site`, `switch_php`, `create_backup`, ...) defined in
`crates/common/src/protocol.rs`. The agent owns every implementation, builds
argument vectors (never shell strings), and whitelists WP-CLI subcommands.

**Sites are isolated.** Dedicated Linux UID, `/var/www/<domain>` owned by it,
one PHP-FPM container per site with CPU, memory and PID limits, `cap-drop ALL`,
read-only rootfs, and either a private schema or a dedicated MariaDB container.

**PHP switches avoid downtime.** New container up, health-checked, Nginx
upstream moved, traffic verified, old container removed.

**Generated config matches the host.** The agent probes `nginx -V` at startup and
renders only directives the installed binary understands: `http2 on;` on 1.25.1+
and the `listen ... http2` parameter below it, HTTP/3 only with
`--with-http_v3_module`, brotli only with `ngx_brotli` (otherwise `gzip_static`).
`just test-nginx` renders both dialects and runs `nginx -t` against the Nginx on
your machine. Nginx is the only supported web server; the reasoning, including why
OpenLiteSpeed and LSCache are not adopted, is in the plan document §9.5.

**The UI is fast because it does very little.** One stylesheet, no webfonts, no
framework, no client-side router. HTMX polls only the fragments that change
(job progress, site status, server metrics), Brotli/gzip on responses, and
tables collapse into readable stacked rows on phones. Dark and light themes,
keyboard focus styles, and `prefers-reduced-motion` respected.

## Status

All planned milestones are implemented and verified: authentication with CSRF,
TLS-pinned agents, API tokens, login throttling and TOTP 2FA; sites with
per-site tabs, domains, cache and resource limits; WordPress management
(plugins, themes, users, cron, a constrained WP-CLI console); encrypted backups
to S3-compatible destinations with schedules, retention and restore; cloning,
staging and staging push; log viewing, metrics history with inline sparklines and
alerting; importing existing sites over SSH; teams with roles and per-site
grants; and a JSON API scoped to the caller.

Quality gates: `cargo fmt --check`, `clippy -D warnings` with no suppressions,
and 97 tests spanning unit, repository, HTTP-handler and real-`nginx -t` layers.

Node operations have so far been exercised with `WP_AGENT_DRY_RUN=true`
(every command logged, nothing executed) plus real Nginx config validation. The
first run against live Docker, MariaDB and certbot still needs a shakedown, and
the full import path needs two hosts to prove end to end.

Current state, the defect history behind it, and the remaining items are tracked
in [`status`](./status); a step-by-step production deployment guide is in
[`docs/DEPLOYMENT.md`](./docs/DEPLOYMENT.md), the engineering plan and
conventions are in [`docs/IMPLEMENTATION-PLAN.md`](./docs/IMPLEMENTATION-PLAN.md),
with operational runbooks in [`docs/OPERATIONS.md`](./docs/OPERATIONS.md) and the
wire protocol in [`docs/PROTOCOL.md`](./docs/PROTOCOL.md).

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).
