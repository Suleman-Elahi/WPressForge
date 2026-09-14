use super::{Db, parse_ts, parse_ts_opt};
use sqlx::{AssertSqlSafe, Row};
use wp_common::models::{
    Backup, BackupScope, CacheSettings, DatabaseMode, Domain, Environment, PhpVersion,
    ResourceLimits, Site, SiteStatus, SslIssuer, SslState,
};

/// Site plus the parent server name, which every site view displays.
#[derive(Debug, Clone)]
pub struct SiteRow {
    pub site: Site,
    pub server_name: String,
}

fn php(raw: &str) -> PhpVersion {
    PhpVersion::parse(raw).unwrap_or(PhpVersion::Php84)
}

fn status(raw: &str) -> SiteStatus {
    match raw {
        "online" => SiteStatus::Online,
        "stopped" => SiteStatus::Stopped,
        "failed" => SiteStatus::Failed,
        "suspended" => SiteStatus::Suspended,
        _ => SiteStatus::Provisioning,
    }
}

fn map(row: &sqlx::sqlite::SqliteRow) -> SiteRow {
    let site = Site {
        id: row.get("id"),
        server_id: row.get("server_id"),
        domain: row.get("domain"),
        title: row.get("title"),
        status: status(row.get::<String, _>("status").as_str()),
        php_version: php(row.get::<String, _>("php_version").as_str()),
        wp_version: row.get("wp_version"),
        database_mode: match row.get::<String, _>("database_mode").as_str() {
            "dedicated" => DatabaseMode::Dedicated,
            _ => DatabaseMode::Shared,
        },
        limits: ResourceLimits {
            cpu_cores: row.get::<f64, _>("cpu_cores") as f32,
            memory_mb: row.get::<i64, _>("memory_mb") as u32,
            php_workers: row.get::<i64, _>("php_workers") as u16,
        },
        cache: CacheSettings {
            fastcgi_cache: row.get::<i64, _>("cache_fastcgi") != 0,
            ttl_seconds: row.get::<i64, _>("cache_ttl_seconds") as u32,
            redis_object_cache: row.get::<i64, _>("cache_redis") != 0,
            opcache: row.get::<i64, _>("cache_opcache") != 0,
            brotli: row.get::<i64, _>("cache_brotli") != 0,
        },
        ssl: SslState {
            enabled: row.get::<i64, _>("ssl_enabled") != 0,
            issuer: match row.get::<String, _>("ssl_issuer").as_str() {
                "custom" => SslIssuer::Custom,
                "none" => SslIssuer::None,
                _ => SslIssuer::LetsEncrypt,
            },
            expires_at: parse_ts_opt(row.get("ssl_expires_at")),
            auto_renew: row.get::<i64, _>("ssl_auto_renew") != 0,
        },
        environment: match row.get::<String, _>("environment").as_str() {
            "staging" => Environment::Staging,
            _ => Environment::Production,
        },
        parent_site_id: row.get("parent_site_id"),
        uid: row.get::<i64, _>("uid") as u32,
        disk_usage_mb: row.get::<i64, _>("disk_usage_mb") as u64,
        created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
    };

    SiteRow {
        site,
        server_name: row.try_get("server_name").unwrap_or_default(),
    }
}

const SELECT: &str =
    "SELECT s.*, srv.name AS server_name FROM sites s JOIN servers srv ON srv.id = s.server_id";

pub async fn list(db: &Db) -> sqlx::Result<Vec<SiteRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!("{SELECT} ORDER BY s.domain")))
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn list_for_server(db: &Db, server_id: i64) -> sqlx::Result<Vec<SiteRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "{SELECT} WHERE s.server_id = ?1 ORDER BY s.domain"
    )))
    .bind(server_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn get(db: &Db, id: i64) -> sqlx::Result<Option<SiteRow>> {
    let row = sqlx::query(AssertSqlSafe(format!("{SELECT} WHERE s.id = ?1")))
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.as_ref().map(map))
}

pub async fn list_for_user(db: &Db, user_id: i64, user_role: &str) -> sqlx::Result<Vec<SiteRow>> {
    if super::teams::has_global_access(user_role) {
        return list(db).await;
    }
    let rows = sqlx::query(AssertSqlSafe(format!(
        "{SELECT} JOIN site_users su ON su.site_id = s.id WHERE su.user_id = ?1 ORDER BY s.domain"
    )))
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(map).collect())
}

