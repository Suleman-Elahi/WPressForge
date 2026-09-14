use sqlx::{Row, SqlitePool};

pub struct Notification {
    pub id: i64,
    pub severity: String,
    pub rule: String,
    pub target: String,
    pub message: String,
    pub resolved_at: Option<String>,
    pub created_at: String,
}

pub async fn list_open(db: &SqlitePool, limit: i64) -> sqlx::Result<Vec<Notification>> {
    let rows = sqlx::query(
        "SELECT id, severity, rule, target, message, resolved_at, created_at
         FROM notifications
         WHERE resolved_at IS NULL
         ORDER BY created_at DESC
         LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().map(|row| Notification {
        id: row.get("id"),
        severity: row.get("severity"),
        rule: row.get("rule"),
        target: row.get("target"),
        message: row.get("message"),
        resolved_at: row.get("resolved_at"),
        created_at: row.get("created_at"),
    }).collect())
}

pub async fn list_recent(db: &SqlitePool, limit: i64) -> sqlx::Result<Vec<Notification>> {
    let rows = sqlx::query(
        "SELECT id, severity, rule, target, message, resolved_at, created_at
         FROM notifications
         ORDER BY created_at DESC
         LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().map(|row| Notification {
        id: row.get("id"),
        severity: row.get("severity"),
        rule: row.get("rule"),
        target: row.get("target"),
        message: row.get("message"),
        resolved_at: row.get("resolved_at"),
        created_at: row.get("created_at"),
    }).collect())
}

pub async fn count_open(db: &SqlitePool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE resolved_at IS NULL")
        .fetch_one(db)
        .await
}

pub async fn resolve(db: &SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE notifications SET resolved_at = datetime('now') WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
