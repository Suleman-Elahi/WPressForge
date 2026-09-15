# WP Panel — Implementation Plan (post-scaffold)

Audience: an engineer or coding agent picking up the repository with no prior
context. Everything needed to implement the next milestones is here: exact file
paths, migration SQL, protocol changes, handler signatures, pseudocode, and the
commands that prove each milestone works.

Companion documents:

- Architecture and product vision: [`../# Open-Source GridPane Alternative — Rev.md`](../%23%20Open-Source%20GridPane%20Alternative%20%E2%80%94%20Rev.md)
- Operator-facing overview: [`../README.md`](../README.md)

Audited and hardened on 2026-09-15: M1–M7 and X1 are implemented and verified
working. Two rounds found and fixed 24 defects — 3 fatal on a fresh install, 6
that made a documented feature impossible. `../status` holds both defect tables,
the evidence, and the five remaining (non-blocking) items.

Gates now enforced: `cargo fmt --check`, `clippy -D warnings` with **zero**
suppressions anywhere in the tree, 97 tests across unit / repository / HTTP /
host layers.

Last verified against the code on 2026-09-14: `cargo check --workspace
--all-targets` clean, `cargo test --workspace` green (13 tests), the `--ignored`
`nginx -t` host test green against Nginx 1.26.3, and a panel → agent → job
round trip provisioning a site through a dry-run agent. If a file path or
signature in this document does not match the code, the code wins — fix the
document in the same commit.

---

## 0. How to use this document

1. Read §1 (current state) and §2 (conventions). §2 is the contract: follow the
   recipes there instead of inventing new patterns.
2. Pick the lowest-numbered unfinished milestone in §3–§9. Milestones are
   ordered by dependency, not by attractiveness. **M1 blocks everything that
   touches production**, because it closes real security gaps.
3. Work the milestone's task list top to bottom. Each task states the files it
   touches. Run `just check` after each task, `just test` before finishing.
4. Run the milestone's *Verification* block verbatim. If a command's output
   differs from what is written, the milestone is not done.
5. Tick the milestone's *Acceptance criteria* and update §10 (status board) in
   the same commit.

Rules that apply to every milestone:

- No new runtime dependency without a line in §11 (dependency ledger) saying why.
- No JavaScript framework, no client-side routing, no build step for assets.
- Every mutating user action goes through the job system (§2.3), never inline in
  a request handler.
- The panel never sends shell strings to a node. Only typed operations (§2.2).
- Every new privileged agent action must honour `Config::dry_run`.

---

## 1. Current state (verified inventory)

### 1.1 Crates

| Crate | Path | Binary | Role |
| --- | --- | --- | --- |
| `wp-common` | `crates/common` | — | Models, `fmt` helpers, panel↔agent protocol, shared `Error` |
| `wp-panel` | `crates/panel` | `wp-panel` | Web UI, JSON API, auth, SQLite, job workers |
| `wp-agent` | `crates/agent` | `wp-agent` | Privileged node operations |

### 1.2 Panel module map

```text
crates/panel/src/
├── main.rs        router assembly, layers, graceful shutdown, bootstrap
├── config.rs      clap Config (every field also an env var)
├── state.rs       AppState { db, config, agent, job_signal, started_at }
├── error.rs       AppError -> HTML/JSON responses, AppResult<T>
├── auth.rs        Argon2, sessions, require_session, require_api_auth, CurrentUser
├── agent.rs       AgentClient::send/ping/metrics
├── jobs.rs        plan(), spawn(), heartbeat_loop(), run(), simulate(),
│                  build_operation(), apply_effects()
├── api.rs         /api/v1 JSON handlers
├── db/
│   ├── mod.rs     connect() (WAL, busy_timeout), parse_ts(), now_string()
│   ├── users.rs   users + sessions
│   ├── servers.rs ServerRow, heartbeat recording
│   ├── sites.rs   SiteRow, NewSite, domains, backups
│   ├── jobs.rs    JobRow, enqueue/claim_next/progress/finish/steps
│   ├── audit.rs   audit_logs
│   └── seed.rs    bootstrap_admin(), demo_data()
└── web/
    ├── mod.rs     Chrome, render(), render_error(), redirect_with_flash(), routes()
    ├── pages.rs   dashboard, audit, settings, login/logout
    ├── servers.rs list/detail/new/create/delete/metrics_fragment
    ├── sites.rs   list/detail/new/create/action/php/cache/limits/domains/status_fragment
    └── jobs.rs    list/detail/active_fragment/progress_fragment
```

### 1.3 Agent module map

```text
crates/agent/src/
├── main.rs         config parse, dry-run warning, store open, serve
├── config.rs       bind, token, sites_root, cache_root, state_file, nginx_dir,
│                   uid_base, dry_run, restic_repo, acme_email
├── capabilities.rs NginxCapabilities::probe()/parse(): version + modules of the
│                   installed Nginx, so generated config always validates
├── api.rs          POST /v1/operations (bearer auth, protocol check, idempotency,
│                   single-permit semaphore), GET /healthz
├── exec.rs         run(dry_run, program, args) -> CommandOutput; Steps collector
├── store.rs        Store (JSON index of SiteRecord), next_uid()
├── state.rs        AgentState { config, store, web, lock }
└── ops/
    ├── mod.rs        dispatch(state, operation)
    ├── webserver.rs  enum WebServer: write_site/remove_site/reload/purge/kind
    │                 (the backend seam; Nginx is the only variant)
    ├── site.rs       create/delete/start/stop/restart/status/switch_php/
    │                 set_limits/add_domain/remove_domain/issue_certificate/
    │                 renew_certificate/set_cache/clear_cache/install_wordpress/
    │                 update_wordpress/backup/restore/tail_logs
    ├── docker.rs     start_php/stop/remove/restart/healthy/pull/exec_in/count
    ├── nginx.rs      NginxServer + pure render_vhost(config, caps, site, ssl)
    │                 and render_cache_zone(config, site), with unit tests per
    │                 Nginx version and an --ignored `nginx -t` host test
    ├── database.rs   create/drop/dump/import/generate_password/db_name
    ├── filesystem.rs ensure_user/create_tree/remove_tree/remove_user/disk_usage_mb
    ├── ssl.rs        verify_dns/issue/renew
    ├── backup.rs     create/restore/prune
    ├── wordpress.rs  install/update_core/version/flush_cache/search_replace/passthrough
    └── metrics.rs    collect()
```

### 1.4 Database tables (migration `migrations/0001_init.sql`)

`users`, `sessions`, `api_tokens`, `servers`, `sites`, `domains`,
`site_databases`, `jobs`, `job_steps`, `backup_destinations`, `backups`,
`backup_schedules`, `certificates`, `settings`, `audit_logs`.

Tables that exist but carry no data yet (verify with
`grep -rn "<table>" crates/ --include=*.rs`):

| Table | Current use | Wired up in |
| --- | --- | --- |
| `api_tokens` | none (mentioned in an `auth.rs` comment) | M1 §3.3 |
| `settings` | none (the `settings` hits in code are the page handler, not the table) | M3 (SMTP/global config) |
| `backup_destinations` | `LEFT JOIN` only, in `db::sites::backups` | M3 §5.5 |
| `backup_schedules` | none | M3 §5.6 |
| `certificates` | none (SSL state lives on `sites.ssl_*` today) | M1/M5 (history + expiry alerts) |
| `site_databases` | none (the agent derives names; the panel shows `Site::db_name()`) | M3 (dedicated DB tracking) |

### 1.5 Existing operations (`crates/common/src/protocol.rs`)

`ping`, `get_server_metrics`, `create_site`, `delete_site`, `start_site`,
`stop_site`, `restart_site`, `get_site_status`, `clone_site` *(agent returns
`Unsupported`)*, `switch_php`, `set_limits`, `add_domain`, `remove_domain`,
`issue_certificate`, `renew_certificate`, `set_cache`, `clear_cache`, `wp_cli`,
`install_wordpress`, `update_wordpress`, `create_backup`, `restore_backup`,
`list_backups` *(returns empty)*, `tail_logs`.

### 1.6 Existing job kinds (`JobKind` in `crates/common/src/models.rs`)

`site.create`, `site.delete`, `site.clone`*, `site.start`, `site.stop`,
`site.restart`, `php.switch`, `wordpress.install`*, `wordpress.update`,
`backup.create`, `backup.restore`*, `ssl.issue`, `ssl.renew`, `cache.clear`,
`staging.create`*, `staging.push`*.

`*` = enum exists and has a step plan, but `build_operation()` returns `None`
for it, so the worker falls through to the simulated path. Milestones M3/M4 fix
this.

### 1.7 Known gaps this plan closes

| # | Gap | Milestone |
| --- | --- | --- |
| G1 | No CSRF protection on state-changing forms | M1 |
| G2 | `AgentClient` sets `danger_accept_invalid_certs(true)` | M1 |
| G3 | API tokens rejected with "not enabled yet" | M1 |
| G4 | No login throttling; brute force is unbounded | M1 |
| G5 | No plugin/theme/user/cron management | M2 |
| G6 | Backups have no destination, schedule, or restore path | M3 |
| G7 | Clone/staging refuse loudly | M4 |
| G8 | Logs tab shows paths, not log content | M5 |
| G9 | Metrics are instantaneous only; no history, no alerts | M5 |
| G10 | No import of existing WordPress sites | M6 |
| G11 | Single user, no teams/roles enforcement | M7 |
| G12 | Thin test coverage (13 tests cover the Nginx renderer and capability parser; nothing covers repos, HTTP handlers or agent site flows) and no CI | Cross-cutting (§9.2, §9.3) |
| G13 | Cache purging is all-or-nothing; publishing a post drops the whole site cache | M2 §4.8 |
| G14 | No LiteSpeed/LSCache compatibility (deliberate; see §9.6) | M8, only on demand |

Closed already, listed so nobody re-introduces them:

| # | Was | Fix |
| --- | --- | --- |
| F1 | `http2 on;` emitted unconditionally — invalid before Nginx 1.25.1, so `nginx -t` failed on Debian 12 (1.22) and Ubuntu 24.04 (1.24), failing every `site.create` at the reload step | `crates/agent/src/capabilities.rs` probes `nginx -V`; `render_vhost` emits `listen 443 ssl http2;` below 1.25.1 |
| F2 | `brotli on;` emitted whenever `CacheSettings.brotli` was true (the default) — `ngx_brotli` is not in distro packages, same failure mode | Brotli block only when the module is present, otherwise `gzip_static on;` |
| F3 | Cache path hardcoded to `/var/cache/nginx` | `Config::cache_root` / `Config::cache_dir(domain)` |
| F4 | Agent panicked at startup: no rustls `CryptoProvider` could be selected (`rcgen` pulled `aws-lc-rs` alongside `ring`) | `rcgen` pinned to `ring`; both binaries install the provider explicitly |
| F5 | `CsrfKey::verify` compared the wrong values, so every mutating request was rejected | fixed and covered by 6 tests in `csrf.rs` |
| F6 | Certificate pinning was unusable: no form field, `agent_fingerprint` always `NULL` | field + normalisation + persistence; `https` without a pin and non-loopback `http` are refused |
| F7 | `user_for_session` omitted the TOTP columns, so 2FA could not be enabled | session query derives its columns from `COLUMNS`; test asserts arity |
| F8 | 2FA login was impossible and a session was created before the second factor | two-stage `login_submit`, signed 5-minute challenge, throttled |
| F9 | Seven `OperationData` list variants failed to serialise (internally tagged newtype around a `Vec`) | named fields + a round-trip test over every variant |
| F10 | `CloneForm.request_ssl: bool` rejected every checkbox submission | `Option<String>` + `checked()` |
| F11 | Clone/staging left the new site `provisioning` | `apply_effects` handles `StagingCreate` and resolves `target_site_id` |
| F12 | JSON API ignored RBAC and returned every site, server and job | all six handlers scope to the caller |
| F13 | Agent logged WordPress, restic and MySQL passwords in cleartext | `exec::redact_command` + 5 tests |