pub async fn get_for_user(
    db: &Db,
    id: i64,
    user_id: i64,
    user_role: &str,
) -> sqlx::Result<Option<SiteRow>> {
    if super::teams::has_global_access(user_role) {
        return get(db, id).await;
    }
    let row = sqlx::query(AssertSqlSafe(format!(
        "{SELECT} JOIN site_users su ON su.site_id = s.id WHERE s.id = ?1 AND su.user_id = ?2"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(map))
}

pub async fn count(db: &Db) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM sites")
        .fetch_one(db)
        .await
}

pub async fn count_by_status(db: &Db, status: SiteStatus) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM sites WHERE status = ?1")
        .bind(status.as_str())
        .fetch_one(db)
        .await
}

pub async fn domain_exists(db: &Db, domain: &str) -> sqlx::Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sites WHERE domain = ?1")
        .bind(domain)
        .fetch_one(db)
        .await?;
    Ok(count > 0)
}

/// Next free UID in the per-site range described in the architecture plan.
pub async fn next_uid(db: &Db) -> sqlx::Result<u32> {
    let max: Option<i64> = sqlx::query_scalar("SELECT MAX(uid) FROM sites")
        .fetch_one(db)
        .await?;
    Ok(max.map(|m| m as u32 + 1).unwrap_or(10001))
}

pub struct NewSite<'a> {
    pub server_id: i64,
    pub domain: &'a str,
    pub title: Option<&'a str>,
    pub php_version: PhpVersion,
    pub database_mode: DatabaseMode,
    pub environment: Environment,
    pub parent_site_id: Option<i64>,
    pub limits: ResourceLimits,
    pub cache: CacheSettings,
    pub request_ssl: bool,
}

pub async fn create(db: &Db, new: NewSite<'_>) -> sqlx::Result<i64> {
    let uid = next_uid(db).await?;
    let row = sqlx::query(
        "INSERT INTO sites (
            server_id, domain, title, status, php_version, database_mode, environment,
            parent_site_id, uid, cpu_cores, memory_mb, php_workers,
            cache_fastcgi, cache_ttl_seconds, cache_redis, cache_opcache, cache_brotli,
            ssl_enabled, ssl_issuer, created_at
         ) VALUES (
            ?1, ?2, ?3, 'provisioning', ?4, ?5, ?6,
            ?7, ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16,
            0, ?17, ?18
         ) RETURNING id",
    )
    .bind(new.server_id)
    .bind(new.domain)
    .bind(new.title)
    .bind(new.php_version.as_str())
    .bind(new.database_mode.as_str())
    .bind(new.environment.as_str())
    .bind(new.parent_site_id)
    .bind(uid as i64)
    .bind(new.limits.cpu_cores as f64)
    .bind(new.limits.memory_mb as i64)
    .bind(new.limits.php_workers as i64)
    .bind(new.cache.fastcgi_cache as i64)
    .bind(new.cache.ttl_seconds as i64)
    .bind(new.cache.redis_object_cache as i64)
    .bind(new.cache.opcache as i64)
    .bind(new.cache.brotli as i64)
    .bind(if new.request_ssl {
        SslIssuer::LetsEncrypt.as_str()
    } else {
        SslIssuer::None.as_str()
    })
    .bind(super::now_string())
    .fetch_one(db)
    .await?;

    let site_id: i64 = row.get("id");
    add_domain(db, site_id, new.domain, true).await?;
    Ok(site_id)
}

