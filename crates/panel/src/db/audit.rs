use super::{parse_ts, Db};
use sqlx::Row;
use wp_common::models::AuditEntry;

pub async fn record(
    db: &Db,
    actor: &str,
    action: &str,
    target: &str,
    detail: Option<&str>,
    success: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO audit_logs (actor, action, target, detail, success, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(success as i64)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list(db: &Db, limit: i64) -> sqlx::Result<Vec<AuditEntry>> {
    let rows = sqlx::query(
        "SELECT id, actor, action, target, detail, success, created_at
         FROM audit_logs ORDER BY id DESC LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| AuditEntry {
            id: row.get("id"),
            actor: row.get("actor"),
            action: row.get("action"),
            target: row.get("target"),
            detail: row.get("detail"),
            success: row.get::<i64, _>("success") != 0,
            created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
        })
        .collect())
}
