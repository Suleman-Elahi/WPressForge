//! SQLite persistence for the control plane.
//!
//! Queries are written by hand (no compile-time macros) so the project builds
//! without a live database, and every repository returns `wp-common` models so
//! the web layer never sees SQL types.

pub mod audit;
pub mod destinations;
pub mod jobs;
pub mod metrics;
pub mod notifications;
pub mod schedules;
pub mod seed;
pub mod servers;
pub mod sites;
pub mod teams;
pub mod tokens;
pub mod users;

use anyhow::Context;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub type Db = SqlitePool;

/// Opens the database with the pragmas that matter for a low-latency panel:
/// WAL for concurrent reads, `NORMAL` sync, and a busy timeout so writers queue
/// instead of erroring.
pub async fn connect(path: &Path) -> anyhow::Result<Db> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating data dir {}", parent.display()))?;
        }
    }

    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .context("opening SQLite database")?;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    Ok(pool)
}

/// Timestamps are stored as text. Accept both `datetime('now')` output and
/// RFC 3339 so hand-written SQL and Rust inserts interoperate.
pub fn parse_ts(raw: &str) -> DateTime<Utc> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return dt.with_timezone(&Utc);
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Utc.from_utc_datetime(&naive);
        }
    }
    Utc::now()
}

pub fn parse_ts_opt(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.as_deref().map(parse_ts)
}

pub fn now_string() -> String {
    Utc::now().to_rfc3339()
}