pub async fn set_status(db: &Db, id: i64, status: SiteStatus) -> sqlx::Result<()> {
    sqlx::query("UPDATE sites SET status = ?2 WHERE id = ?1")
        .bind(id)
        .bind(status.as_str())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_php(db: &Db, id: i64, version: PhpVersion) -> sqlx::Result<()> {
    sqlx::query("UPDATE sites SET php_version = ?2 WHERE id = ?1")
        .bind(id)
        .bind(version.as_str())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_cache(db: &Db, id: i64, cache: CacheSettings) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sites SET cache_fastcgi = ?2, cache_ttl_seconds = ?3, cache_redis = ?4,
                          cache_opcache = ?5, cache_brotli = ?6
         WHERE id = ?1",
    )
    .bind(id)
    .bind(cache.fastcgi_cache as i64)
    .bind(cache.ttl_seconds as i64)
    .bind(cache.redis_object_cache as i64)
    .bind(cache.opcache as i64)
    .bind(cache.brotli as i64)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_limits(db: &Db, id: i64, limits: ResourceLimits) -> sqlx::Result<()> {
    sqlx::query("UPDATE sites SET cpu_cores = ?2, memory_mb = ?3, php_workers = ?4 WHERE id = ?1")
        .bind(id)
        .bind(limits.cpu_cores as f64)
        .bind(limits.memory_mb as i64)
        .bind(limits.php_workers as i64)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_ssl(
    db: &Db,
    id: i64,
    enabled: bool,
    issuer: SslIssuer,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE sites SET ssl_enabled = ?2, ssl_issuer = ?3, ssl_expires_at = ?4 WHERE id = ?1",
    )
    .bind(id)
    .bind(enabled as i64)
    .bind(issuer.as_str())
    .bind(expires_at.map(|d| d.to_rfc3339()))
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_wp_version(db: &Db, id: i64, version: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE sites SET wp_version = ?2 WHERE id = ?1")
        .bind(id)
        .bind(version)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sites WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

pub async fn add_domain(db: &Db, site_id: i64, name: &str, primary: bool) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO domains (site_id, name, is_primary, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO NOTHING",
    )
    .bind(site_id)
    .bind(name)
    .bind(primary as i64)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn domains(db: &Db, site_id: i64) -> sqlx::Result<Vec<Domain>> {
    let rows = sqlx::query(
        "SELECT id, site_id, name, is_primary, redirect_to_primary, dns_ok
         FROM domains WHERE site_id = ?1 ORDER BY is_primary DESC, name",
    )
    .bind(site_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| Domain {
            id: row.get("id"),
            site_id: row.get("site_id"),
            name: row.get("name"),
            primary: row.get::<i64, _>("is_primary") != 0,
            redirect_to_primary: row.get::<i64, _>("redirect_to_primary") != 0,
            dns_ok: row.get::<i64, _>("dns_ok") != 0,
        })
        .collect())
}

pub async fn remove_domain(db: &Db, site_id: i64, name: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM domains WHERE site_id = ?1 AND name = ?2 AND is_primary = 0")
        .bind(site_id)
        .bind(name)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

pub async fn backups(db: &Db, site_id: i64, limit: i64) -> sqlx::Result<Vec<Backup>> {
    let rows = sqlx::query(
        "SELECT b.id, b.site_id, b.snapshot_id, b.scope, b.size_bytes, b.created_at,
                COALESCE(d.name, 'local') AS destination
         FROM backups b
         LEFT JOIN backup_destinations d ON d.id = b.destination_id
         WHERE b.site_id = ?1 ORDER BY b.created_at DESC LIMIT ?2",
    )
    .bind(site_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| Backup {
            id: row.get("id"),
            site_id: row.get("site_id"),
            snapshot_id: row.get("snapshot_id"),
            size_bytes: row.get::<i64, _>("size_bytes") as u64,
            scope: match row.get::<String, _>("scope").as_str() {
                "files" => BackupScope::FilesOnly,
                "database" => BackupScope::DatabaseOnly,
                _ => BackupScope::Full,
            },
            destination: row.get("destination"),
            created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
        })
        .collect())
}

pub async fn record_backup(
    db: &Db,
    site_id: i64,
    snapshot_id: &str,
    scope: BackupScope,
    size_bytes: u64,
) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO backups (site_id, snapshot_id, scope, size_bytes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
    )
    .bind(site_id)
    .bind(snapshot_id)
    .bind(scope.as_str())
    .bind(size_bytes as i64)
    .bind(super::now_string())
    .fetch_one(db)
    .await?;
    Ok(row.get("id"))
}

/// List all sites (for alert evaluation).
pub async fn list_all(db: &Db) -> sqlx::Result<Vec<SiteRow>> {
    let rows = sqlx::query(AssertSqlSafe(format!("{SELECT} ORDER BY s.domain")))
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(map).collect())
}

/// Get the most recent backup timestamp for a site.
pub async fn last_backup_time(db: &Db, site_id: i64) -> sqlx::Result<Option<String>> {
    let row = sqlx::query(
        "SELECT created_at FROM backups WHERE site_id = ?1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(site_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| r.get("created_at")))
}

/// Get domain by site ID.
pub async fn domain_by_id(db: &Db, site_id: i64) -> sqlx::Result<String> {
    let row = sqlx::query("SELECT domain FROM sites WHERE id = ?1")
        .bind(site_id)
        .fetch_one(db)
        .await?;
    Ok(row.get("domain"))
}
