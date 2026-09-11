//! Restic backups: encrypted, deduplicated, files plus database dump.

use crate::config::Config;
use crate::exec;
use crate::ops::database;
use crate::store::SiteRecord;
use wp_common::models::BackupScope;
use wp_common::{Error, Result};

fn repo(config: &Config) -> Result<&str> {
    config
        .restic_repo
        .as_deref()
        .ok_or_else(|| Error::Invalid("no restic repository configured on this agent".into()))
}

pub struct Snapshot {
    pub id: String,
    pub size_bytes: u64,
}

pub async fn create(config: &Config, site: &SiteRecord, scope: BackupScope) -> Result<Snapshot> {
    let repo = repo(config)?;
    let root = config.site_root(&site.domain);

    if scope != BackupScope::FilesOnly {
        database::dump(config, site).await?;
    }

    let target = match scope {
        BackupScope::DatabaseOnly => root.join("backups").display().to_string(),
        _ => root.display().to_string(),
    };

    let output = exec::run(
        config.dry_run,
        "restic",
        &[
            "--repo".to_string(),
            repo.to_string(),
            "backup".to_string(),
            "--tag".to_string(),
            format!("site={}", site.domain),
            "--tag".to_string(),
            format!("scope={}", scope.as_str()),
            "--json".to_string(),
            target,
        ],
    )
    .await?;

    // restic emits one JSON object per line; the summary is last.
    let (id, size) = output
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("message_type").and_then(|m| m.as_str()) == Some("summary"))
        .map(|summary| {
            (
                summary
                    .get("snapshot_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                summary
                    .get("total_bytes_processed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            )
        })
        .unwrap_or_else(|| ("dry-run".to_string(), 0));

    Ok(Snapshot {
        id,
        size_bytes: size,
    })
}

pub async fn restore(
    config: &Config,
    site: &SiteRecord,
    snapshot_id: &str,
    scope: BackupScope,
) -> Result<()> {
    let repo = repo(config)?;
    let root = config.site_root(&site.domain);

    exec::run(
        config.dry_run,
        "restic",
        &[
            "--repo".to_string(),
            repo.to_string(),
            "restore".to_string(),
            snapshot_id.to_string(),
            "--target".to_string(),
            "/".to_string(),
        ],
    )
    .await?;

    if scope != BackupScope::FilesOnly {
        let dump = root.join("backups/database.sql").display().to_string();
        database::import(config, site, &dump).await?;
    }

    exec::run(
        config.dry_run,
        "chown",
        &[
            "-R".to_string(),
            format!("{}:{}", site.uid, site.uid),
            root.display().to_string(),
        ],
    )
    .await
    .map(|_| ())
}

/// Applies the retention policy after a successful backup.
pub async fn prune(config: &Config, site: &SiteRecord, policy: wp_common::models::RetentionPolicy) -> Result<()> {
    let repo = repo(config)?;
    exec::run(
        config.dry_run,
        "restic",
        &[
            "--repo".to_string(),
            repo.to_string(),
            "forget".to_string(),
            "--prune".to_string(),
            "--tag".to_string(),
            format!("site={}", site.domain),
            "--keep-hourly".to_string(),
            policy.hourly.to_string(),
            "--keep-daily".to_string(),
            policy.daily.to_string(),
            "--keep-weekly".to_string(),
            policy.weekly.to_string(),
            "--keep-monthly".to_string(),
            policy.monthly.to_string(),
        ],
    )
    .await
    .map(|_| ())
}
