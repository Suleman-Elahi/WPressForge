# WPressForge Panel ↔ Agent Wire Protocol Specification

This document is the definitive specification for the wire protocol between the WPressForge control plane (`wp-panel`) and worker node agents (`wp-agent`).

---

## 1. Wire Transport & Security

- **Endpoint**: `POST https://{agent_host}:{agent_port}/v1/operations`
- **Default Port**: `8443`
- **Authentication**: HTTP `Authorization: Bearer <node-token>`
- **Transport Security**: TLS 1.3 enforced via `rustls`. The panel verifies the SHA-256 fingerprint of the node's leaf certificate against the pinned fingerprint stored in the SQLite `servers` table (`cert_fingerprint`). Hostname verification is bypassed in favor of direct cryptographic identity pinning.
- **Protocol Version**: Current protocol version is `1` (`PROTOCOL_VERSION`).

---

## 2. Envelope Specification

Every request and response between the panel and agent is framed within standard JSON envelopes.

### 2.1 Request: `OperationEnvelope`

```json
{
  "protocol_version": 1,
  "job_id": 142,
  "request_id": "7bf3b934-8c88-466d-88f5-3746c1ce4556",
  "operation": {
    "operation": "create_site",
    "site_id": 42,
    "domain": "example.com",
    "php_version": "8.4",
    "database_mode": "shared",
    "limits": {
      "cpu_cores": 2.0,
      "memory_mb": 1024,
      "php_workers": 8
    },
    "cache": {
      "fastcgi_cache": true,
      "ttl_seconds": 3600,
      "redis_object_cache": false,
      "opcache": true,
      "brotli": true
    },
    "install_wordpress": true,
    "request_ssl": true,
    "wordpress": null
  }
}
```

| Field | Type | Description |
|---|---|---|
| `protocol_version` | integer | Must match the agent's supported protocol version (`1`). |
| `job_id` | integer (optional) | The panel job ID associated with this operation, echoed in agent log traces. |
| `request_id` | string (UUID) | Idempotency key. Replaying requests with the same key avoids duplicated side effects. |
| `operation` | object | The tagged operation variant with its payload. |

### 2.2 Response: `OperationResult`

```json
{
  "success": true,
  "error": null,
  "data": {
    "type": "site_created",
    "site_id": 42,
    "container_id": "wp-42-php",
    "uid": 10042,
    "db_name": "wp_42_site"
  },
  "steps": [
    { "name": "Allocate system user", "ok": true, "duration_ms": 12 },
    { "name": "Create filesystem", "ok": true, "duration_ms": 45 },
    { "name": "Create database", "ok": true, "duration_ms": 89 },
    { "name": "Start container", "ok": true, "duration_ms": 310 },
    { "name": "Write Nginx vhost", "ok": true, "duration_ms": 28 },
    { "name": "Health check", "ok": true, "duration_ms": 115 }
  ]
}
```

