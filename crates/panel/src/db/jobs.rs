use super::{parse_ts, parse_ts_opt, Db};
use sqlx::{AssertSqlSafe, Row};
use wp_common::models::{Job, JobKind, JobStatus, StepView};

/// Job plus display context (site domain / server name) for the jobs table.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub job: Job,
    pub site_domain: Option<String>,
    pub server_name: Option<String>,
    pub actor: String,
}

impl JobRow {
    /// "3m 12s" style duration, or elapsed time for running jobs.
    pub fn duration(&self) -> String {
        let end = self.job.finished_at.unwrap_or_else(chrono::Utc::now);
        let start = self.job.started_at.unwrap_or(self.job.created_at);
        let secs = (end - start).num_seconds().max(0) as u64;
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        }
    }
}

fn status(raw: &str) -> JobStatus {
    match raw {
        "running" => JobStatus::Running,
        "succeeded" => JobStatus::Succeeded,
        "failed" => JobStatus::Failed,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Queued,
    }
}

fn map(row: &sqlx::sqlite::SqliteRow) -> JobRow {
    JobRow {
        job: Job {
            id: row.get("id"),
            server_id: row.get("server_id"),
            site_id: row.get("site_id"),
            kind: JobKind::parse(row.get::<String, _>("kind").as_str())
                .unwrap_or(JobKind::SiteCreate),
            status: status(row.get::<String, _>("status").as_str()),
            progress: row.get::<i64, _>("progress").clamp(0, 100) as u8,
            message: row.get("message"),
            error: row.get("error"),
            created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
            started_at: parse_ts_opt(row.get("started_at")),
            finished_at: parse_ts_opt(row.get("finished_at")),
        },
        site_domain: row.try_get("site_domain").ok().flatten(),
        server_name: row.try_get("server_name").ok().flatten(),
        actor: row.try_get("actor").unwrap_or_else(|_| "system".into()),
    }
}

const SELECT: &str = "SELECT j.*, s.domain AS site_domain, srv.name AS server_name
     FROM jobs j
     LEFT JOIN sites s ON s.id = j.site_id
     LEFT JOIN servers srv ON srv.id = j.server_id";

pub async fn enqueue(
    db: &Db,
    kind: JobKind,
    server_id: Option<i64>,
    site_id: Option<i64>,
    payload: Option<serde_json::Value>,
    actor: &str,
) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO jobs (server_id, site_id, kind, status, progress, message, payload, actor, created_at)
         VALUES (?1, ?2, ?3, 'queued', 0, 'Queued', ?4, ?5, ?6) RETURNING id",
    )
    .bind(server_id)
    .bind(site_id)
    .bind(kind.as_str())
    .bind(payload.map(|p| p.to_string()))
    .bind(actor)
    .bind(super::now_string())
    .fetch_one(db)
    .await?;
    Ok(row.get("id"))
}

/// Atomically claims the oldest queued job. Returns `None` when idle.
pub async fn claim_next(db: &Db) -> sqlx::Result<Option<JobRow>> {
    let row = sqlx::query(
        "UPDATE jobs SET status = 'running', started_at = ?1, message = 'Starting'
         WHERE id = (SELECT id FROM jobs WHERE status = 'queued' ORDER BY id LIMIT 1)
         RETURNING id",
    )
    .bind(super::now_string())
    .fetch_optional(db)
    .await?;

    match row {
        Some(row) => get(db, row.get("id")).await,
        None => Ok(None),
    }
}

pub async fn get(db: &Db, id: i64) -> sqlx::Result<Option<JobRow>> {
    let row = sqlx::query(AssertSqlSafe(format!("{SELECT} WHERE j.id = ?1")))
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.as_ref().map(map))
}

pub async fn payload(db: &Db, id: i64) -> sqlx::Result<Option<serde_json::Value>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT payload FROM jobs WHERE id = ?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .flatten();
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
}

pub async fn list(db: &Db, limit: i64) -> sqlx::Result<Vec<JobRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!("{SELECT} ORDER BY j.id DESC LIMIT ?1")))
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn list_for_site(db: &Db, site_id: i64, limit: i64) -> sqlx::Result<Vec<JobRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!("{SELECT} WHERE j.site_id = ?1 ORDER BY j.id DESC LIMIT ?2")))
        .bind(site_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn list_active(db: &Db, limit: i64) -> sqlx::Result<Vec<JobRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "{SELECT} WHERE j.status IN ('queued','running') ORDER BY j.id DESC LIMIT ?1"
    )))
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn count_active(db: &Db) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status IN ('queued','running')")
        .fetch_one(db)
        .await
}

/// Check if there's an active (queued or running) job of the given kind for a site.
pub async fn has_active(db: &Db, site_id: i64, kind: JobKind) -> sqlx::Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE site_id = ?1 AND kind = ?2 AND status IN ('queued','running')",
    )
    .bind(site_id)
    .bind(kind.as_str())
    .fetch_one(db)
    .await?;
    Ok(count > 0)
}

pub async fn progress(db: &Db, id: i64, progress: u8, message: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET progress = ?2, message = ?3 WHERE id = ?1")
        .bind(id)
        .bind(progress as i64)
        .bind(message)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn finish(db: &Db, id: i64, status: JobStatus, message: &str, error: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs SET status = ?2, progress = ?3, message = ?4, error = ?5, finished_at = ?6
         WHERE id = ?1",
    )
    .bind(id)
    .bind(status.as_str())
    .bind(if status == JobStatus::Succeeded { 100i64 } else { 0i64 })
    .bind(message)
    .bind(error)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}

/// Marks jobs left `running` by a crashed process as failed at boot.
pub async fn requeue_orphans(db: &Db) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE jobs SET status = 'failed', message = 'Interrupted',
                         error = 'panel restarted while the job was running', finished_at = ?1
         WHERE status = 'running'",
    )
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

pub async fn add_step(
    db: &Db,
    job_id: i64,
    name: &str,
    ok: bool,
    duration_ms: u64,
    detail: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO job_steps (job_id, name, ok, duration_ms, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(job_id)
    .bind(name)
    .bind(ok as i64)
    .bind(duration_ms as i64)
    .bind(detail)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn steps(db: &Db, job_id: i64) -> sqlx::Result<Vec<StepView>> {
    let rows = sqlx::query(
        "SELECT name, ok, duration_ms, detail, created_at FROM job_steps
         WHERE job_id = ?1 ORDER BY id",
    )
    .bind(job_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| StepView {
            name: row.get("name"),
            ok: row.get::<i64, _>("ok") != 0,
            duration_ms: row.get::<i64, _>("duration_ms") as u64,
            detail: row.get("detail"),
            created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
        })
        .collect())
}
