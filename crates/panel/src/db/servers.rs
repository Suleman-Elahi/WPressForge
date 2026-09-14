use super::{Db, parse_ts, parse_ts_opt};
use sqlx::{AssertSqlSafe, Row};
use wp_common::models::{Server, ServerMetrics, ServerStatus};

/// A server row plus the aggregates the list view needs, so the template does
/// not trigger extra queries per row.
#[derive(Debug, Clone)]
pub struct ServerRow {
    pub server: Server,
    pub metrics: ServerMetrics,
    pub site_count: i64,
    pub agent_token: String,
    pub agent_fingerprint: Option<String>,
}

impl ServerRow {
    pub fn connection(&self) -> crate::agent::ServerConnection {
        crate::agent::ServerConnection {
            url: self.server.agent_url.clone(),
            token: self.agent_token.clone(),
            fingerprint: self.agent_fingerprint.clone(),
        }
    }
}

fn status(raw: &str) -> ServerStatus {
    match raw {
        "online" => ServerStatus::Online,
        "degraded" => ServerStatus::Degraded,
        "offline" => ServerStatus::Offline,
        _ => ServerStatus::Provisioning,
    }
}

fn map(row: &sqlx::sqlite::SqliteRow) -> ServerRow {
    let metrics = row
        .get::<Option<String>, _>("metrics_json")
        .and_then(|json| serde_json::from_str::<ServerMetrics>(&json).ok())
        .unwrap_or_default();

    ServerRow {
        server: Server {
            id: row.get("id"),
            name: row.get("name"),
            agent_url: row.get("agent_url"),
            hostname: row.get("hostname"),
            ip_address: row.get("ip_address"),
            provider: row.get("provider"),
            region: row.get("region"),
            status: status(row.get::<String, _>("status").as_str()),
            agent_version: row.get("agent_version"),
            last_seen_at: parse_ts_opt(row.get("last_seen_at")),
            created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
        },
        metrics,
        site_count: row.try_get("site_count").unwrap_or(0),
        agent_token: row.try_get("agent_token").unwrap_or_default(),
        agent_fingerprint: row.try_get("agent_fingerprint").ok().flatten(),
    }
}

const SELECT: &str = "SELECT s.*, (SELECT COUNT(*) FROM sites WHERE sites.server_id = s.id) AS site_count FROM servers s";

pub async fn list(db: &Db) -> sqlx::Result<Vec<ServerRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!("{SELECT} ORDER BY s.name")))
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn get(db: &Db, id: i64) -> sqlx::Result<Option<ServerRow>> {
    let row = sqlx::query(AssertSqlSafe(format!("{SELECT} WHERE s.id = ?1")))
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.as_ref().map(map))
}

pub async fn count(db: &Db) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM servers")
        .fetch_one(db)
        .await
}

pub struct NewServer<'a> {
    pub name: &'a str,
    pub agent_url: &'a str,
    pub agent_token: &'a str,
    pub hostname: &'a str,
    pub ip_address: &'a str,
    pub provider: Option<&'a str>,
    pub region: Option<&'a str>,
    pub agent_fingerprint: Option<&'a str>,
}

pub async fn create(db: &Db, new: NewServer<'_>) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO servers (name, agent_url, agent_token, hostname, ip_address, provider, region, agent_fingerprint, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'provisioning', ?9) RETURNING id",
    )
    .bind(new.name)
    .bind(new.agent_url)
    .bind(new.agent_token)
    .bind(new.hostname)
    .bind(new.ip_address)
    .bind(new.provider)
    .bind(new.region)
    .bind(new.agent_fingerprint)
    .bind(super::now_string())
    .fetch_one(db)
    .await?;
    Ok(row.get("id"))
}

pub async fn delete(db: &Db, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM servers WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Stores the result of a heartbeat: status, agent version and metrics blob.
pub async fn record_heartbeat(
    db: &Db,
    id: i64,
    status: ServerStatus,
    agent_version: Option<&str>,
    metrics: Option<&ServerMetrics>,
) -> sqlx::Result<()> {
    let metrics_json = metrics.and_then(|m| serde_json::to_string(m).ok());
    sqlx::query(
        "UPDATE servers
         SET status = ?2, agent_version = COALESCE(?3, agent_version),
             metrics_json = COALESCE(?4, metrics_json), last_seen_at = ?5
         WHERE id = ?1",
    )
    .bind(id)
    .bind(status.as_str())
    .bind(agent_version)
    .bind(metrics_json)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}