Second round, from closing the audit's open issues:

| # | Was | Fix |
| --- | --- | --- |
| F14 | Agent `ListBackups` returned an empty list; `backup::list` parsed restic's `snapshots --json` into a struct whose fields do not exist there | real `ResticSnapshot` DTO, wired into dispatch |
| F15 | "Sync from node" sent an empty restic target, ignored the reply, always flashed success | resolves the destination, idempotent upsert, honest counts |
| F16 | Import always failed with "site N is not managed by this agent" | `ImportSite` carries an `ImportRequest`; the agent builds the record on a first import |
| F17 | Import's search/replace was given the database name as the search string | reads the old URL via `wp option get siteurl` |
| F18 | Clone/staging copied the database with an empty password | `ops/wpconfig.rs` parses the source `wp-config.php` |
| F19 | `set_db_config` wrote unquoted PHP values (`--raw`) | `--raw` removed |
| F20 | A viewer posting a malformed form got 422 from the extractor, never reaching the role check | read-only enforced in `require_session` middleware |
| F21 | SSH and remote-DB passwords in hidden form fields and in `jobs.payload` | `CredentialStash` handle + `SecretBox`-sealed payload |
| F22 | Log grep stripped characters from the pattern; missing file was a raw error; no-match was an error | shell-quoting, `|| true`, empty state, template escaping |
| F23 | Worker/sync backups all labelled `local`; `repo_prefix` unused; simulated jobs recorded a fake 1.15 GB | destination fallback, single `restic_target` builder, honest zero sizes |

**Rule learned from F5/F7/F9/F12 and the whole second round:** a feature is not
done until it has been exercised over HTTP against a running panel *and* agent.
Everything above compiled, type-checked and passed the then-current suite.

**Corollary, learned from F14/F15/F16:** a blanket `#![allow(dead_code)]` hides
unimplemented features. The agent's dead-code warnings were pointing straight at
three of them. Never suppress lints crate-wide.

---

## 2. Conventions and recipes

Follow these exactly. They exist so that features look like they were written by
one person.

### 2.1 Recipe: add a database migration

1. Create `migrations/000N_short_name.sql` where `N` is the next integer.
   Never edit an applied migration; SQLite + sqlx verify checksums.
2. Statements are plain SQL, `PRAGMA foreign_keys = ON;` is already set by the
   pool. Use `TEXT` for timestamps (RFC 3339 written by `db::now_string()`),
   `INTEGER` for booleans (`0`/`1`), and enum wire strings from `wp-common`.
3. Add indexes for every column the UI filters or orders by.
4. Migrations run automatically on boot (`db::connect`). Verify with:
   `rm -rf /tmp/mig && WP_PANEL_DB=/tmp/mig/panel.db cargo run -p wp-panel`

### 2.2 Recipe: add a panel↔agent operation

Four files, in this order:

1. **`crates/common/src/protocol.rs`**
   - Add a variant to `enum Operation` (snake_case tag via existing
     `#[serde(tag = "operation", rename_all = "snake_case")]`).
   - Add its arm to `Operation::name()`.
   - If it returns data, add a variant to `enum OperationData`.
   - If the request has more than three fields, add a named struct next to
     `CreateSite`/`CloneSite` and use a tuple variant.
2. **`crates/agent/src/ops/mod.rs`** — add the `match` arm in `dispatch()`.
   The compiler will tell you if you forget; the match is exhaustive.
3. **`crates/agent/src/ops/<area>.rs`** — implement it. Signature convention:
   `pub async fn thing(state: &AgentState, ...) -> Result<OperationResult>`
   for site-scoped work in `site.rs`, or a narrow helper
   `pub async fn thing(config: &Config, ...) -> Result<T>` in the area module.
   Wrap each externally visible step in `Steps::step("Human label", future)`.
4. **`crates/panel/src/jobs.rs`** — map the job kind to the operation in
   `build_operation()`, and apply the resulting state change in
   `apply_effects()`.

Protocol compatibility: adding a variant is backwards compatible for the panel
(new panel + old agent → agent replies `Invalid`/unknown variant error). If you
must change or remove an existing variant, bump `PROTOCOL_VERSION` in
`crates/common/src/lib.rs`; the agent already rejects mismatched majors in
`crates/agent/src/api.rs`.

### 2.3 Recipe: add a job kind (user-triggered work)

1. **`crates/common/src/models.rs`**
   - Add the variant to `enum JobKind` with `#[serde(rename = "area.verb")]`.
   - Add arms to `JobKind::as_str()`, `JobKind::label()`, and add the variant to
     the `ALL` array inside `JobKind::parse()` (bump its array length).
2. **`crates/panel/src/jobs.rs`** — add a step plan to `plan()`. The plan drives
   the progress bar and the "pending steps" list in the UI, so the names should
   read like the agent's `Steps::step` labels.
3. **`crates/panel/src/web/<area>.rs`** — enqueue it:
   ```rust
   let job_id = db::jobs::enqueue(
       &state.db, JobKind::AreaVerb, Some(server_id), Some(site_id),
       Some(json!({ /* payload the agent needs */ })), &user.0.email,
   ).await?;
   state.notify_jobs();                 // wakes an idle worker immediately
   db::audit::record(&state.db, &user.0.email, JobKind::AreaVerb.as_str(),
                     &site.site.domain, Some(&format!("job {job_id}")), true).await?;
   Ok(redirect_with_flash(&format!("/jobs/{job_id}"), "Doing the thing"))
   ```
4. Wire the operation per §2.2.

Payload rule: the payload must contain everything the agent needs, because the
agent has no access to the panel database. Domain names, versions, limits and
snapshot ids all go in the payload.

### 2.4 Recipe: add a page

1. Handler in `crates/panel/src/web/<area>.rs`:
   ```rust
   #[derive(Template)]
   #[template(path = "area/page.html")]
   struct PageTemplate { chrome: Chrome, /* data */ }

   pub async fn page(
       State(state): State<AppState>,
       user: CurrentUser,
       Query(query): Query<FlashQuery>,
   ) -> AppResult<Response> {
       Ok(render(PageTemplate {
           chrome: Chrome::new(&state, &user, "sites", "Title", query.flash).await,
           /* data */
       }))
   }
   ```
   `section` must be one of `dashboard | sites | servers | jobs | audit |
   settings` so the sidebar highlights correctly.
2. Template in `templates/area/page.html` starting with
   `{% extends "base.html" %}{% block content %}`.
3. Route in `crates/panel/src/web/mod.rs::routes()`. Axum 0.8 path syntax is
   `{id}`, not `:id`.

Template rules (they prevent the two mistakes that already cost time):

- **No format filters.** Formatting lives in Rust: add a method to the model in
  `crates/common/src/models.rs` (e.g. `fn size_label(&self) -> String`) or a
  helper in `crates/common/src/fmt.rs`, then call it: `{{ backup.size_label() }}`.
- Comparing an iterated array element to a field needs `as_str()` on both sides
  (`{% let current = version.as_str() == site.site.php_version.as_str() %}`),
  because iteration yields references.
- Status colours come from `.tone()` methods, rendered as
  `<span class="pill {{ x.tone() }}">`.
- Mobile tables: `<table class="data stack">` plus `data-label="Column"` on each
  `<td>`.

### 2.5 Recipe: add an HTMX fragment

Fragments are for things that change without user input. Everything else is a
plain form POST + redirect.

```rust
#[derive(Template)]
#[template(path = "area/fragment.html")]
struct Fragment { /* data */ }

pub async fn fragment(State(state): State<AppState>, _user: CurrentUser,
                      Path(id): Path<i64>) -> AppResult<Response> {
    Ok(super::no_store(render(Fragment { /* data */ })))   // no_store is required
}
```

Route under `/partials/...`. In the parent template:

```html
<div hx-get="/partials/area/{{ id }}" hx-trigger="load, every 5s" hx-swap="innerHTML">
  ...server-rendered first paint...
</div>
```

Stop polling when there is nothing left to watch by emitting the `hx-trigger`
attribute conditionally, the way `templates/jobs/progress.html` does.

### 2.6 Error handling

- Handlers return `AppResult<Response>`. `AppError::NotFound`,
  `AppError::BadRequest(String)`, `From<sqlx::Error>`, `From<wp_common::Error>`,
  `From<anyhow::Error>` are available. 5xx is logged with `tracing::error!`.
- Agent code returns `wp_common::Result<T>`; use `Error::Invalid` for bad input,
  `Error::NotFound` for unknown site ids, `Error::Unsupported` for
  not-implemented, and let `exec::run` produce `Error::Command` for failures.
- Never `unwrap()` on I/O, DB or command results outside tests.

### 2.7 Security invariants (do not regress)

1. Arguments are passed to `exec::run` as a list. No `sh -c` with interpolated
   user data. (Two existing `sh -c` uses in `database.rs` interpolate only
   sanitised identifiers and agent-owned paths; if you touch them, keep it that way.)
2. WP-CLI subcommands stay whitelisted in `ops/wordpress.rs::ALLOWED_SUBCOMMANDS`.
3. Secrets (DB passwords, tokens, WordPress admin passwords) are never logged and
   never stored in the panel database — only references.
4. Site UIDs are unique and allocated monotonically; never reuse a UID while its
   files exist.
5. Anything reachable without a session must be explicitly listed in
   `main.rs::router()` (`/login`, `/logout`, `/healthz`, `/static`).
6. Never emit a web-server directive the host does not support. Gate it on
   `NginxCapabilities` (§2.8) and add a case to the tests in
   `crates/agent/src/ops/nginx.rs`. An invalid directive fails `nginx -t`, which
   fails the reload step, which fails the whole job.
7. Site operations call `state.web.*` (§2.9), never `nginx::*` directly.
8. Argv is logged through `exec::redact_command`, never `{args:?}`. Passwords
   reach commands as arguments in several places; the logger is the only thing
   standing between them and the journal.
9. Every JSON API handler scopes its query to the caller with
   `db::sites::list_for_user` / `get_for_user` / `db::teams::has_global_access`.
   The HTML routes are not the only path to the data.
10. Forms never bind a checkbox to `bool`. Use `Option<String>` plus `checked()`:
    a checkbox arrives as `on` or not at all, and `bool` rejects both.
11. Values interpolated into a `sh -c` fragment go through `exec::shell_quote`.
    That includes strings destined for a *remote* shell over SSH.