| Field | Type | Description |
|---|---|---|
| `success` | boolean | `true` if all planned steps completed without error; `false` otherwise. |
| `error` | object (optional) | Error details (`kind`, `message`, `operation`, `detail`) if failed. |
| `data` | object | The tagged [`OperationData`](#4-operationdata-catalog) variant returned by the operation. |
| `steps` | array of objects | Chronological step report records detailing operation execution times. |

---

## 3. Operation Catalog

### 3.1 Node & Health Operations

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `Ping` | `ping` | _none_ | `pong` | Liveness check; returns agent and protocol versions. |
| `GetServerMetrics` | `get_server_metrics` | _none_ | `metrics` | Queries CPU usage, RAM, disk space, and load averages. |
| `GetSiteMetrics` | `get_site_metrics` | `site_ids: [i64]` | `site_metrics` | Queries per-container CPU, RAM, and active worker count. |

### 3.2 Site Lifecycle Operations

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `CreateSite` | `create_site` | `site_id`, `domain`, `php_version`, `database_mode`, `limits`, `cache`, `install_wordpress`, `request_ssl`, `wordpress` | `site_created` | Allocates UID/GID, directory tree, database, container, and Nginx vhost. |
| `DeleteSite` | `delete_site` | `site_id: i64`, `keep_backups: bool` | `none` | Stops container, purges database, removes vhost, and deletes tree. |
| `StartSite` | `start_site` | `site_id: i64` | `none` | Starts the site's PHP-FPM container. |
| `StopSite` | `stop_site` | `site_id: i64` | `none` | Stops the site's PHP-FPM container. |
| `RestartSite` | `restart_site` | `site_id: i64` | `none` | Restarts the site's PHP-FPM container. |
| `GetSiteStatus` | `get_site_status` | `site_id: i64` | `site_status` | Returns container status, PHP version, WP version, and disk usage. |
| `CloneSite` | `clone_site` | `source_site_id`, `source_domain`, `target_site_id`, `target_domain`, `php_version`, `staging`, `search_replace`, `request_ssl` | `site_created` | Clones filesystem and database, replaces domains, and boots target. |

### 3.3 PHP & Resource Limits

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `SwitchPhp` | `switch_php` | `site_id: i64`, `version: string` | `none` | Swaps the container image/FPM socket to requested PHP version (8.1-8.4). |
| `SetLimits` | `set_limits` | `site_id: i64`, `limits: { cpu_cores, memory_mb, php_workers }` | `none` | Updates cgroup memory/CPU limits and adjusts PHP pool `pm.max_children`. |

### 3.4 Domains, SSL & Ingress

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `AddDomain` | `add_domain` | `site_id: i64`, `domain: string` | `none` | Adds domain alias to the site's Nginx `server_name` directive. |
| `RemoveDomain` | `remove_domain` | `site_id: i64`, `domain: string` | `none` | Removes domain alias from Nginx virtual host. |
| `IssueCertificate`| `issue_certificate`| `site_id: i64`, `domains: [string]` | `certificate` | Triggers ACME challenge via certbot/acme.sh and writes cert paths. |
| `RenewCertificate`| `renew_certificate`| `site_id: i64` | `certificate` | Renews certificates approaching expiry and reloads Nginx. |

### 3.5 Caching & Ingress Performance

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `SetCache` | `set_cache` | `site_id: i64`, `settings: CacheSettings` | `none` | Configures Nginx FastCGI microcache, Brotli, and Redis object cache. |
| `ClearCache` | `clear_cache` | `site_id: i64` | `none` | Purges site FastCGI cache files and flushes Redis keys. |
| `PurgeUrls` | `purge_urls` | `site_id: i64`, `urls: [string]` | `none` | Selectively purges cached URL paths from FastCGI cache. |

### 3.6 WordPress Operations (WP-CLI)

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `WpCli` | `wp_cli` | `site_id: i64`, `args: [string]` | `command_output` | Executes arbitary WP-CLI command as the site user inside the container. |
| `InstallWordpress`| `install_wordpress`| `site_id`, `site_title`, `admin_user`, `admin_email`, `admin_password`, `locale` | `none` | Runs `wp core download` and `wp core install`. |
| `UpdateWordpress` | `update_wordpress` | `site_id: i64` | `none` | Updates WordPress core via `wp core update`. |
| `ListPlugins` | `list_plugins` | `site_id: i64` | `plugins` | Lists installed plugins, versions, status, and update availability. |
| `PluginAction` | `plugin_action` | `site_id: i64`, `slug: string`, `action: string` | `none` | Activates, deactivates, updates, or deletes a plugin. |
| `UpdateAllPlugins`| `update_all_plugins`| `site_id: i64` | `none` | Runs bulk update for all installed plugins. |
| `ListThemes` | `list_themes` | `site_id: i64` | `themes` | Lists installed themes, versions, status, and active theme. |
| `ThemeAction` | `theme_action` | `site_id: i64`, `slug: string`, `action: string` | `none` | Activates, updates, or deletes a theme. |
| `ListWpUsers` | `list_wp_users` | `site_id: i64` | `wp_users` | Lists WordPress admin and content users with roles. |
| `ResetWpPassword` | `reset_wp_password` | `site_id: i64`, `user_login: string` | `generated_password` | Generates a cryptographically strong password and resets the user. |
| `ListCronEvents` | `list_cron_events` | `site_id: i64` | `cron_events` | Queries scheduled WP-Cron hooks and upcoming run schedules. |
| `RunCronEvent` | `run_cron_event` | `site_id: i64`, `hook: string` | `none` | Immediately triggers an overdue or scheduled WP-Cron event. |
| `SetWpCron` | `set_wp_cron` | `site_id: i64`, `mode: string` | `none` | Switches between web-triggered WP-Cron and system crontab. |

### 3.7 Backups & Snapshots (Restic)

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `InitBackupRepo` | `init_backup_repo` | `target: ResticTarget` | `none` | Initializes a new encrypted restic repository at destination. |
| `CreateBackup` | `create_backup` | `site_id`, `scope`, `target`, `retention` | `backup` | Creates encrypted restic snapshot and applies retention pruning. |
| `RestoreBackup` | `restore_backup` | `site_id`, `snapshot_id`, `scope`, `target` | `none` | Restores files or database dump from specified snapshot ID. |
| `ListBackups` | `list_backups` | `site_id: i64`, `target: ResticTarget` | `backups` | Fetches snapshot history, sizes, and timestamps from restic repository. |

### 3.8 Logs & Diagnostics

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `TailLogs` | `tail_logs` | `site_id: i64`, `stream: string`, `lines: u32`, `grep: Option<String>` | `lines` | Streams the last N lines from access, error, slow, or debug logs. |

### 3.9 Site Import Engine

| Operation Name | JSON Tag | Request Parameters | Response `data.type` | Description |
|---|---|---|---|---|
| `InspectImportSource` | `inspect_import_source` | `source: ImportSource` | `import_inspection` | Connects via SSH to inspect remote WordPress version, PHP, and size. |
| `ImportSite` | `import_site` | `site_id: i64`, `source: ImportSource`, `resync: bool` | `site_created` | Performs rsync transfer, remote mysqldump, and local provisioning. |

---

## 4. `OperationData` Catalog

Responses populate the `data` field with one of the following variant structures:

### `pong`
```json
{
  "type": "pong",
  "agent_version": "0.1.0",
  "protocol_version": 1
}
```

### `metrics`
```json
{
  "type": "metrics",
  "cpu_percent": 14.2,
  "memory_total_mb": 16384,
  "memory_used_mb": 4210,
  "disk_total_gb": 250,
  "disk_used_gb": 42,
  "load_1m": 0.45,
  "load_5m": 0.32,
  "load_15m": 0.18
}
```

### `site_metrics`
```json
{
  "type": "site_metrics",
  "samples": [
    { "site_id": 42, "cpu_percent": 2.1, "memory_mb": 128, "php_busy": 1 }
  ]
}
```

### `command_output`
```json
{
  "type": "command_output",
  "stdout": "Success: WordPress 6.7.1 updated.\n",
  "stderr": "",
  "exit_code": 0
}
```

### `lines`
```json
{
  "type": "lines",
  "lines": [
    "2026/09/15 01:23:45 [error] 1421#1421: *4 FastCGI sent in stderr...",
    "2026/09/15 01:24:10 [error] 1421#1421: *5 FastCGI sent in stderr..."
  ]
}
```

### `import_inspection`
```json
{
  "type": "import_inspection",
  "wp_version": "6.7.1",
  "php_version": "8.2.14",
  "size_mb": 1450,
  "db_name": "production_wp",
  "db_user": "wp_user"
}
```

---

## 5. Error Schema

When `success` is `false`, the `error` object adheres to the following structure:

```json
{
  "kind": "container_error",
  "message": "Failed to start PHP-FPM container wp-42-php",
  "operation": "start_site",
  "detail": "docker: Error response from daemon: port 9000 already allocated."
}
```

### Standard Error Kinds
- `validation_error`: Invalid inputs (domain syntax, out-of-range memory, unsupported PHP version).
- `container_error`: Docker/container daemon startup, cgroup, or runtime failure.
- `database_error`: MySQL connection, user grant, or dump execution failure.
- `filesystem_error`: User creation, quota exhaustion, or directory permission failure.
- `web_server_error`: Nginx configuration syntax test failure (`nginx -t`).
- `tls_error`: ACME challenge failure or certificate write error.
- `backup_error`: Restic repository lock, network timeout, or snapshot failure.
- `wordpress_error`: WP-CLI execution error.
- `import_error`: SSH authentication failure or rsync transfer interruption.
