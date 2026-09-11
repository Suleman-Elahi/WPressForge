//! Per-site users and directories.
//!
//! ```text
//! /var/www/example.com
//!   ├── public_html/   (WordPress)
//!   ├── logs/
//!   └── backups/
//! ```
//! Everything is owned by the site's dedicated UID so one site cannot touch
//! another's files.

use crate::config::Config;
use crate::exec;
use wp_common::Result;

pub async fn ensure_user(config: &Config, domain: &str, uid: u32) -> Result<()> {
    let user = system_user(domain);
    exec::run(
        config.dry_run,
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "--uid",
            &uid.to_string(),
            "--user-group",
            &user,
        ],
    )
    .await
    .map(|_| ())
}

pub async fn create_tree(config: &Config, domain: &str, uid: u32) -> Result<()> {
    let root = config.site_root(domain);
    for sub in ["public_html", "logs", "backups", "tmp"] {
        let path = root.join(sub);
        if config.dry_run {
            tracing::info!(path = %path.display(), "dry-run: would create directory");
        } else {
            tokio::fs::create_dir_all(&path)
                .await
                .map_err(wp_common::Error::internal)?;
        }
    }

    exec::run(
        config.dry_run,
        "chown",
        &[
            "-R".to_string(),
            format!("{uid}:{uid}"),
            root.display().to_string(),
        ],
    )
    .await?;

    exec::run(
        config.dry_run,
        "chmod",
        &["750".to_string(), root.display().to_string()],
    )
    .await
    .map(|_| ())
}

pub async fn remove_tree(config: &Config, domain: &str, keep_backups: bool) -> Result<()> {
    let root = config.site_root(domain);
    let targets = if keep_backups {
        vec![root.join("public_html"), root.join("logs"), root.join("tmp")]
    } else {
        vec![root.clone()]
    };

    for target in targets {
        exec::run(
            config.dry_run,
            "rm",
            &["-rf".to_string(), target.display().to_string()],
        )
        .await?;
    }

    Ok(())
}

pub async fn remove_user(config: &Config, domain: &str) -> Result<()> {
    exec::run(config.dry_run, "userdel", &[system_user(domain)])
        .await
        .map(|_| ())
}

/// Disk usage of a site in megabytes.
pub async fn disk_usage_mb(config: &Config, domain: &str) -> Result<u64> {
    let output = exec::run(
        config.dry_run,
        "du",
        &["-sm".to_string(), config.site_root(domain).display().to_string()],
    )
    .await?;

    Ok(output
        .trimmed_stdout()
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

/// `example.com` -> `wp_example_com`, a valid Linux user and MySQL identifier.
pub fn system_user(domain: &str) -> String {
    let sanitised: String = domain
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("wp_{}", sanitised.trim_matches('_').to_lowercase())
}