12. Authorisation that must hold for every request shape belongs in middleware,
    not a handler body: extractors run first, so a malformed body would answer
    422 before a handler-level role check is ever reached.
13. No crate-wide `allow` attributes. If a lint fires, fix it or annotate the one
    item, with a reason.

### 2.8 Recipe: gate output on host capabilities

`AgentState.web` holds the detected backend. For Nginx, `NginxCapabilities`
carries `version`, `http2_directive` (>= 1.25.1), `http3`
(`--with-http_v3_module`), `brotli` (`ngx_brotli`) and `cache_purge`
(`ngx_cache_purge`). `NginxCapabilities::CONSERVATIVE` is what we render when the
probe fails: output valid on every supported version.

To use a directive that is not universally available:

1. Add a field to `NginxCapabilities` and detect it in `parse()` (pure function,
   fed the combined stdout+stderr of `nginx -V`).
2. Add a unit test in `capabilities.rs` for at least Debian 12 (1.22.1),
   Ubuntu 24.04 (1.24.0) and a mainline build.
3. Branch on it in `render_vhost` and assert both branches in
   `ops::nginx::tests`, including in
   `conservative_output_avoids_every_optional_directive`.
4. Validate against a real binary on every OS you support:
   ```bash
   just test-nginx     # cargo test -p wp-agent -- --ignored nginx_accepts
   ```
   That test renders the http and https vhosts for both the detected and the
   conservative dialect, drops in a self-signed certificate and a temp prefix,
   and runs `nginx -t`. It is the check that would have caught F1 and F2.

Do not add a directive that requires a module the installer does not provide
without either (a) making the installer able to provide it, or (b) a fallback
path, like `gzip_static` standing in for brotli.

### 2.9 Recipe: add a web server backend

`crates/agent/src/ops/webserver.rs` is an enum, not a trait object: `async fn` in
traits is not dyn-compatible, and enum dispatch costs no dependency and no
allocation. The surface a backend must provide:

```rust
write_site(&SiteRecord, ssl: bool) -> Result<()>   // idempotent, includes cache config
remove_site(domain: &str)          -> Result<()>
reload()                           -> Result<()>   // must validate before applying
purge(domain: &str)                -> Result<()>
kind() -> &'static str;  version_label() -> String
```

Adding a variant makes the compiler list every place that needs a new arm. Site
operations never learn which backend is in use. See §9.6 before adding one.

### 2.10 Verification loop (run before declaring anything done)

```bash
just check                       # cargo check --workspace --all-targets
just test                        # cargo test --workspace
just test-nginx                  # renders vhosts and runs `nginx -t` on this host
just lint                        # clippy -D warnings

# Fresh-database smoke test
rm -rf /tmp/wp-verify && WP_PANEL_BIND=127.0.0.1:8099 \
  WP_PANEL_DB=/tmp/wp-verify/panel.db WP_PANEL_ADMIN_PASSWORD=verify-me-123 \
  cargo run -p wp-panel &
sleep 5
curl -s -c /tmp/cj -o /dev/null -w 'login=%{http_code}\n' \
  -X POST -d 'email=admin@localhost&password=verify-me-123' \
  http://127.0.0.1:8099/login
for p in / /sites /servers /jobs /audit /settings; do
  printf '%s=%s ' "$p" "$(curl -s -b /tmp/cj -o /dev/null -w '%{http_code}' \
    http://127.0.0.1:8099$p)"
done; echo

# Local dry-run agent, attached through the UI or API
WP_AGENT_TOKEN=local-development-token-000001 WP_AGENT_BIND=127.0.0.1:8443 \
  WP_AGENT_STATE=/tmp/wp-verify/agent.json cargo run -p wp-agent &
```

Note: panel logs disable ANSI when stderr is not a TTY, so
`sed -n 's/.*password=\([^ ]*\).*/\1/p'` works on redirected logs but not on a
terminal capture with colours.

---

## 3. M1 — Security foundations (blocks production use)

**Goal:** the panel can be exposed to the internet and the panel↔agent channel
can cross an untrusted network.

**Acceptance criteria**

- [ ] Every state-changing form and HTMX POST carries a CSRF token; requests
      without a valid token get `403` and are audit-logged.
- [ ] `AgentClient` verifies the agent's certificate against a per-server
      fingerprint; `danger_accept_invalid_certs` is gone from the codebase.
- [ ] `POST /api/v1/*` works with `Authorization: Bearer <api token>`; tokens are
      created and revoked in the UI and stored only as hashes.
- [ ] Six failed logins from one IP within 15 minutes lock further attempts for
      15 minutes; the lock is visible in the audit log.
- [ ] Optional TOTP 2FA can be enabled per user and is enforced at login.

### 3.1 CSRF protection

Files:

| Action | Path |
| --- | --- |
| add | `crates/panel/src/csrf.rs` |
| modify | `crates/panel/src/main.rs` (module decl, layer wiring) |
| modify | `crates/panel/src/web/mod.rs` (expose token to templates via `Chrome`) |
| modify | `templates/base.html`, every `<form method="post">` in `templates/**` |
| modify | `migrations/0002_security.sql` (nothing needed for CSRF; token is derived) |

Design: stateless double-submit with an HMAC-bound token, so no extra table.

```text
secret        = 32 random bytes generated at boot, kept in AppState
token(session) = base64url( hmac_sha256(secret, session_token || ":" || day_bucket) )
```

`crates/panel/src/csrf.rs` pseudocode:

```rust
pub struct CsrfKey([u8; 32]);                 // in AppState, generated in main()

impl CsrfKey {
    pub fn token(&self, session_token: &str) -> String;      // hmac + base64url
    pub fn verify(&self, session_token: &str, presented: &str) -> bool;
    //   accepts the current and previous day bucket so a form open across
    //   midnight still submits; constant-time compare.
}

/// Middleware: applied to the protected router only, after require_session.
pub async fn require_csrf(State(state): State<AppState>, request: Request, next: Next)
    -> Response
{
    if request.method().is_safe() { return next.run(request).await; }   // GET/HEAD

    let session = cookie_value(&request, auth::COOKIE_NAME);            // reuse auth.rs helper
    let presented = form_field_or_header(&mut request, "csrf_token", "x-csrf-token").await;

    if session.is_none() || !state.csrf.verify(session, presented) {
        audit::record(db, actor, "auth.csrf_reject", path, None, false).await;
        return (StatusCode::FORBIDDEN, render_error(403, "Invalid form token")).into_response();
    }
    next.run(request).await
}
```

Implementation notes:

- Reading a form field in middleware requires buffering the body. Use
  `axum::body::to_bytes(body, 64 * 1024)`, parse with
  `serde_urlencoded::from_bytes::<HashMap<String, String>>`, then rebuild the
  request with `Request::from_parts(parts, Body::from(bytes))`. Skip buffering
  when the `x-csrf-token` header is present (HTMX path).
- Make `auth::cookie_value` `pub(crate)` so `csrf.rs` can reuse it.
- Add `pub csrf: Arc<CsrfKey>` to `AppState`; add `pub csrf_token: String` to
  `Chrome` (computed in `Chrome::new`, which needs the session token — simplest
  path: store the session token in the request extension next to `CurrentUser`
  in `auth::require_session`, then read it in `Chrome::new` via a new
  `CurrentSession(String)` extractor).
- Every form gets a hidden input:
  ```html
  <input type="hidden" name="csrf_token" value="{{ chrome.csrf_token }}">
  ```
  There are 22 `method="post"` forms today; verify with
  `grep -rc 'method="post"' templates`. Counts at the time of writing:
  `sites/detail.html` 16 (one per action button, plus the cache, limits, php,
  domain-add, domain-remove and delete forms), `sites/new.html` 1,
  `servers/new.html` 1, `servers/detail.html` 1 (detach),
  `partials/sidebar.html` 1 (logout), `settings.html` 1 (logout),
  `login.html` 1 (exempt — no session exists yet).
  After the change, this must print `0`:
  ```bash
  grep -rL 'csrf_token' $(grep -rl 'method="post"' templates | grep -v login.html) | wc -l
  ```
- `/login` is exempt (no session yet) but gets rate limiting in §3.4.

### 3.2 Agent TLS with certificate pinning

Files:

| Action | Path |
| --- | --- |
| modify | `crates/agent/src/config.rs` (`--tls-cert`, `--tls-key`, `--tls-self-signed`) |
| add | `crates/agent/src/tls.rs` (load or generate cert, print SHA-256 fingerprint) |
| modify | `crates/agent/src/main.rs` (serve with `axum_server::tls_rustls`) |
| modify | `crates/panel/src/agent.rs` (per-server pinned verifier) |
| modify | `migrations/0002_security.sql` (`servers.agent_fingerprint TEXT`) |
| modify | `crates/panel/src/db/servers.rs` (`NewServer.fingerprint`, read into `ServerRow`) |
| modify | `templates/servers/new.html`, `crates/panel/src/web/servers.rs` (fingerprint field) |
| modify | `deploy/install-agent.sh` (generate cert, echo fingerprint) |

Agent side:

```rust
// tls.rs
pub fn load_or_generate(config: &Config) -> anyhow::Result<(RustlsConfig, String)> {
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let cfg = RustlsConfig::from_pem_file(cert, key).await?;
        return Ok((cfg, fingerprint_of(cert)?));
    }
    // self-signed, persisted next to the state file so the fingerprint is stable
    // across restarts: /var/lib/wp-agent/agent.crt + agent.key
    if !exists(cert_path) { generate_self_signed(&[hostname, ip]) }   // rcgen
    Ok((RustlsConfig::from_pem_file(...), fingerprint_of(cert_path)?))
}

pub fn fingerprint_of(pem: &Path) -> anyhow::Result<String>;  // "sha256:AB:CD:..."
```

Log the fingerprint at startup at `WARN` so the installer can capture it.

Panel side, replace the blanket accept:

```rust
// crates/panel/src/agent.rs
// One reqwest client per fingerprint, cached: building a client is expensive.
pub struct AgentClient {
    default: reqwest::Client,                      // for plain http (loopback dev)
    pinned: Arc<Mutex<HashMap<String, reqwest::Client>>>,   // fingerprint -> client
}

fn client_for(&self, fingerprint: Option<&str>) -> reqwest::Client {
    match fingerprint {
        None => self.default.clone(),              // http:// only; refuse https without a pin
        Some(fp) => cached_or_build(fp),           // rustls ClientConfig with a custom
                                                   // ServerCertVerifier comparing SHA-256
    }
}

pub async fn send(&self, server: &ServerConnection, op: Operation, job: Option<i64>)
    -> Result<OperationResult>
```

`ServerConnection { url, token, fingerprint }` replaces the current
`(&str, &str)` pairs — update the three call sites in
`crates/panel/src/jobs.rs` and `crates/panel/src/web/servers.rs`.

Rules:
- `https://` URL with no stored fingerprint → `Error::Invalid("pin the agent
  certificate first")`, surfaced on the attach form.
- `http://` is allowed only for `127.0.0.1`/`::1`; otherwise reject.
- Verifier compares the leaf certificate's SHA-256 only (no chain, no hostname):
  the token authenticates the panel, the pin authenticates the agent.

