use sqlx::Row;
use sqlx::SqlitePool;

use wp_common::models::SiteMetricSample;

pub struct ServerMetricRow {
    pub id: i64,
    pub server_id: i64,
    pub cpu: f64,
    pub memory: f64,
    pub disk: f64,
    pub load_1m: f64,
    pub sites: i64,
    pub created_at: String,
}

pub async fn insert_server(
    db: &SqlitePool,
    server_id: i64,
    cpu: f64,
    memory: f64,
    disk: f64,
    load_1m: f64,
    sites: i64,
) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO server_metrics_history (server_id, cpu, memory, disk, load_1m, sites, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         RETURNING id",
    )
    .bind(server_id)
    .bind(cpu)
    .bind(memory)
    .bind(disk)
    .bind(load_1m)
    .bind(sites)
    .fetch_one(db)
    .await?;

    Ok(row.get::<i64, _>("id"))
}

pub async fn insert_site(db: &SqlitePool, sample: &SiteMetricSample) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO site_metrics_history (site_id, cpu, memory_mb, php_busy, cache_hit_ratio, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         RETURNING id",
    )
    .bind(sample.site_id)
    .bind(sample.cpu_percent as f64)
    .bind(sample.memory_mb as i64)
    .bind(sample.php_busy_workers as i32)
    .bind(sample.cache_hit_ratio)
    .fetch_one(db)
    .await?;

    Ok(row.get::<i64, _>("id"))
}

pub async fn server_history(
    db: &SqlitePool,
    server_id: i64,
    hours: u32,
) -> sqlx::Result<Vec<ServerMetricRow>> {
    let mut rows_raw = sqlx::query(
        "SELECT id, server_id, cpu, memory, disk, load_1m, sites, created_at
         FROM server_metrics_history
         WHERE server_id = ?1 AND created_at > datetime('now', ?2)
         ORDER BY created_at ASC",
    )
    .bind(server_id)
    .bind(format!("-{hours} hours"))
    .fetch_all(db)
    .await?;

    let mut rows = Vec::new();
    for row in rows_raw.drain(..) {
        rows.push(ServerMetricRow {
            id: row.get("id"),
            server_id: row.get("server_id"),
            cpu: row.get("cpu"),
            memory: row.get("memory"),
            disk: row.get("disk"),
            load_1m: row.get("load_1m"),
            sites: row.get("sites"),
            created_at: row.get("created_at"),
        });
    }

    Ok(rows)
}

pub struct SiteMetricRow {
    pub id: i64,
    pub site_id: i64,
    pub cpu: f64,
    pub memory_mb: i64,
    pub php_busy: i32,
    pub cache_hit_ratio: Option<f64>,
    pub created_at: String,
}

pub async fn site_history(
    db: &SqlitePool,
    site_id: i64,
    hours: u32,
) -> sqlx::Result<Vec<SiteMetricRow>> {
    let mut rows_raw = sqlx::query(
        "SELECT id, site_id, cpu, memory_mb, php_busy, cache_hit_ratio, created_at
         FROM site_metrics_history
         WHERE site_id = ?1 AND created_at > datetime('now', ?2)
         ORDER BY created_at ASC",
    )
    .bind(site_id)
    .bind(format!("-{hours} hours"))
    .fetch_all(db)
    .await?;

    let mut rows = Vec::new();
    for row in rows_raw.drain(..) {
        rows.push(SiteMetricRow {
            id: row.get("id"),
            site_id: row.get("site_id"),
            cpu: row.get("cpu"),
            memory_mb: row.get("memory_mb"),
            php_busy: row.get("php_busy"),
            cache_hit_ratio: row.get("cache_hit_ratio"),
            created_at: row.get("created_at"),
        });
    }

    Ok(rows)
}

/// Delete history older than `days` days.
pub async fn cleanup(db: &SqlitePool, days: u32) -> sqlx::Result<u64> {
    let r1 =
        sqlx::query("DELETE FROM server_metrics_history WHERE created_at < datetime('now', ?1)")
            .bind(format!("-{days} days"))
            .execute(db)
            .await?;
    let r2 = sqlx::query("DELETE FROM site_metrics_history WHERE created_at < datetime('now', ?1)")
        .bind(format!("-{days} days"))
        .execute(db)
        .await?;
    Ok((r1.rows_affected() + r2.rows_affected()) as u64)
}

/// Downsample: for rows older than 48h, keep only one per 15-minute bucket.
pub async fn downsample(db: &SqlitePool) -> sqlx::Result<u64> {
    // Delete server_metrics_history older than 48h except one per 15-min bucket.
    let r1 = sqlx::query(
        "DELETE FROM server_metrics_history
         WHERE id NOT IN (
             SELECT MIN(id)
             FROM server_metrics_history
             WHERE created_at < datetime('now', '-48 hours')
             GROUP BY server_id, strftime('%Y-%m-%d %H:', created_at) || CAST((CAST(strftime('%M', created_at) AS INT) / 15) * 15 AS TEXT)
         ) AND created_at < datetime('now', '-48 hours')",
    )
    .execute(db)
    .await?;

    let r2 = sqlx::query(
        "DELETE FROM site_metrics_history
         WHERE id NOT IN (
             SELECT MIN(id)
             FROM site_metrics_history
             WHERE created_at < datetime('now', '-48 hours')
             GROUP BY site_id, strftime('%Y-%m-%d %H:', created_at) || CAST((CAST(strftime('%M', created_at) AS INT) / 15) * 15 AS TEXT)
         ) AND created_at < datetime('now', '-48 hours')",
    )
    .execute(db)
    .await?;

    Ok((r1.rows_affected() + r2.rows_affected()) as u64)
}
