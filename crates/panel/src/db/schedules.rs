//! Backup schedule CRUD. Schedules determine when automatic backups run.

use sqlx::{AssertSqlSafe, Row, SqlitePool};
use wp_common::{Error, Result};

#[derive(Debug, Clone)]
pub struct Schedule {
    pub id: i64,
    pub site_id: i64,
    pub destination_id: Option<i64>,
    pub scope: String,
    pub interval_minutes: i64,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub enabled: bool,
    pub keep_hourly: i64,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
}

#[derive(Debug, Clone)]
pub struct ScheduleForm {
    pub site_id: i64,
    pub destination_id: Option<i64>,
    pub scope: String,
    pub interval_minutes: i64,
    pub enabled: bool,
    pub keep_hourly: i64,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
}

const COLUMNS: &str = "id, site_id, destination_id, scope, interval_minutes, last_run_at, next_run_at, enabled, keep_hourly, keep_daily, keep_weekly, keep_monthly";

fn row_to_schedule(row: &sqlx::sqlite::SqliteRow) -> Schedule {
    Schedule {
        id: row.get("id"),
        site_id: row.get("site_id"),
        destination_id: row.get("destination_id"),
        scope: row.get("scope"),
        interval_minutes: row.get("interval_minutes"),
        last_run_at: row.get("last_run_at"),
        next_run_at: row.get("next_run_at"),
        enabled: row.get::<i64, _>("enabled") != 0,
        keep_hourly: row.get("keep_hourly"),
        keep_daily: row.get("keep_daily"),
        keep_weekly: row.get("keep_weekly"),
        keep_monthly: row.get("keep_monthly"),
    }
}

/// Fetch schedules that are due (enabled and next_run_at <= now).
pub async fn due(db: &SqlitePool, now: &str) -> Result<Vec<Schedule>> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_schedules WHERE enabled = 1 AND (next_run_at IS NULL OR next_run_at <= ?1)"
    )))
    .bind(now)
    .fetch_all(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(rows.iter().map(row_to_schedule).collect())
}

/// Get a schedule by ID.
pub async fn get(db: &SqlitePool, id: i64) -> Result<Option<Schedule>> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_schedules WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(row.as_ref().map(row_to_schedule))
}

/// Get the schedule for a site (there should be at most one).
pub async fn for_site(db: &SqlitePool, site_id: i64) -> Result<Option<Schedule>> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_schedules WHERE site_id = ?1 LIMIT 1"
    )))
    .bind(site_id)
    .fetch_optional(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(row.as_ref().map(row_to_schedule))
}

/// Create or update a schedule for a site.
pub async fn upsert(db: &SqlitePool, form: ScheduleForm) -> Result<Schedule> {
    // Check if a schedule already exists for this site.
    let existing = for_site(db, form.site_id).await?;

    if let Some(schedule) = existing {
        sqlx::query(
            "UPDATE backup_schedules SET destination_id = ?1, scope = ?2, interval_minutes = ?3, enabled = ?4, keep_hourly = ?5, keep_daily = ?6, keep_weekly = ?7, keep_monthly = ?8 WHERE id = ?9",
        )
        .bind(form.destination_id)
        .bind(&form.scope)
        .bind(form.interval_minutes)
        .bind(form.enabled as i64)
        .bind(form.keep_hourly)
        .bind(form.keep_daily)
        .bind(form.keep_weekly)
        .bind(form.keep_monthly)
        .bind(schedule.id)
        .execute(db)
        .await
        .map_err(|e| Error::Internal(format!("database error: {e}")))?;

        get(db, schedule.id)
            .await?
            .ok_or_else(|| Error::Internal("failed to fetch updated schedule".into()))
    } else {
        let result = sqlx::query(
            "INSERT INTO backup_schedules (site_id, destination_id, scope, interval_minutes, enabled, keep_hourly, keep_daily, keep_weekly, keep_monthly) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(form.site_id)
        .bind(form.destination_id)
        .bind(&form.scope)
        .bind(form.interval_minutes)
        .bind(form.enabled as i64)
        .bind(form.keep_hourly)
        .bind(form.keep_daily)
        .bind(form.keep_weekly)
        .bind(form.keep_monthly)
        .execute(db)
        .await
        .map_err(|e| Error::Internal(format!("database error: {e}")))?;

        let id = result.last_insert_rowid();
        get(db, id)
            .await?
            .ok_or_else(|| Error::Internal("failed to fetch created schedule".into()))
    }
}

/// Mark a schedule as run and set next_run_at.
pub async fn mark_scheduled(db: &SqlitePool, id: i64, now: &str, next_run: &str) -> Result<()> {
    sqlx::query("UPDATE backup_schedules SET last_run_at = ?1, next_run_at = ?2 WHERE id = ?3")
        .bind(now)
        .bind(next_run)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(())
}

/// Delete a schedule.
pub async fn delete(db: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM backup_schedules WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(())
}

/// List all enabled schedules (for alert evaluation).
pub async fn list_active(db: &SqlitePool) -> Result<Vec<Schedule>> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_schedules WHERE enabled = 1"
    )))
    .fetch_all(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(rows.iter().map(row_to_schedule).collect())
}