### 3.3 API tokens

Files:

| Action | Path |
| --- | --- |
| add | `crates/panel/src/db/tokens.rs` |
| modify | `crates/panel/src/db/mod.rs` (`pub mod tokens;`) |
| modify | `crates/panel/src/auth.rs` (`require_api_auth` accepts bearer) |
| modify | `crates/panel/src/web/pages.rs` (`settings` shows tokens; create/revoke) |
| modify | `templates/settings.html` |
| modify | `crates/panel/src/web/mod.rs` (routes `/settings/tokens`, `/settings/tokens/{id}/revoke`) |

Token format: `wpp_` + 32 random URL-safe bytes (`auth::random_token()`).
Store `sha256(token)` hex in `api_tokens.token_hash`; show the plaintext once in
a flash message. Lookup is a single indexed query, so hashing with SHA-256 (not
Argon2) is correct here — the token has full entropy.

```rust
// db/tokens.rs
pub struct ApiToken { pub id: i64, pub name: String, pub user_id: i64,
                      pub created_at: DateTime<Utc>, pub last_used_at: Option<DateTime<Utc>> }

pub async fn create(db: &Db, user_id: i64, name: &str) -> sqlx::Result<(i64, String)>;
pub async fn list(db: &Db, user_id: i64) -> sqlx::Result<Vec<ApiToken>>;
pub async fn revoke(db: &Db, user_id: i64, id: i64) -> sqlx::Result<()>;
pub async fn user_for_token(db: &Db, presented: &str) -> sqlx::Result<Option<User>>;
    // hash, SELECT ... JOIN users, then UPDATE last_used_at (fire and forget)
```

In `auth::require_api_auth`, replace the "api tokens not enabled yet" branch
with `tokens::user_for_token`. Keep the cookie branch first so the UI's own
`fetch` calls keep working.

### 3.4 Login throttling

Files: `crates/panel/src/auth.rs`, `migrations/0002_security.sql`.

```sql
CREATE TABLE login_attempts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ip         TEXT NOT NULL,
    email      TEXT NOT NULL,
    success    INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX login_attempts_ip_idx ON login_attempts(ip, created_at);
```

```rust
const WINDOW_MINUTES: i64 = 15;
const MAX_FAILURES: i64 = 6;

pub async fn is_locked(db: &Db, ip: &str) -> sqlx::Result<bool>;   // COUNT failures in window
pub async fn record_attempt(db: &Db, ip: &str, email: &str, success: bool);
```

`pages::login_submit`: if `is_locked` → `429` with the login template and
"Too many attempts. Try again in 15 minutes."; record every attempt; on success,
delete that IP's failures. Prune rows older than 24 h inside the existing
`heartbeat_loop` where `purge_expired_sessions` is already called.

Client IP: prefer `x-forwarded-for` first hop, fall back to the socket address
(add `ConnectInfo<SocketAddr>` to the handler and
`.into_make_service_with_connect_info::<SocketAddr>()` in `main.rs`).

### 3.5 TOTP 2FA (optional per user)

Files: `crates/panel/src/totp.rs` (add), `crates/panel/src/web/pages.rs`,
`templates/login.html`, `templates/settings.html`,
`migrations/0002_security.sql` (`users.totp_secret` already exists; add
`users.totp_confirmed_at TEXT`).

Flow:

```text
Settings → Enable 2FA
   → generate 20 random bytes, base32 encode, store in users.totp_secret
   → show otpauth:// URI + manual key (render QR client-side is out of scope:
     show the URI and the base32 secret as text)
   → user submits a 6-digit code → verify → set totp_confirmed_at
Login
   → password verified and totp_confirmed_at is not null
   → do NOT create a session; render login.html with a `stage=totp` variant and
     a signed, 5-minute HMAC "pending login" hidden field (reuse CsrfKey)
   → code verified → create session
```

Verification: RFC 6238, SHA-1, 6 digits, 30 s step, accept ±1 step. Implement in
`totp.rs` with `hmac` + `sha1` (add `sha1` to the ledger) or add the `totp-rs`
crate. Prefer the 40-line hand-rolled version over a new dependency.

### 3.6 Verification (M1)

```bash
# CSRF: a POST without a token must fail
curl -s -b /tmp/cj -o /dev/null -w 'no-csrf=%{http_code}\n' \
  -X POST http://127.0.0.1:8099/sites/1/actions/restart          # expect 403
TOK=$(curl -s -b /tmp/cj http://127.0.0.1:8099/sites/1 | grep -o 'name="csrf_token" value="[^"]*"' | head -1 | cut -d'"' -f4)
curl -s -b /tmp/cj -o /dev/null -w 'with-csrf=%{http_code}\n' \
  -X POST -d "csrf_token=$TOK" http://127.0.0.1:8099/sites/1/actions/restart   # expect 303

# Pinning: attaching an https agent without a fingerprint must be refused
curl -s -b /tmp/cj -o /dev/null -w 'unpinned=%{http_code}\n' -X POST \
  -d "csrf_token=$TOK&name=x&agent_url=https://10.0.0.9:8443&agent_token=$(head -c32 /dev/urandom | base64)" \
  http://127.0.0.1:8099/servers                                   # expect 422

# API token
# (create in Settings, then)
curl -s -H "Authorization: Bearer $WPP_TOKEN" http://127.0.0.1:8099/api/v1/sites | head -c 80

# Throttling
for i in $(seq 1 8); do
  curl -s -o /dev/null -w '%{http_code} ' -X POST \
    -d 'email=admin@localhost&password=wrong' http://127.0.0.1:8099/login
done; echo            # expect 401 x6 then 429 429

grep -c "danger_accept_invalid_certs" -r crates/            # expect 0
```

---

## 4. M2 — WordPress management

**Goal:** the WordPress tab does real work: plugins, themes, core, users, cron,
and a constrained WP-CLI console.

**Acceptance criteria**

- [ ] Plugin list with version, status, and available update, read live from the node.
- [ ] Activate / deactivate / update / delete a plugin, each as a job.
- [ ] Theme list with the same actions.
- [ ] Core version, update check, and one-click update (already exists as a job;
      now shows the target version).
- [ ] WordPress user list and password reset for the admin user.
- [ ] WP-Cron schedule listing and "run now"; option to disable WP-Cron and use
      a system cron on the node.
- [ ] WP-CLI console page that only accepts whitelisted subcommands and shows
      stdout/stderr.

### 4.1 Protocol additions

`crates/common/src/protocol.rs`:

```rust
// Read-only, executed synchronously from the request handler (fast, no job).
ListPlugins { site_id: i64 },
ListThemes  { site_id: i64 },
ListWpUsers { site_id: i64 },
ListCronEvents { site_id: i64 },
CoreCheckUpdate { site_id: i64 },

// Mutating, always via a job.
PluginAction { site_id: i64, slug: String, action: WpItemAction },
ThemeAction  { site_id: i64, slug: String, action: WpItemAction },
UpdateAllPlugins { site_id: i64 },
ResetWpPassword { site_id: i64, user_login: String },
RunCronEvent { site_id: i64, hook: String },
SetWpCron { site_id: i64, mode: CronMode },
```

```rust
#[derive(...)] #[serde(rename_all = "snake_case")]
pub enum WpItemAction { Activate, Deactivate, Update, Delete, Install }

#[derive(...)] #[serde(rename_all = "snake_case")]
pub enum CronMode { WpCron, SystemCron }   // SystemCron sets DISABLE_WP_CRON + a node cron entry
```

`OperationData` additions:

```rust
Plugins(Vec<PluginInfo>),
Themes(Vec<ThemeInfo>),
WpUsers(Vec<WpUserInfo>),
CronEvents(Vec<CronEventInfo>),
CoreUpdate { current: String, latest: Option<String> },
GeneratedPassword { user_login: String, password: String },   // never logged
```

New models in `crates/common/src/models.rs` (with `tone()`/`label()` helpers so
templates stay filter-free):

```rust
pub struct PluginInfo { pub name: String, pub slug: String, pub status: String,
                        pub version: String, pub update_version: Option<String>,
                        pub auto_update: bool }
pub struct ThemeInfo  { /* same shape */ }
pub struct WpUserInfo { pub id: i64, pub login: String, pub email: String, pub role: String }
pub struct CronEventInfo { pub hook: String, pub next_run_relative: String, pub schedule: String }
```

### 4.2 Agent implementation

`crates/agent/src/ops/wordpress.rs`:

```rust
pub async fn list_plugins(config: &Config, site: &SiteRecord) -> Result<Vec<PluginInfo>> {
    // wp plugin list --format=json --fields=name,status,version,update_version,auto_update
    let out = wp(config, &site.container_name(),
                 &["plugin", "list", "--format=json",
                   "--fields=name,status,version,update_version,auto_update"]).await?;
    if out.skipped { return Ok(demo_plugin_list()); }   // dry-run: stable fake data
    serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)
}

pub async fn plugin_action(config: &Config, site: &SiteRecord, slug: &str,
                           action: WpItemAction) -> Result<()> {
    validate_slug(slug)?;                 // ^[a-z0-9][a-z0-9._-]{0,62}$
    let verb = match action { Activate => "activate", Deactivate => "deactivate",
                              Update => "update", Delete => "delete", Install => "install" };
    wp(config, &site.container_name(), &["plugin", verb, slug]).await.map(|_| ())
}
```

Notes:

- `validate_slug` is mandatory for every caller-supplied identifier. Reject
  anything with `/`, `..`, whitespace or shell metacharacters.
- `wp cron event list --format=json --fields=hook,next_run_relative,schedule`.
- Password reset: `wp user update <login> --user_pass=<generated>` where the
  password comes from `database::generate_password()`; return it once in
  `OperationData::GeneratedPassword` and never write it to a log or the DB.
- `SetWpCron::SystemCron`: `wp config set DISABLE_WP_CRON true --raw`, then write
  `/etc/cron.d/wp-<domain>` running
  `docker exec -u <uid> <container> wp cron event run --due-now` every 5 minutes.
  Add `ops/cron.rs` for the crontab file writer (dry-run aware).

### 4.3 Panel: read-only lists (synchronous)

The plugin/theme lists must not go through the job system — they are reads. Add
a helper so handlers can call the agent directly with a short timeout:

```rust
// crates/panel/src/agent.rs
impl AgentClient {
    /// Read-only calls: 10 s timeout, no job, errors surface as a banner.
    pub async fn query(&self, conn: &ServerConnection, op: Operation)
        -> Result<OperationData, wp_common::Error>;
}
```

Handler pattern (`crates/panel/src/web/sites.rs`):

```rust
// GET /partials/sites/{id}/plugins   (fragment, so a slow node cannot block the page)
pub async fn plugins_fragment(...) -> AppResult<Response> {
    let site = db::sites::get(...).ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id).await?.ok_or(...)?;

    let result = state.agent.query(&server.connection(), Operation::ListPlugins { site_id }).await;
    let (plugins, error) = match result {
        Ok(OperationData::Plugins(list)) => (list, None),
        Ok(_)      => (vec![], Some("unexpected agent response".to_string())),
        Err(error) => (vec![], Some(error.to_string())),
    };
    Ok(no_store(render(PluginsFragment { site, plugins, error, csrf_token })))
}
```

