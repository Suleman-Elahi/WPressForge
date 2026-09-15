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

/// Builds the [`ResticTarget`] the agent needs for one destination.
///
/// Single source of truth: repository URL, password and S3 credentials were
/// previously assembled inline at four call sites, which is how `repo_prefix`
/// ended up unused and how "sync from node" ended up sending an empty target.
///
/// Snapshots of every site live in one repository, separated by restic tags
/// (`site=<domain>`), so deduplication works across sites. `repo_prefix` keeps
/// the panel's data in its own path inside the bucket.
pub fn restic_target(
    dest: &Destination,
    secrets: &SecretBox,
) -> Result<wp_common::protocol::ResticTarget> {
    let creds = decrypt_credentials(dest, secrets)?;

    if creds.restic_password.is_empty() {
        return Err(Error::Invalid(format!(
            "destination `{}` has no repository password",
            dest.name
        )));
    }

    let endpoint = dest
        .endpoint
        .as_deref()
        .filter(|e| !e.is_empty())
        // Restic's S3 backend takes host[/path], not a scheme.
        .map(|e| {
            e.trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string()
        })
        .unwrap_or_else(|| match dest.region.as_deref() {
            Some(region) if !region.is_empty() => format!("s3.{region}.amazonaws.com"),
            _ => "s3.amazonaws.com".to_string(),
        });

    let prefix = dest.repo_prefix.trim_matches('/');
    let repo = if prefix.is_empty() {
        format!("s3:{}/{}", endpoint, dest.bucket)
    } else {
        format!("s3:{}/{}/{}", endpoint, dest.bucket, prefix)
    };

    let mut env = vec![("AWS_ACCESS_KEY_ID".to_string(), creds.access_key_id.clone())];
    env.push(("AWS_SECRET_ACCESS_KEY".to_string(), creds.secret.clone()));
    if let Some(region) = dest.region.as_deref().filter(|r| !r.is_empty()) {
        env.push(("AWS_DEFAULT_REGION".to_string(), region.to_string()));
    }

    Ok(wp_common::protocol::ResticTarget {
        repo,
        password: creds.restic_password,
        env,
    })
}

/// Resolves the destination a site backs up to: its schedule's destination, or
/// the only destination configured, otherwise `None`.
pub async fn for_site(
    db: &SqlitePool,
    secrets: &SecretBox,
    site_id: i64,
) -> Result<Option<(Destination, wp_common::protocol::ResticTarget)>> {
    let scheduled: Option<i64> = sqlx::query_scalar(
        "SELECT destination_id FROM backup_schedules WHERE site_id = ?1 AND destination_id IS NOT NULL",
    )
    .bind(site_id)
    .fetch_optional(db)
    .await
    .map_err(|e| Error::Internal(format!("database error: {e}")))?
    .flatten();

    let dest = match scheduled {
        Some(id) => get(db, id).await?,
        None => {
            let all = list(db).await?;
            if all.len() == 1 {
                all.into_iter().next()
            } else {
                None
            }
        }
    };

    match dest {
        Some(dest) => {
            let target = restic_target(&dest, secrets)?;
            Ok(Some((dest, target)))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(endpoint: Option<&str>, region: Option<&str>, prefix: &str) -> Destination {
        let secrets = SecretBox::from_key([3u8; 32]);
        Destination {
            id: 1,
            name: "test".into(),
            provider: "minio".into(),
            bucket: "wp-backups".into(),
            region: region.map(str::to_owned),
            endpoint: endpoint.map(str::to_owned),
            access_key_id: Some("AKIA".into()),
            secret_sealed: Some(secrets.seal("s3cret").expect("seal")),
            restic_password_sealed: Some(secrets.seal("repo-pass").expect("seal")),
            repo_prefix: prefix.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn builds_repo_url_from_endpoint_and_prefix() {
        let secrets = SecretBox::from_key([3u8; 32]);
        let target = restic_target(
            &destination(Some("http://127.0.0.1:9000"), None, "wp"),
            &secrets,
        )
        .expect("target");

        assert_eq!(target.repo, "s3:127.0.0.1:9000/wp-backups/wp");
        assert_eq!(target.password, "repo-pass");
        assert!(
            target
                .env
                .iter()
                .any(|(k, v)| k == "AWS_ACCESS_KEY_ID" && v == "AKIA")
        );
        assert!(
            target
                .env
                .iter()
                .any(|(k, v)| k == "AWS_SECRET_ACCESS_KEY" && v == "s3cret")
        );
    }

    #[test]
    fn falls_back_to_regional_aws_endpoint() {
        let secrets = SecretBox::from_key([3u8; 32]);
        let target =
            restic_target(&destination(None, Some("eu-central-1"), ""), &secrets).expect("target");

        assert_eq!(target.repo, "s3:s3.eu-central-1.amazonaws.com/wp-backups");
        assert!(
            target
                .env
                .iter()
                .any(|(k, v)| k == "AWS_DEFAULT_REGION" && v == "eu-central-1")
        );
    }

    #[test]
    fn refuses_a_destination_without_a_repository_password() {
        let secrets = SecretBox::from_key([3u8; 32]);
        let mut dest = destination(None, None, "wp");
        dest.restic_password_sealed = None;

        assert!(restic_target(&dest, &secrets).is_err());
    }
}
