# WPressForge

Open-source WordPress server manager: a control plane for hosting WordPress on
your own Linux servers — a self-hosted alternative to RunCloud, GridPane and
EasyEngine. Rust + Axum + Askama + HTMX for the panel, a Rust daemon on each
node, and battle-tested pieces underneath: Docker, Nginx, MariaDB, Let's
Encrypt, Restic, WP-CLI.

The project ships two binaries: **`wp-panel`** (the control plane) and
**`wp-agent`** (the node agent).

Architecture and roadmap: [`# Open-Source GridPane Alternative — Rev.md`](./plans/%23%20Open-Source%20GridPane%20Alternative%20%E2%80%94%20Rev.md)

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
docs/       landing page (index.html)
plans/      design plan, deployment guide, operations runbook, wire protocol
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

The short version is below; [`plans/DEPLOYMENT.md`](./plans/DEPLOYMENT.md) has the
full production guide — service accounts, hardened env files, reverse proxy,
TLS pinning, firewalls, hardening checklist and rollback. If you want the panel
and the sites on **one** server, read [`plans/SINGLE-HOST.md`](./plans/SINGLE-HOST.md)
instead.

1. Build a release bundle with `cargo build --release --workspace`, preserving
   the `deploy/`, `static/`, and `target/release/` layout.
2. Run the interactive installer on a clean Ubuntu 24.04 or Debian 12 host:

   ```bash
   sudo deploy/install.sh                       # choose panel, agent, or both
   sudo deploy/install.sh --role all-in-one     # panel + loopback-only agent
   ```

   It installs the panel service, or Docker/Nginx/MariaDB/Restic/certbot plus the
   node agent, writes root-only environment files, and keeps every fresh agent in
   dry-run mode. For automation, use `--non-interactive` with the environment
   variables listed by `deploy/install.sh --help`.
3. For a separate node, provide its panel IP when prompted; the installer adds a
   UFW rule that allows only that IP to port 8443. Attach the agent using its TLS
   fingerprint, review dry-run logs, then explicitly set
   `WP_AGENT_DRY_RUN=false` and create a site.

The installer deliberately does not guess DNS ownership or issue the panel's
public certificate. Configure the panel Nginx TLS vhost afterward using
[`plans/DEPLOYMENT.md`](./plans/DEPLOYMENT.md). Expose only 80/443 publicly; the
agent's port must be panel-only (or loopback-only on an all-in-one host).

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
in [`plans/status`](./plans/status); a step-by-step production deployment guide is
in [`plans/DEPLOYMENT.md`](./plans/DEPLOYMENT.md), the engineering plan and
conventions are in [`plans/IMPLEMENTATION-PLAN.md`](./plans/IMPLEMENTATION-PLAN.md),
with operational runbooks in [`plans/OPERATIONS.md`](./plans/OPERATIONS.md) and the
wire protocol in [`plans/PROTOCOL.md`](./plans/PROTOCOL.md).

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).