The `wordpress` tab in `templates/sites/detail.html` renders a placeholder plus
`hx-get="/partials/sites/{{ id }}/plugins" hx-trigger="load"`. This keeps the
tab fast and degrades to an inline error banner when the node is unreachable.

### 4.4 Panel: job kinds

Add per §2.3:

| JobKind | wire name | plan steps |
| --- | --- | --- |
| `PluginActivate/Deactivate/Update/Delete` | `plugin.activate` etc. | `["Run WP-CLI", "Verify site responds"]` |
| `PluginUpdateAll` | `plugin.update_all` | `["Backup", "Update plugins", "Verify site responds"]` |
| `ThemeAction…` | `theme.*` | same as plugin |
| `WpUserPasswordReset` | `wordpress.reset_password` | `["Generate password", "Apply"]` |
| `CronRun` | `cron.run` | `["Run due events"]` |
| `CronModeSet` | `cron.mode` | `["Update wp-config", "Write system cron"]` |

For `plugin.update_all`, the "Backup" step is skipped with a note when the node
has no restic repository (mirror `site::update_wordpress`).

### 4.5 WP-CLI console

- Page: `templates/sites/console.html`, route `GET /sites/{id}/console`,
  `POST /sites/{id}/console`.
- The POST parses the input line with a strict tokenizer:
  ```rust
  fn tokenize(line: &str) -> Result<Vec<String>, String>
  //   splits on whitespace, supports single/double quoted args,
  //   rejects ; | & ` $ ( ) < > \n
  ```
- Send `Operation::WpCli { site_id, args }`; the agent already whitelists the
  subcommand. Render stdout/stderr in a `<pre class="log">`.
- Audit-log every executed command line (`wpcli.run`, target = domain,
  detail = the command).
- Rate limit: max 1 command per second per session (simple `Instant` in a
  `Mutex<HashMap<i64, Instant>>` in `AppState`, or reuse the login attempts
  table pattern).

### 4.6 Files touched (M2)

| Action | Path |
| --- | --- |
| modify | `crates/common/src/protocol.rs`, `crates/common/src/models.rs` |
| modify | `crates/agent/src/ops/mod.rs`, `ops/wordpress.rs`, `ops/site.rs` |
| add | `crates/agent/src/ops/cron.rs` |
| modify | `crates/panel/src/agent.rs` (`query`, `ServerConnection`) |
| modify | `crates/panel/src/jobs.rs` (`plan`, `build_operation`, `apply_effects`) |
| modify | `crates/panel/src/web/sites.rs`, `crates/panel/src/web/mod.rs` |
| add | `templates/sites/plugins.html`, `themes.html`, `wpusers.html`, `cron.html`, `console.html` |
| modify | `templates/sites/detail.html` (wordpress tab, new `cron`/`console` tabs) |
| add | `crates/agent/src/ops/mu_plugin.rs` (writes `wp-panel-cache.php`, §4.7) |
| modify | `crates/agent/src/ops/nginx.rs` (purge location gated on `caps.cache_purge`) |
| modify | `deploy/install-agent.sh` (optional `ngx_cache_purge` build) |

### 4.7 Cache purge quality (closes G13)

Today `clear_cache` deletes every cached page for the site, and nothing purges
automatically when content changes: an editor publishes a post and either sees a
stale page for up to `ttl_seconds`, or an operator clears the whole cache. This is
the one area where LSCache is genuinely ahead of a plain FastCGI cache (§9.6), and
it is fixable without changing web servers.

Two parts.

**Part 1 — targeted purge in Nginx.** Requires `ngx_cache_purge`, which
`NginxCapabilities.cache_purge` already detects. When present, add to the vhost:

```nginx
location ~ /wp-panel-purge(/.*) {
    allow 127.0.0.1;
    allow ::1;
    deny all;
    fastcgi_cache_purge <zone> "$scheme$request_method$host$1";
}
```

Gate the whole block on `caps.cache_purge`, and add both branches to the tests in
§2.8. The installer needs an option to build the module (dynamic module build
against the installed Nginx version, or the mainline package plus
`--add-dynamic-module`); when it is absent, keep today's directory-delete
behaviour as the fallback so nothing regresses.

**Part 2 — WordPress tells the panel what changed.** Ship a must-use plugin from
the agent (`wp-content/mu-plugins/wp-panel-cache.php`, written during
`create_site` and on `set_cache`):

```php
// Pseudocode. Hooks: save_post, deleted_post, comment_post, wp_set_comment_status,
// edited_term, switch_theme, woocommerce_product_set_stock.
// For each event, collect the URLs that changed:
//   permalink of the post, its archives, the feed, the home page,
//   and the REST/sitemap entries that reference it.
// Then POST the list to the agent's local purge endpoint:
//   http://127.0.0.1:<agent port>/v1/purge  with the site's shared secret,
//   or simply GET http://127.0.0.1/wp-panel-purge<path> per URL with Host set.
// Prefer the second: no new agent surface, and it works when the agent is busy.
```

Add `Operation::PurgeUrls { site_id, urls: Vec<String> }` for the panel-driven
case (an operator clicking "purge this page"), validating each URL against the
site's own domains before touching the cache.

**Acceptance for §4.7**

- [ ] Publishing a post purges that URL plus the home page within one second,
      with no full-cache flush.
- [ ] `X-Cache: HIT` returns on the second request to a cached page, and `MISS`
      on the first request after a purge (verify with `curl -I`).
- [ ] With `ngx_cache_purge` absent, behaviour matches today and no invalid
      directive is emitted.

### 4.8 Verification (M2)

```bash
# With a dry-run agent attached, the fragment must render the fake list, not 500
curl -s -b /tmp/cj http://127.0.0.1:8099/partials/sites/1/plugins | grep -c "<tr"
# Plugin action creates a job and reaches succeeded
curl -s -b /tmp/cj -o /dev/null -w '%{http_code}\n' -X POST \
  -d "csrf_token=$TOK&slug=akismet&action=deactivate" http://127.0.0.1:8099/sites/1/plugins
sleep 3 && curl -s -b /tmp/cj http://127.0.0.1:8099/api/v1/jobs | head -c 200
# Console rejects shell metacharacters
curl -s -b /tmp/cj -X POST -d "csrf_token=$TOK&command=plugin list; rm -rf /" \
  http://127.0.0.1:8099/sites/1/console | grep -c "not allowed"
```

---

## 5. M3 — Backups, destinations, retention, restore

**Goal:** scheduled, encrypted, off-site backups with a restore that works.

**Acceptance criteria**

- [ ] Backup destinations (S3/R2/B2/Wasabi/MinIO) managed in Settings; secrets
      stored encrypted at rest in the panel, decrypted only to hand to an agent.
- [ ] A site can be assigned a destination and a schedule with retention counts.
- [ ] A scheduler enqueues `backup.create` jobs when due, at most one per site.
- [ ] Backup list is real (from the agent's restic repository, cached in `backups`).
- [ ] Restore works for full / files-only / database-only and is a job with a
      confirmation step.
- [ ] Retention pruning runs after each backup and is reflected in the list.

### 5.1 Secret storage

The panel must hold destination credentials (the agent needs them per run).
Encrypt them; do not store plaintext.

Files: `crates/panel/src/secrets.rs` (add), `migrations/0003_backups.sql`.

```rust
/// AES-256-GCM with a key from WP_PANEL_SECRET_KEY (base64, 32 bytes).
/// If the variable is missing at boot, generate one, write it to
/// <db_dir>/secret.key with mode 0600, and warn once.
pub struct SecretBox { key: [u8; 32] }
impl SecretBox {
    pub fn seal(&self, plaintext: &str) -> String;    // base64(nonce || ciphertext)
    pub fn open(&self, sealed: &str) -> Result<String>;
}
```

Dependency: `aes-gcm` (ledger entry required). Alternative accepted: shell out
to nothing — do not invent crypto.

### 5.2 Schema

`migrations/0003_backups.sql`:

```sql
-- backup_destinations already exists; extend it.
ALTER TABLE backup_destinations ADD COLUMN access_key_id TEXT;
ALTER TABLE backup_destinations ADD COLUMN secret_sealed  TEXT;   -- SecretBox::seal
ALTER TABLE backup_destinations ADD COLUMN restic_password_sealed TEXT;
ALTER TABLE backup_destinations ADD COLUMN repo_prefix TEXT NOT NULL DEFAULT 'wp';

-- backup_schedules already exists; add scheduling state.
ALTER TABLE backup_schedules ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
ALTER TABLE backup_schedules ADD COLUMN interval_minutes INTEGER NOT NULL DEFAULT 1440;
ALTER TABLE backup_schedules ADD COLUMN last_run_at TEXT;
ALTER TABLE backup_schedules ADD COLUMN next_run_at TEXT;
CREATE INDEX backup_schedules_due_idx ON backup_schedules(enabled, next_run_at);

ALTER TABLE backups ADD COLUMN files_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE backups ADD COLUMN db_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE backups ADD COLUMN restic_repo TEXT;
```

Replace `cron` with `interval_minutes` for v1: a cron parser is a dependency and
a support burden; "every N minutes/hours/days" covers the stated retention model.
Keep the existing `cron` column unused, or drop it in the same migration.

### 5.3 Protocol

```rust
// Destination is passed per operation so the agent stores no long-lived secrets.
pub struct ResticTarget {
    pub repo: String,            // e.g. s3:s3.eu-central-1.amazonaws.com/bucket/wp/example.com
    pub password: String,        // restic repository password
    pub env: Vec<(String, String)>,  // AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, ...
}

CreateBackup { site_id, scope, target: ResticTarget, retention: RetentionPolicy },
RestoreBackup { site_id, snapshot_id, scope, target: ResticTarget },
ListBackups { site_id, target: ResticTarget },
InitBackupRepo { target: ResticTarget },        // restic init, idempotent
```

This changes three existing variants → **bump `PROTOCOL_VERSION` to 2** and
update `crates/agent/src/api.rs`'s expectation implicitly (it compares to the
constant, so no code change needed) plus the note in §2.2.

`exec::run` needs environment support for restic credentials:

```rust
pub async fn run_with_env<S: AsRef<OsStr>>(dry_run: bool, program: &str, args: &[S],
                                          env: &[(String, String)]) -> Result<CommandOutput>
// Log the command, log env KEYS ONLY, never values.
```

### 5.4 Agent

`crates/agent/src/ops/backup.rs`: replace `repo(config)` with the per-call
`ResticTarget`; keep `config.restic_repo` as a fallback default for
single-server installs.

```rust
pub async fn init(config: &Config, target: &ResticTarget) -> Result<()> {
    // restic init; treat "repository master key already initialized" as success
}

pub async fn list(config: &Config, site: &SiteRecord, target: &ResticTarget)
    -> Result<Vec<Backup>> {
    // restic snapshots --json --tag site=<domain>
    // map to wp_common::models::Backup { snapshot_id, size_bytes, scope from tags, created_at }
}

