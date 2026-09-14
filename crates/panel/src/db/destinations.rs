//! Backup destination CRUD. Destinations store S3/R2/B2/Wasabi/MinIO credentials
//! encrypted at rest using SecretBox.

use crate::secrets::SecretBox;
use sqlx::{AssertSqlSafe, SqlitePool};
use wp_common::{Error, Result};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Destination {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub bucket: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_sealed: Option<String>,
    pub restic_password_sealed: Option<String>,
    pub repo_prefix: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct DestinationForm {
    pub name: String,
    pub provider: String,
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub access_key_id: String,
    pub secret: String,
    pub restic_password: String,
}

const COLUMNS: &str = "id, name, provider, bucket, region, endpoint, access_key_id, secret_sealed, restic_password_sealed, repo_prefix, created_at";

fn row_to_destination(row: &sqlx::sqlite::SqliteRow) -> Destination {
    use sqlx::Row;
    Destination {
        id: row.get("id"),
        name: row.get("name"),
        provider: row.get("provider"),
        bucket: row.get("bucket"),
        region: row.get("region"),
        endpoint: row.get("endpoint"),
        access_key_id: row.get("access_key_id"),
        secret_sealed: row.get("secret_sealed"),
        restic_password_sealed: row.get("restic_password_sealed"),
        repo_prefix: row.get("repo_prefix"),
        created_at: row.get("created_at"),
    }
}

pub async fn list(db: &SqlitePool) -> Result<Vec<Destination>> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_destinations ORDER BY name"
    )))
    .fetch_all(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(rows.iter().map(row_to_destination).collect())
}

pub async fn get(db: &SqlitePool, id: i64) -> Result<Option<Destination>> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM backup_destinations WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    Ok(row.map(|r| row_to_destination(&r)))
}

pub async fn create(
    db: &SqlitePool,
    form: DestinationForm,
    secrets: &SecretBox,
) -> Result<Destination> {
    let secret_sealed = if form.secret.is_empty() {
        None
    } else {
        Some(secrets.seal(&form.secret)?)
    };
    let restic_password_sealed = if form.restic_password.is_empty() {
        None
    } else {
        Some(secrets.seal(&form.restic_password)?)
    };

    let result = sqlx::query(
        "INSERT INTO backup_destinations (name, provider, bucket, region, endpoint, access_key_id, secret_sealed, restic_password_sealed, repo_prefix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'wp')",
    )
    .bind(&form.name)
    .bind(&form.provider)
    .bind(&form.bucket)
    .bind(if form.region.is_empty() { None } else { Some(&form.region) })
    .bind(if form.endpoint.is_empty() { None } else { Some(&form.endpoint) })
    .bind(if form.access_key_id.is_empty() { None } else { Some(&form.access_key_id) })
    .bind(&secret_sealed)
    .bind(&restic_password_sealed)
    .execute(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?;

    let id = result.last_insert_rowid();
    get(db, id)
        .await?
        .ok_or_else(|| Error::Invalid("failed to fetch created destination".into()))
}

pub async fn delete(db: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM backup_destinations WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| Error::Internal(format!("database error: {e}")))?;
    Ok(())
}

/// Decrypt destination credentials for passing to an agent operation.
pub fn decrypt_credentials(
    dest: &Destination,
    secrets: &SecretBox,
) -> Result<DestinationCredentials> {
    let secret = dest
        .secret_sealed
        .as_deref()
        .map(|s| secrets.open(s))
        .transpose()?;
    let restic_password = dest
        .restic_password_sealed
        .as_deref()
        .map(|s| secrets.open(s))
        .transpose()?
        .unwrap_or_default();

    Ok(DestinationCredentials {
        access_key_id: dest.access_key_id.clone().unwrap_or_default(),
        secret: secret.unwrap_or_default(),
        restic_password,
    })
}

pub struct DestinationCredentials {
    pub access_key_id: String,
    pub secret: String,
    pub restic_password: String,
}
