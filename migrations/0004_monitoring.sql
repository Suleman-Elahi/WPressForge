-- M5: Server and site metrics history, notifications.

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