pub async fn restore(config: &Config, site: &SiteRecord, snapshot: &str,
                     scope: BackupScope, target: &ResticTarget) -> Result<()> {
    // 1. restic restore <snapshot> --target /tmp/restore-<site_id> (never straight to /)
    // 2. scope != DatabaseOnly: rsync -a --delete tmp/public_html/ <root>/public_html/
    // 3. scope != FilesOnly: mysql < tmp/backups/database.sql
    // 4. chown -R uid:uid <root>; rm -rf tmp
}
```

Restoring into a temporary directory first is the important change from the
scaffold: `restic restore --target /` would rewrite unrelated paths.

### 5.5 Panel: destinations UI

| Action | Path |
| --- | --- |
| add | `crates/panel/src/db/destinations.rs` |
| add | `crates/panel/src/web/destinations.rs` |
| add | `templates/settings/destinations.html`, `templates/settings/destination_new.html` |
| modify | `templates/settings.html` (link + summary), `web/mod.rs` (routes) |

Routes: `GET /settings/destinations`, `GET /settings/destinations/new`,
`POST /settings/destinations`, `POST /settings/destinations/{id}/delete`,
`POST /settings/destinations/{id}/test`.

"Test" enqueues `InitBackupRepo` against the site's server (or the first online
server) and reports success in a flash message.

### 5.6 Panel: scheduler

Add to `crates/panel/src/jobs.rs` (or a new `crates/panel/src/scheduler.rs`
spawned from `jobs::spawn`):

```rust
async fn scheduler_loop(state: AppState) {
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        for schedule in db::schedules::due(&state.db, Utc::now()).await? {
            // Skip if a backup job for this site is queued or running:
            if db::jobs::has_active(&state.db, schedule.site_id, JobKind::BackupCreate).await? {
                continue;
            }
            let payload = json!({ "scope": schedule.scope, "destination_id": schedule.destination_id });
            let job = db::jobs::enqueue(&state.db, JobKind::BackupCreate,
                                        Some(server_id), Some(schedule.site_id),
                                        Some(payload), "scheduler").await?;
            db::schedules::mark_scheduled(&state.db, schedule.id, Utc::now(),
                                          Utc::now() + Duration::minutes(schedule.interval_minutes)).await?;
        }
    }
}
```

New repo functions needed: `db::jobs::has_active(db, site_id, kind)`,
`db::schedules::{due, mark_scheduled, upsert, for_site}`.

`build_operation()` for `BackupCreate` now resolves the destination:
load `destination_id` from the payload (or the site's schedule), decrypt with
`SecretBox`, build `ResticTarget`, and include it. Because this needs async DB
access, change `build_operation` from a pure function to
`async fn build_operation(state: &AppState, job: &JobRow, payload: &Value) -> Option<Operation>`
and update its single call site in `run()`.

`apply_effects()` for `BackupCreate` already records the snapshot; extend it to
store `files_bytes`, `db_bytes` and `restic_repo`.

### 5.7 Panel: restore UI

In the `backups` tab of `templates/sites/detail.html`, replace the disabled
Restore button with a form that posts to
`POST /sites/{id}/backups/{snapshot}/restore` carrying `scope` and a
`data-confirm` string naming the site. Enqueue `JobKind::BackupRestore` with
`{"snapshot_id": ..., "scope": ...}` and redirect to the job page.

Add a "Sync from node" action that runs `ListBackups` synchronously
(`AgentClient::query`) and upserts rows into `backups` so the list survives a
panel database restore.

### 5.8 Verification (M3)

```bash
# Local MinIO makes this testable without cloud credentials
docker run -d --name minio -p 9000:9000 -e MINIO_ROOT_USER=minio \
  -e MINIO_ROOT_PASSWORD=minio123 minio/minio server /data
# Create destination in Settings: provider=minio endpoint=http://127.0.0.1:9000 bucket=wp
# Then, against a NON dry-run agent on a scratch VM:
#   1. create a site, 2. Back up now, 3. delete a file in public_html,
#   4. Restore (files only), 5. confirm the file returns.
curl -s -b /tmp/cj http://127.0.0.1:8099/api/v1/sites/1 | python3 -m json.tool | grep -i backup
sqlite3 data/panel.db "SELECT snapshot_id, scope, size_bytes FROM backups ORDER BY id DESC LIMIT 5;"
sqlite3 data/panel.db "SELECT id, next_run_at FROM backup_schedules;"
grep -rn "secret_sealed" crates/panel/src | head        # secrets never selected into logs
```

---

## 6. M4 — Cloning and staging

**Goal:** one click to create `staging.<domain>` from production, and a
controlled push back.

**Acceptance criteria**

- [ ] Clone creates a new site record, a new UID, a new database, copies files
      and DB, rewrites URLs, and lands `online`.
- [ ] Staging sites are visibly marked, excluded from search engines
      (`blog_public=0`), and default to no TLS if DNS is absent.
- [ ] Push staging → production takes a safety backup first and is refused if a
      backup destination is not configured.
- [ ] `Operation::CloneSite` no longer returns `Unsupported`.

### 6.1 Agent implementation

`crates/agent/src/ops/site.rs`, new `pub async fn clone(state, request: CloneSite)`:

```text
steps:
  1. "Validate source"        source record exists, target domain unused
  2. "Allocate system user"   filesystem::ensure_user(target_uid)
  3. "Create filesystem"      filesystem::create_tree
  4. "Copy files"             rsync -a --delete --exclude 'wp-content/cache/'
                              <src>/public_html/ <dst>/public_html/
  5. "Create database"        database::create(target record)
  6. "Copy database"          mysqldump <src_db> | mysql <dst_db>   (single sh -c,
                              identifiers are sanitised, paths are agent-owned)
  7. "Rewrite wp-config"      wp config set DB_NAME/DB_USER/DB_PASSWORD in target
  8. "Start container"        docker::start_php(target)
  9. "Search & replace URLs"  wordpress::search_replace(src_domain, dst_domain)
 10. "Staging hygiene"        if staging: wp option update blog_public 0
                              wp config set WP_ENVIRONMENT_TYPE staging
 11. "Write Nginx vhost"      nginx::write_vhost(target, ssl=false); reload
 12. "Health check"           docker::healthy
store.put(target record with status)
```

Push (`staging.push`) is the same sequence with source/target swapped, plus a
mandatory step 0: `backup::create(production, Full, target_from_payload)`.

Guard rails to implement:

- Refuse if source and target are the same site id or domain.
- Refuse push when the target is not `Environment::Production` or when the
  source's `parent_site_id` does not match the target.
- Never copy `wp-content/cache`, `wp-content/upgrade`, or `*.sql` dumps.

### 6.2 Panel

| Action | Path |
| --- | --- |
| modify | `crates/common/src/protocol.rs` (`CloneSite` gains `source_domain`, `staging: bool`, `target_uid` is agent-assigned) |
| modify | `crates/panel/src/jobs.rs` (`build_operation` for `SiteClone`/`StagingCreate`/`StagingPush`, `apply_effects` sets target `online`) |
| modify | `crates/panel/src/web/sites.rs` (`clone_form`, `clone_create`, `staging_create`, `staging_push`) |
| modify | `crates/panel/src/web/mod.rs` (routes) |
| add | `templates/sites/clone.html`, `templates/sites/staging.html` |
| modify | `templates/sites/detail.html` (add `staging` tab back into `TABS`) |

`web::sites::clone_create` pseudocode:

```rust
// 1. validate target domain (reuse valid_domain + domain_exists)
// 2. create the target site row in the panel first, status=provisioning,
//    environment = staging ? Staging : Production,
//    parent_site_id = Some(source.id) when staging,
//    php/limits/cache copied from the source
// 3. enqueue JobKind::SiteClone (or StagingCreate) with payload
//    { source_site_id, source_domain, target_site_id, target_domain,
//      staging, search_replace: true, request_ssl }
// 4. redirect to the job page
```

Staging domain default: `staging.<primary domain>`; if that already exists, add a
numeric suffix. Show the resulting domain in the form before submit.

### 6.3 Verification (M4)

```bash
# On a scratch VM with a non dry-run agent:
#  1. create example.test, 2. Staging → Create, 3. confirm staging.example.test
#     serves the same content and has blog_public=0, 4. change a post on staging,
#  5. Push to production, 6. confirm the change appears and a backup job ran first.
sqlite3 data/panel.db \
  "SELECT id, domain, environment, parent_site_id, status FROM sites;"
curl -s -b /tmp/cj http://127.0.0.1:8099/api/v1/jobs | \
  python3 -c "import json,sys;[print(j['kind'],j['status']) for j in json.load(sys.stdin)]"
```

---

## 7. M5 — Logs, metrics history, alerts

**Goal:** answer "what is wrong with this site right now" without SSH.

**Acceptance criteria**

- [ ] Logs tab streams the last N lines of any of the six log streams, with a
      follow toggle that polls every 3 s.
- [ ] Server metrics are stored every heartbeat and drawn as an inline SVG
      sparkline for 24 h (CPU, memory, disk).
- [ ] Per-site metrics (container CPU/RAM, PHP-FPM busy workers, cache hit
      ratio) appear on the site overview.
- [ ] Alert rules (disk > 85 %, certificate expiring < 14 days, site offline
      > 5 min, backup older than 48 h) produce entries in a notifications table
      and a badge in the top bar.

### 7.1 Log viewer

- `Operation::TailLogs` already exists and works. Add
  `grep: Option<String>` (validated: max 64 chars, no regex metacharacters
  unless you pass `-F` to grep — pass `-F`).
- Handler: `GET /partials/sites/{id}/logs?stream=nginx-error&lines=200`, returns
  a `<pre class="log">` fragment. Follow mode is
  `hx-trigger="load, every 3s"` toggled by a query parameter, so no JS.
- Escape log content (Askama escapes by default; do not use `|safe`).
- Cap `lines` at 2000 server-side; the agent already clamps to 5000.

### 7.2 Metrics history

`migrations/0004_monitoring.sql`:

```sql
CREATE TABLE server_metrics_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    server_id  INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    cpu        REAL NOT NULL,
    memory     REAL NOT NULL,
    disk       REAL NOT NULL,
    load_1m    REAL NOT NULL,
    sites      INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX smh_server_time_idx ON server_metrics_history(server_id, created_at);

CREATE TABLE site_metrics_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id     INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    cpu         REAL NOT NULL,
    memory_mb   INTEGER NOT NULL,
    php_busy    INTEGER NOT NULL DEFAULT 0,
    cache_hit_ratio REAL,
    created_at  TEXT NOT NULL
);
CREATE INDEX sitemh_site_time_idx ON site_metrics_history(site_id, created_at);

CREATE TABLE notifications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    severity   TEXT NOT NULL,               -- info | warning | critical
    rule       TEXT NOT NULL,               -- disk.high, ssl.expiring, site.offline, backup.stale
    target     TEXT NOT NULL,
    message    TEXT NOT NULL,
    resolved_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX notifications_open_idx ON notifications(resolved_at, created_at DESC);
