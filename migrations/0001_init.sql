-- WP Panel control-plane schema (SQLite).
-- Enum-ish columns are stored as TEXT using the wire values from wp-common.

PRAGMA foreign_keys = ON;

CREATE TABLE users (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    email           TEXT    NOT NULL UNIQUE,
    name            TEXT    NOT NULL DEFAULT '',
    password_hash   TEXT    NOT NULL,
    role            TEXT    NOT NULL DEFAULT 'owner',
    totp_secret     TEXT,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    last_login_at   TEXT
);

CREATE TABLE sessions (
    token       TEXT    PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_agent  TEXT    NOT NULL DEFAULT '',
    ip          TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at  TEXT    NOT NULL
);
CREATE INDEX sessions_user_idx ON sessions(user_id);

CREATE TABLE api_tokens (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    token_hash  TEXT    NOT NULL UNIQUE,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT
);

CREATE TABLE servers (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT    NOT NULL UNIQUE,
    agent_url       TEXT    NOT NULL,
    -- Bearer token the panel presents to the agent.
    agent_token     TEXT    NOT NULL DEFAULT '',
    hostname        TEXT    NOT NULL DEFAULT '',
    ip_address      TEXT    NOT NULL DEFAULT '',
    provider        TEXT,
    region          TEXT,
    status          TEXT    NOT NULL DEFAULT 'provisioning',
    agent_version   TEXT,
    metrics_json    TEXT,
    last_seen_at    TEXT,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE sites (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    server_id           INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    domain              TEXT    NOT NULL UNIQUE,
    title               TEXT,
    status              TEXT    NOT NULL DEFAULT 'provisioning',
    php_version         TEXT    NOT NULL DEFAULT '8.4',
    wp_version          TEXT,
    database_mode       TEXT    NOT NULL DEFAULT 'shared',
    environment         TEXT    NOT NULL DEFAULT 'production',
    parent_site_id      INTEGER REFERENCES sites(id) ON DELETE SET NULL,
    uid                 INTEGER NOT NULL,
    cpu_cores           REAL    NOT NULL DEFAULT 1.0,
    memory_mb           INTEGER NOT NULL DEFAULT 1024,
    php_workers         INTEGER NOT NULL DEFAULT 8,
    cache_fastcgi       INTEGER NOT NULL DEFAULT 1,
    cache_ttl_seconds   INTEGER NOT NULL DEFAULT 3600,
    cache_redis         INTEGER NOT NULL DEFAULT 0,
    cache_opcache       INTEGER NOT NULL DEFAULT 1,
    cache_brotli        INTEGER NOT NULL DEFAULT 1,
    ssl_enabled         INTEGER NOT NULL DEFAULT 0,
    ssl_issuer          TEXT    NOT NULL DEFAULT 'letsencrypt',
    ssl_auto_renew      INTEGER NOT NULL DEFAULT 1,
    ssl_expires_at      TEXT,
    disk_usage_mb       INTEGER NOT NULL DEFAULT 0,
    container_id        TEXT,
    created_at          TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX sites_server_idx ON sites(server_id);
CREATE INDEX sites_status_idx ON sites(status);

CREATE TABLE domains (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id             INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    name                TEXT    NOT NULL UNIQUE,
    is_primary          INTEGER NOT NULL DEFAULT 0,
    redirect_to_primary INTEGER NOT NULL DEFAULT 1,
    dns_ok              INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX domains_site_idx ON domains(site_id);

CREATE TABLE site_databases (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id     INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    username    TEXT    NOT NULL,
    -- Credentials live on the node; the panel keeps a reference only.
    secret_ref  TEXT    NOT NULL DEFAULT '',
    mode        TEXT    NOT NULL DEFAULT 'shared',
    size_mb     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX site_databases_site_idx ON site_databases(site_id);

CREATE TABLE jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    server_id   INTEGER REFERENCES servers(id) ON DELETE SET NULL,
    site_id     INTEGER REFERENCES sites(id) ON DELETE SET NULL,
    kind        TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'queued',
    progress    INTEGER NOT NULL DEFAULT 0,
    message     TEXT    NOT NULL DEFAULT '',
    payload     TEXT,
    error       TEXT,
    actor       TEXT    NOT NULL DEFAULT 'system',
    created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    started_at  TEXT,
    finished_at TEXT
);
CREATE INDEX jobs_status_idx ON jobs(status, id);
CREATE INDEX jobs_site_idx ON jobs(site_id, id DESC);

CREATE TABLE job_steps (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    ok          INTEGER NOT NULL DEFAULT 1,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    detail      TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX job_steps_job_idx ON job_steps(job_id, id);

CREATE TABLE backup_destinations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    provider    TEXT    NOT NULL,
    bucket      TEXT    NOT NULL,
    region      TEXT,
    endpoint    TEXT,
    secret_ref  TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE backups (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id         INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    destination_id  INTEGER REFERENCES backup_destinations(id) ON DELETE SET NULL,
    snapshot_id     TEXT    NOT NULL,
    scope           TEXT    NOT NULL DEFAULT 'full',
    size_bytes      INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX backups_site_idx ON backups(site_id, created_at DESC);

CREATE TABLE backup_schedules (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id         INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    destination_id  INTEGER REFERENCES backup_destinations(id) ON DELETE SET NULL,
    cron            TEXT    NOT NULL DEFAULT '0 3 * * *',
    keep_hourly     INTEGER NOT NULL DEFAULT 24,
    keep_daily      INTEGER NOT NULL DEFAULT 14,
    keep_weekly     INTEGER NOT NULL DEFAULT 8,
    keep_monthly    INTEGER NOT NULL DEFAULT 12,
    enabled         INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE certificates (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    site_id     INTEGER NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    domains     TEXT    NOT NULL,
    issuer      TEXT    NOT NULL DEFAULT 'letsencrypt',
    issued_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    expires_at  TEXT
);

CREATE TABLE settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE audit_logs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    actor       TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    target      TEXT    NOT NULL DEFAULT '',
    detail      TEXT,
    success     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX audit_logs_created_idx ON audit_logs(created_at DESC);
