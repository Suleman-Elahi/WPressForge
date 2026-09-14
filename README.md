# WP Panel

Open-source control plane for hosting WordPress on your own Linux servers.
Rust + Axum + Askama + HTMX for the panel, a Rust daemon on each node, and
battle-tested pieces underneath: Docker, Nginx, MariaDB, Let's Encrypt, Restic,
WP-CLI.

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

All planned milestones (M0 through M7), testing foundation (X1), operational runbooks (`docs/OPERATIONS.md`), and wire protocol catalog (`docs/PROTOCOL.md`) are implemented, passing tests, and verified.

- **Security (M1):** Per-session CSRF protection, TLS fingerprint pinning, API tokens, sliding-window login rate limiting, and RFC 6238 TOTP 2FA.
- **WordPress Management (M2):** Live inspection and actions for plugins, themes, users, WP cron, and WP-CLI command runner.
- **Backups & Restoration (M3):** Encrypted offsite S3/MinIO destination credentials (AES-256-GCM), Restic snapshots, per-site backup schedule UI, and one-click restores.
- **Cloning & Staging (M4):** Automated site replication, staging environment isolation with safety backups, and production push pipeline.
- **Observability (M5):** Pure inline SVG sparklines for host and container telemetry, 30s auto-polling, log streaming, and threshold alerts.
- **Site Migration (M6):** Multi-step import wizard with SSH compatibility probes, rsync file migration, and automated DB search & replace.
- **Multi-Tenancy & Teams (M7):** Role-based access control (`owner`, `admin`, `operator`, `viewer`), granular per-site user assignments, invitation tokens, and async RFC 5321 SMTP outbound email.
- **Documentation & CI (X1):** Full operational disaster recovery guides, wire protocol documentation, and GitHub Actions CI suite.

Detailed architecture and milestone specifications are documented in [`docs/IMPLEMENTATION-PLAN.md`](./docs/IMPLEMENTATION-PLAN.md).

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).