```

Retention: in `heartbeat_loop`, delete history rows older than 30 days and
downsample rows older than 48 h to one per 15 minutes (keep `MIN`/`MAX`/`AVG`
of the bucket; simplest correct version: keep the row closest to each bucket
boundary and delete the rest).

Agent side: add `Operation::GetSiteMetrics { site_ids: Vec<i64> }` returning
`OperationData::SiteMetrics(Vec<SiteMetricSample>)`, implemented with
`docker stats --no-stream --format '{{json .}}'` plus the PHP-FPM status page and
`X-Cache` counters if available. Collect during the same heartbeat to avoid a
second round trip.

### 7.3 Sparklines without JavaScript

Render SVG in the template from precomputed points:

```rust
// crates/panel/src/web/servers.rs
pub struct Spark { pub points: String, pub last: String, pub tone: &'static str }

fn spark(samples: &[f32], width: f32, height: f32) -> Spark {
    // map i -> x = i * width / (n-1), value -> y = height - (v/100 * height)
    // join as "x,y x,y ..." for <polyline points="...">
}
```

```html
<svg viewBox="0 0 120 28" class="spark" role="img" aria-label="CPU, last 24 hours">
  <polyline points="{{ cpu.points }}" fill="none" stroke="currentColor" stroke-width="1.5"/>
</svg>
```

Add `.spark { width: 120px; height: 28px; color: var(--accent); }` to
`static/css/app.css`.

### 7.4 Alert evaluation

New `crates/panel/src/alerts.rs`, spawned from `jobs::spawn`, evaluated once per
minute:

```rust
struct Rule { id: &'static str, severity: &'static str }

// disk.high        server metrics disk_percent > 85
// ssl.expiring     sites.ssl_expires_at < now + 14 days
// site.offline     sites.status = 'failed' or server offline > 5 min
// backup.stale     max(backups.created_at) < now - 48h AND schedule enabled

for each firing rule:
    if no open notification with (rule, target): insert one
for each previously firing rule that no longer fires:
    set resolved_at = now
```

Top bar: extend `templates/jobs/active.html` (or add
`templates/partials/notifications.html` polled every 30 s) with a bell badge
linking to `/notifications`, plus a `GET /notifications` page listing open and
recently resolved items.

### 7.5 Verification (M5)

```bash
sqlite3 data/panel.db "SELECT COUNT(*) FROM server_metrics_history;"   # grows every 30s
curl -s -b /tmp/cj "http://127.0.0.1:8099/partials/sites/1/logs?stream=nginx-error&lines=50" | head -5
sqlite3 data/panel.db "UPDATE sites SET ssl_expires_at = datetime('now','+3 days') WHERE id=1;"
sleep 65 && sqlite3 data/panel.db "SELECT rule, severity, target FROM notifications WHERE resolved_at IS NULL;"
curl -s -b /tmp/cj http://127.0.0.1:8099/notifications | grep -c "ssl.expiring"
```

---

## 8. M6 — Import existing WordPress sites

**Goal:** take over a site running elsewhere with predictable downtime.

**Acceptance criteria**

- [ ] Import wizard accepts SSH (key or password), verifies connectivity, and
      reports discovered document root, WordPress version, PHP version and DB size.
- [ ] Import copies files and database, rewrites `wp-config.php`, provisions the
      site locally, and leaves DNS untouched.
- [ ] A "test URL" (`<site>.import.<panel-domain>` or hosts-file instructions)
      lets the operator verify before switching DNS.
- [ ] Re-sync can be run again to pick up changes made on the source since the
      first copy.

### 8.1 Protocol and agent

```rust
pub struct ImportSource {
    pub host: String, pub port: u16, pub user: String,
    pub auth: SshAuth,                  // PrivateKey(String) | Password(String)
    pub remote_path: String,            // document root
    pub db: Option<RemoteDb>,           // if None, read credentials from wp-config.php
}

InspectImportSource { source: ImportSource },
ImportSite { site_id: i64, source: ImportSource, resync: bool },
```

Agent implementation (`crates/agent/src/ops/import.rs`):

```text
inspect:
  ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new <user>@<host> \
      "cat <remote_path>/wp-includes/version.php; php -v; du -sm <remote_path>"
  parse $wp_version, PHP version, size; read DB creds from wp-config.php with
  a grep/sed pipeline (never eval the file)

import:
  1. rsync -az --delete --exclude wp-config.php --exclude wp-content/cache \
       -e "ssh -o BatchMode=yes" <user>@<host>:<remote_path>/ <dst>/public_html/
  2. ssh <host> "mysqldump --single-transaction <db>" > <dst>/backups/import.sql
  3. database::create(target) if not resync; mysql < import.sql
  4. wp config create (fresh, local credentials) — the source wp-config is never copied
  5. wp search-replace <old_url> <new_url> only when the domain changes
  6. wp core verify-checksums (report, do not fail the job)
  7. nginx vhost + reload + health check
```

Secrets: SSH keys and passwords are supplied by the operator per import, passed
in the operation, used in-memory, written to a `0600` temp file when ssh needs a
key file, and deleted in a `Drop` guard. Never persist them in the panel.

### 8.2 Panel

- Wizard as three POSTs, state carried in hidden fields (no server-side wizard
  state): `Source` → `Inspect result + options` → `Confirm`.
- Files: `crates/panel/src/web/imports.rs`, `templates/imports/{new,inspect}.html`,
  routes `/imports/new`, `/imports/inspect`, `/imports`.
- Job kinds: `import.inspect` is synchronous (`AgentClient::query`),
  `import.run` is a job with the plan above.

### 8.3 Verification (M6)

Requires two scratch VMs. Prove: import a stock WordPress from VM A to VM B,
site loads on the test URL, `wp core verify-checksums` clean, re-sync picks up a
new post created on A after the first import.

---

## 9. M7 and cross-cutting work

### 9.1 M7 — Teams, roles, multi-tenant hardening

- Enforce `users.role` (`owner`, `admin`, `operator`, `viewer`) in a
  `require_role(Role)` middleware; `viewer` cannot POST anything.
- `site_users` join table for per-site access; every site query gains a
  visibility filter. Do this before any multi-tenant deployment: retrofitting
  authorisation onto queries later is where security bugs live.
- Invitations by email token, password reset flow (needs SMTP config in
  `settings`).

### 9.2 Testing (start this during M1, not after)

| Layer | Tool | What to cover |
| --- | --- | --- |
| Unit | `#[cfg(test)]` in place | **done:** `capabilities::parse`, `nginx::render_vhost` + purge gating, `exec::redact_command`, `csrf` issue/verify/tamper, session column arity, `secrets` seal/open, TOTP vectors + base32, fingerprint normalisation, loopback detection, mu-plugin hooks, import dry-run, `OperationData` round-trip over every variant. **Todo:** `fmt::*`, `tokenize`, `valid_domain`, `filesystem::system_user` |
| Repo | `sqlx` against a temp file DB | **done:** `crates/panel/tests/repo.rs`, 18 tests over every `db::*` module, including the exact regression for F7. Use a temp *file*: `sqlite::memory:` is per-connection, so a pooled test silently talks to several empty databases. |
| HTTP | `tower::ServiceExt::oneshot` on the router | **done:** `crates/panel/tests/http.rs`, 18 tests covering CSRF accept/reject/cross-session, viewer read-only across endpoints and body shapes, operator grants, API auth and API scoping. F2, F9, F12 and F20 each have a test now. |
| Agent | fake `exec` | inject a command recorder so `site::create` can be asserted step-by-step without Docker |
| Host | `--ignored` tests | **done for Nginx:** `just test-nginx` renders both dialects and runs `nginx -t` with a temp prefix and self-signed cert. Run it on every OS you support before release |

To make agent code testable, change `exec::run` calls to go through a trait
object stored in `AgentState` (`Arc<dyn CommandRunner>`), with `RealRunner` and
`RecordingRunner`. This is a mechanical refactor worth doing at the start of M2.

Add `crates/panel/tests/http.rs` and `crates/agent/tests/ops.rs`. Target: every
new milestone ships tests for its pure functions and at least one end-to-end
router test.

Blanket `allow` attributes defeat the point of linting. They have been removed
(the tree now has zero suppressions) and `clippy -D warnings` passes; keep it
that way. Removing them is what exposed F14, F15 and F16.

### 9.3 CI

`.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -D warnings`,
`cargo test --workspace`, and a release build. Cache `~/.cargo` and `target`.
Add `cargo deny check` once the dependency ledger (§11) is populated.

### 9.4 Observability

- Structured logs already; add a `request_id` (tower-http `RequestId`) to the
  trace span and echo it in error pages so users can quote it.
- `/metrics` endpoint (Prometheus text format, behind API auth): job queue
  depth, job durations by kind, agent request failures, HTTP status counts.

### 9.5 Decision: web server strategy (Nginx now, OpenLiteSpeed only on demand)

**Decision.** Nginx stays the only web server. OpenLiteSpeed (OLS) and the
LiteSpeed Cache plugin are not adopted. Revisit only if users ask for LSCache or
QUIC.cloud compatibility by name.

**Why the question comes up.** LSCache's advantage is not raw server speed, it is
tag-aware full-page cache invalidation driven from inside WordPress. That is a
real gap in our stack, and §4.7 closes most of it with `ngx_cache_purge` plus a
must-use plugin.

**Why not adopt it.**

1. LSCache's caching features only work on a LiteSpeed-family server or through
   QUIC.cloud; on Nginx the plugin is limited to its general optimisation
   features ([plugin page](https://wordpress.org/plugins/litespeed-cache/),
   [LiteSpeed docs](https://docs.litespeedtech.com/lscache/lscwp/beginner/)).
   So "keep Nginx and add LSCache" is not an option — it is a stack swap.
   (Sources paraphrased; content rephrased for compliance with licensing terms.)
2. OLS runs PHP through its own LSAPI children rather than proxying to PHP-FPM.
   Our isolation model is one resource-limited PHP-FPM container per site with a
   dedicated UID, `cap-drop ALL` and a read-only rootfs. With OLS, PHP lives
   inside the web server process, so preserving isolation means one OLS container
   per site *plus* an edge proxy — the web server layer duplicated per site, at
   roughly 30–60 MB RSS each.
3. Everything downstream doubles: a second config generator (OLS uses its own
   config format), a second cache model, a second health check, a second PHP
   switch procedure, and every future feature tested twice. GridPane maintains
   two stacks and swaps the cache plugins when a site moves between them
   ([GridPane KB](https://gridpane.com/kb/openlitespeed-ols-caching-and-the-litespeed-cache-plugin-lscache/));
   that is a team-sized commitment.

**What was done instead of nothing.** The backend seam exists
(`ops/webserver.rs`, §2.9), so adding OLS later is additive rather than a
rewrite, and site operations already speak in terms of "write config, reload,
purge".

#### M8 (conditional) — OpenLiteSpeed backend

Only start this with a concrete user request. Scope if you do:

1. **Schema.** `migrations/000N_webserver.sql`:
   `ALTER TABLE servers ADD COLUMN web_server TEXT NOT NULL DEFAULT 'nginx';`
   Sites inherit the server's backend; mixing per site is out of scope.
2. **Agent config.** `--web-server nginx|openlitespeed`
   (`WP_AGENT_WEB_SERVER`). `WebServer::detect` picks the variant, probes the
   corresponding binary (`/usr/local/lsws/bin/openlitespeed -v`), and reports
   `kind()` in service health so the panel can show it.
3. **Backend.** `crates/agent/src/ops/ols.rs` implementing the §2.9 surface:
   - `write_site`: render the OLS virtual-host config plus its `.htaccess`-style
     rewrite rules; per-site `lsphp` version selection replaces the PHP-FPM
     container choice.
   - `reload`: `/usr/local/lsws/bin/lswsctrl restart` (there is no config-test
     equivalent, so validate by parsing the config yourself before writing, and
     health-check immediately after).
   - `purge`: LSCache purge via `PURGE` request or the plugin's purge-all hook.
4. **Container image.** `deploy/docker/openlitespeed/Dockerfile`: OLS + `lsphp`
   for each supported version + WP-CLI, one container per site, same UID/limits
   flags as the PHP-FPM image. Host Nginx (or HAProxy) stays as the TLS edge and
   routes by SNI to the per-site container.
5. **Cache plugin swap.** On backend change: install `litespeed-cache`, remove
   the Nginx purge mu-plugin, and vice versa. This must be a job
   (`site.migrate_stack`) with a backup step first.
6. **PHP switching.** Replaces container image rather than moving an upstream:
   start the new OLS container, health-check, switch the edge route, remove the
   old one. Same shape as `switch_php`, different mechanics.
7. **Tests.** An `--ignored` host test equivalent to `nginx_accepts_generated_config`
   that runs the OLS config checker, plus unit tests for the config renderer.

Estimated size: comparable to M2 + M3 combined. That is the cost being deferred.

### 9.6 Documentation to keep current

- `README.md` status section after each milestone.
- `docs/OPERATIONS.md` (to write during M3): backup/restore runbook, agent
  upgrade procedure, disaster recovery for the panel database.
- `docs/PROTOCOL.md` (to write during M2): generated table of operations, their
  payloads and their responses.

---

## 10. Status board

Update this table in the same commit that finishes a milestone.

| Milestone | Scope | State | Blocking |
| --- | --- | --- | --- |
| M0 | Scaffold: auth, servers, sites, jobs, agent ops, UI | **done** | — |
| M0.1 | Nginx capability probe, version-correct vhosts, `WebServer` seam | **done** | — |
| M1 | CSRF, TLS pinning, API tokens, login throttle, 2FA | **done, audited** | — |
| M2 | Plugins, themes, users, cron, WP-CLI console, purge mu-plugin | **done, audited** | — |
| M3 | Destinations, schedules, retention, restore | **done, audited** | — |
| M4 | Clone, staging, push | **done, audited** | — |
| M5 | Log viewer, metrics history, alerts | **done, audited** | — |
| M6 | Import existing sites | **done, audited** | full run needs two hosts (status §5 A) |
| M7 | Teams, roles, invitations | **done, audited** | — |
| M8 | OpenLiteSpeed backend (conditional, see §9.5) | not planned | explicit user demand |
| X1 | Tests + CI | **done**: 97 tests + 1 ignored, zero lint suppressions | — |

---

## 11. Dependency ledger

Every runtime dependency needs a row. Additions require a justification and a
note on what was rejected.

| Crate | Used by | Why | Considered instead |
| --- | --- | --- | --- |
| axum, tower-http | panel, agent | HTTP server, compression, static files | — |
| askama | panel | compiled templates, no runtime parsing | tera (slower, runtime errors) |
| sqlx (sqlite) | panel | async SQLite with migrations | rusqlite (blocking) |
| reqwest (rustls) | panel | agent client | hyper directly (more code) |
| argon2 | panel | password hashing | bcrypt (weaker defaults) |
| rand, base64, uuid, chrono, humantime | both | primitives | — |
| clap | both | flags + env in one place | hand-rolled parsing |
| tracing(-subscriber) | both | structured logs | log + env_logger |
| **hmac, sha2** | panel | CSRF token, API token hashing (M1) | new crate for CSRF |
| **sha1** | panel | RFC 6238 TOTP HMAC-SHA1 calculation (M1) | heavy all-in-one 2FA crate |
| **rcgen** | agent | self-signed agent certificate (M1) | shelling out to openssl |
| **rustls / axum-server** | agent, panel | TLS listener (M1) & fingerprint pinning | terminate TLS in Nginx (extra hop) |
| **rustls-pemfile, webpki-roots** | panel | Certificate parser & roots for TLS pinning | danger_accept_invalid_certs (rejected) |
| **aes-gcm** | panel | sealing destination secrets (M3) | storing plaintext (rejected) |

Rejected outright, do not add: any SPA framework, Kubernetes client, cron
expression parser (use `interval_minutes`), ORM, custom backup format,
OpenLiteSpeed/LSCache as a second stack (§9.5).

Note: the Nginx capability probe deliberately shells out to `nginx -V` and parses
the output rather than pulling in a version-parsing or config-generation crate.
Zero new dependencies, and the parser is unit tested against real `nginx -V`
output from Debian 12, Ubuntu 24.04 and mainline.

---

## 12. Appendix A — worked example: adding `plugin.update` end to end

This is the reference change set. Copy its shape for anything similar.

1. `crates/common/src/models.rs`
   ```rust
   // in JobKind
   #[serde(rename = "plugin.update")]
   PluginUpdate,
   // JobKind::as_str  -> Self::PluginUpdate => "plugin.update",
   // JobKind::label   -> Self::PluginUpdate => "Update plugin",
   // JobKind::parse   -> add to ALL and bump the array length
   ```
2. `crates/common/src/protocol.rs`
   ```rust
   PluginAction { site_id: i64, slug: String, action: WpItemAction },
   // Operation::name -> Self::PluginAction { .. } => "plugin_action",
   ```
3. `crates/agent/src/ops/mod.rs`
   ```rust
   Operation::PluginAction { site_id, slug, action } => {
       site::plugin_action(state, site_id, &slug, action).await
   }
   ```
4. `crates/agent/src/ops/site.rs`
   ```rust
   pub async fn plugin_action(state: &AgentState, site_id: i64, slug: &str,
                              action: WpItemAction) -> Result<OperationResult> {
       let config = &state.config;
       let record = state.store.get(site_id).await?;
       let mut steps = Steps::new();
       steps.step("Run WP-CLI", wordpress::plugin_action(config, &record, slug, action)).await?;
       steps.step("Verify site responds", docker::healthy(config, &record.container_name())).await?;
       Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
   }
   ```
5. `crates/panel/src/jobs.rs`
   ```rust
   // plan()
   JobKind::PluginUpdate => &["Run WP-CLI", "Verify site responds"],
   // build_operation()
   JobKind::PluginUpdate => Operation::PluginAction {
       site_id,
       slug: payload["slug"].as_str()?.to_string(),
       action: WpItemAction::Update,
   },
   // apply_effects(): nothing to persist; the plugin list is read live
   ```
6. `crates/panel/src/web/sites.rs`
   ```rust
   #[derive(Deserialize)] pub struct PluginForm { pub slug: String, pub action: String }

   pub async fn plugin_action(State(state): State<AppState>, user: CurrentUser,
                              Path(id): Path<i64>, Form(form): Form<PluginForm>)
       -> AppResult<Response>
   {
       let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
       let kind = match form.action.as_str() {
           "update" => JobKind::PluginUpdate,
           "activate" => JobKind::PluginActivate,
           other => return Err(AppError::BadRequest(format!("unknown action `{other}`"))),
       };
       let job_id = db::jobs::enqueue(&state.db, kind, Some(site.site.server_id), Some(id),
                                      Some(json!({ "slug": form.slug })), &user.0.email).await?;
       state.notify_jobs();
       db::audit::record(&state.db, &user.0.email, kind.as_str(), &site.site.domain,
                         Some(&form.slug), true).await?;
       Ok(redirect_with_flash(&format!("/sites/{id}?tab=wordpress"), "Plugin job queued"))
   }
   ```
7. `crates/panel/src/web/mod.rs`
   ```rust
   .route("/sites/{id}/plugins", post(sites::plugin_action))
   ```
8. `templates/sites/plugins.html` — a row per plugin with a small form per action
   (`csrf_token` hidden input included).
9. Tests: `plan()` returns a non-empty slice for the new kind;
   `tokenize`/`validate_slug` reject `../evil`; router test posts the form and
   expects `303` plus a queued job row.
10. Verify with the M2 commands, then update §10.

## 13. Appendix B — file-by-file change index

Quick lookup of which files each milestone touches. `+` add, `~` modify.

```text
M1  + crates/panel/src/csrf.rs, crates/panel/src/db/tokens.rs,
      crates/agent/src/tls.rs, migrations/0002_security.sql,
      crates/panel/src/totp.rs
    ~ panel: main.rs, state.rs, auth.rs, agent.rs, web/{mod,pages,servers}.rs,
      db/{mod,servers}.rs; agent: main.rs, config.rs;
      templates: base.html + every form; deploy/install-agent.sh; README.md

M2  + crates/agent/src/ops/cron.rs,
      templates/sites/{plugins,themes,wpusers,cron,console}.html
    ~ common: protocol.rs, models.rs; agent: ops/{mod,wordpress,site}.rs;
      panel: agent.rs, jobs.rs, web/{sites,mod}.rs; templates/sites/detail.html

M3  + crates/panel/src/secrets.rs, crates/panel/src/db/{destinations,schedules}.rs,
      crates/panel/src/web/destinations.rs, crates/panel/src/scheduler.rs,
      migrations/0003_backups.sql, templates/settings/destination*.html
    ~ common: protocol.rs (PROTOCOL_VERSION -> 2), models.rs;
      agent: exec.rs (run_with_env), ops/{backup,site,mod}.rs;
      panel: jobs.rs, db/{mod,sites,jobs}.rs, web/{mod,sites,pages}.rs;
      templates/sites/detail.html, templates/settings.html

M4  + crates/agent/src/ops/clone.rs (or extend site.rs),
      templates/sites/{clone,staging}.html
    ~ common: protocol.rs, models.rs; agent: ops/{mod,site}.rs;
      panel: jobs.rs, web/{mod,sites}.rs; templates/sites/detail.html

M5  + crates/panel/src/alerts.rs, crates/panel/src/db/metrics.rs,
      migrations/0004_monitoring.sql,
      templates/{notifications.html,partials/notifications.html,sites/logs.html}
    ~ common: protocol.rs, models.rs; agent: ops/{mod,metrics,site}.rs;
      panel: jobs.rs, web/{mod,servers,sites,pages}.rs; static/css/app.css

M6  + crates/agent/src/ops/import.rs, crates/panel/src/web/imports.rs,
      templates/imports/{new,inspect}.html
    ~ common: protocol.rs, models.rs; agent: ops/mod.rs;
      panel: jobs.rs, web/mod.rs, db/sites.rs

M8  + crates/agent/src/ops/ols.rs, deploy/docker/openlitespeed/Dockerfile,
      migrations/000N_webserver.sql
    ~ agent: ops/webserver.rs (new variant), config.rs, state.rs;
      panel: db/servers.rs, web/servers.rs, templates/servers/*.html
    (conditional — see §9.5)

M7  + migrations/0005_teams.sql, crates/panel/src/web/users.rs,
      templates/settings/users.html
    ~ panel: auth.rs (require_role), db/{users,sites,servers}.rs (visibility),
      web/* (role gates)
```
