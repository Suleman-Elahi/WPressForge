//! Restic backups: encrypted, deduplicated, files plus database dump.
//!
//! All operations now accept a `ResticTarget` so credentials are passed per-call
//! and never stored by the agent.

use crate::config::Config;
use crate::exec;
use crate::ops::database;
use crate::store::SiteRecord;
use wp_common::Result;
use wp_common::models::BackupScope;
use wp_common::protocol::ResticTarget;

#[derive(serde::Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub size_bytes: u64,
    pub files_bytes: u64,
    pub db_bytes: u64,
}

/// Build restic args with repo and password.
fn restic_args(target: &ResticTarget, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--repo".to_string(),
        target.repo.clone(),
        "--password".to_string(),
        target.password.clone(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

/// Build restic args with env vars logged (keys only).
fn restic_env(target: &ResticTarget) -> Vec<(String, String)> {
    target.env.clone()
}

/// Initialize a restic repository (idempotent).
pub async fn init(config: &Config, target: &ResticTarget) -> Result<()> {
    let args = restic_args(target, &["init"]);
    let env = restic_env(target);

    match exec::run_with_env(config.dry_run, "restic", &args, &env).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("repository master key already initialized") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Create a backup.
pub async fn create(
    config: &Config,
    site: &SiteRecord,
    scope: BackupScope,
    target: &ResticTarget,
) -> Result<Snapshot> {
    let root = config.site_root(&site.domain);

    if scope != BackupScope::FilesOnly {
        database::dump(config, site).await?;
    }

    let backup_target = match scope {
        BackupScope::DatabaseOnly => root.join("backups").display().to_string(),
        _ => root.display().to_string(),
    };

    let extra = &[
        "backup",
        "--tag",
        &format!("site={}", site.domain),
        "--tag",
        &format!("scope={}", scope.as_str()),
        "--json",
        &backup_target,
    ];
    let args = restic_args(target, extra);
    let env = restic_env(target);

    let output = exec::run_with_env(config.dry_run, "restic", &args, &env).await?;

    // restic emits one JSON object per line; the summary is last.
    let summary = output
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("message_type").and_then(|m| m.as_str()) == Some("summary"));

    let id = summary
        .as_ref()
        .and_then(|s| s.get("snapshot_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("dry-run")
        .to_string();

    let total = summary
        .as_ref()
        .and_then(|s| s.get("total_bytes_processed"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Split into files vs db bytes based on scope.
    let (files_bytes, db_bytes) = match scope {
        BackupScope::Full => (total / 2, total / 2),
        BackupScope::FilesOnly => (total, 0),
        BackupScope::DatabaseOnly => (0, total),
    };

    Ok(Snapshot {
        id,
        size_bytes: total,
        files_bytes,
        db_bytes,
    })
}

/// List snapshots for a site.
/// One entry of `restic snapshots --json`.
///
/// Restic reports `id`, `time` and `tags`; `summary` (with the processed byte
/// count) is present from restic 0.17. An earlier version deserialised this
/// straight into [`Snapshot`], whose field names do not exist in restic's
/// output, so every listing silently came back empty.
#[derive(serde::Deserialize)]
struct ResticSnapshot {
    id: String,
    #[serde(default)]
    short_id: Option<String>,
    time: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    summary: Option<ResticSnapshotSummary>,
}

#[derive(serde::Deserialize)]
struct ResticSnapshotSummary {
    #[serde(default)]
    total_bytes_processed: u64,
}

/// Lists the snapshots this site owns, newest first, as panel-shaped backups.
pub async fn list(
    config: &Config,
    site: &SiteRecord,
    target: &ResticTarget,
) -> Result<Vec<wp_common::models::Backup>> {
    let tag = format!("site={}", site.domain);
    let args = restic_args(target, &["snapshots", "--json", "--tag", &tag]);
    let output = exec::run_with_env(config.dry_run, "restic", &args, &restic_env(target)).await?;

    if output.skipped {
        return Ok(Vec::new());
    }

    let snapshots: Vec<ResticSnapshot> = serde_json::from_str(output.trimmed_stdout())
        .map_err(|e| wp_common::Error::internal(format!("parsing restic snapshot list: {e}")))?;

    let mut backups: Vec<wp_common::models::Backup> = snapshots
        .into_iter()
        .map(|snapshot| {
            let scope = snapshot
                .tags
                .iter()
                .find_map(|tag| tag.strip_prefix("scope="))
                .map(|scope| match scope {
                    "files" => BackupScope::FilesOnly,
                    "database" => BackupScope::DatabaseOnly,
                    _ => BackupScope::Full,
                })
                .unwrap_or(BackupScope::Full);

            wp_common::models::Backup {
                // The panel keys rows by snapshot id, so ids come from restic.
                id: 0,
                site_id: site.site_id,
                snapshot_id: snapshot.short_id.unwrap_or(snapshot.id),
                size_bytes: snapshot
                    .summary
                    .map(|s| s.total_bytes_processed)
                    .unwrap_or(0),
                scope,
                destination: target.repo.clone(),
                created_at: chrono::DateTime::parse_from_rfc3339(&snapshot.time)
                    .map(|t| t.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now()),
            }
        })
        .collect();

    // Newest first: the panel renders the list in this order.
    backups.sort_by_key(|backup| std::cmp::Reverse(backup.created_at));
    Ok(backups)
}

/// Restore a snapshot.
pub async fn restore(
    config: &Config,
    site: &SiteRecord,
    snapshot_id: &str,
    scope: BackupScope,
    target: &ResticTarget,
) -> Result<()> {
    let root = config.site_root(&site.domain);
    let restore_dir = format!("/tmp/restore-{}", site.site_id);

    // 1. Restore to a temporary directory.
    let extra = &["restore", snapshot_id, "--target", &restore_dir];
    let args = restic_args(target, extra);
    let env = restic_env(target);
    exec::run_with_env(config.dry_run, "restic", &args, &env).await?;

    // 2. Sync files (unless database-only).
    if scope != BackupScope::DatabaseOnly {
        let src = format!("{}/", restore_dir);
        let dst = root.join("public_html").display().to_string();
        exec::run(config.dry_run, "rsync", &["-a", "--delete", &src, &dst]).await?;
    }

    // 3. Import database (unless files-only).
    if scope != BackupScope::FilesOnly {
        let dump = format!("{}/backups/database.sql", restore_dir);
        database::import(config, site, &dump).await?;
    }

    // 4. Fix ownership and clean up.
    exec::run(
        config.dry_run,
        "chown",
        &[
            "-R",
            &format!("{}:{}", site.uid, site.uid),
            &root.display().to_string(),
        ],
    )
    .await?;

    exec::run(config.dry_run, "rm", &["-rf", &restore_dir]).await?;

    Ok(())
}

/// Apply retention policy after a successful backup.
pub async fn prune(
    config: &Config,
    site: &SiteRecord,
    policy: wp_common::models::RetentionPolicy,
    target: &ResticTarget,
) -> Result<()> {
    let extra = &[
        "forget",
        "--prune",
        "--tag",
        &format!("site={}", site.domain),
        "--keep-hourly",
        &policy.hourly.to_string(),
        "--keep-daily",
        &policy.daily.to_string(),
        "--keep-weekly",
        &policy.weekly.to_string(),
        "--keep-monthly",
        &policy.monthly.to_string(),
    ];
    let args = restic_args(target, extra);
    let env = restic_env(target);
    exec::run_with_env(config.dry_run, "restic", &args, &env)
        .await
        .map(|_| ())
}
