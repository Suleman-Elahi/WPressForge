use crate::config::Config;
use crate::exec::{self, Steps};
use crate::ops::{database, docker, filesystem, wordpress};
use crate::state::AgentState;
use std::os::unix::fs::PermissionsExt;
use wp_common::models::SiteStatus;
use wp_common::protocol::{ImportSource, OperationData, OperationResult, SshAuth};
use wp_common::{Error, Result};

struct TempKey {
    path: Option<std::path::PathBuf>,
}
impl TempKey {
    fn new(auth: &SshAuth) -> Result<Self> {
        match auth {
            SshAuth::PrivateKey(key) => {
                let id = uuid::Uuid::new_v4().to_string();
                let path = std::env::temp_dir().join(format!("wp_import_{id}.key"));
                std::fs::write(&path, key)
                    .map_err(|e| Error::internal(format!("failed to write temp key: {e}")))?;
                let mut perms = std::fs::metadata(&path)
                    .map_err(|e| Error::internal(format!("failed to read metadata: {e}")))?
                    .permissions();
                perms.set_mode(0o600);
                std::fs::set_permissions(&path, perms)
                    .map_err(|e| Error::internal(format!("failed to set permissions: {e}")))?;
                Ok(Self { path: Some(path) })
            }
            _ => Ok(Self { path: None }),
        }
    }
    fn ssh_args(&self) -> Vec<String> {
        if let Some(p) = &self.path {
            vec!["-i".to_string(), p.display().to_string()]
        } else {
            vec![]
        }
    }
}
impl Drop for TempKey {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

struct DumpCleanup(std::path::PathBuf);
impl Drop for DumpCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub async fn inspect(config: &Config, source: &ImportSource) -> Result<OperationResult> {
    if config.dry_run {
        return Ok(OperationResult::ok(OperationData::ImportInspection {
            wp_version: "6.7.1".into(),
            php_version: "8.1.0".into(),
            size_mb: 250,
            db_name: Some("demo_db".into()),
            db_user: Some("demo_user".into()),
        }));
    }

    let key = TempKey::new(&source.auth)?;
    let mut ssh_base = vec![
        "-p".to_string(),
        source.port.to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
    ];
    ssh_base.extend(key.ssh_args());
    let ssh_target = format!("{}@{}", source.user, source.host);

    let mut wp_ver_cmd = ssh_base.clone();
    wp_ver_cmd.push(ssh_target.clone());
    wp_ver_cmd.push(format!(
        "cat {}/wp-includes/version.php",
        source.remote_path
    ));
    let ver_out = exec::run(config.dry_run, "ssh", &wp_ver_cmd)
        .await
        .unwrap_or_default();
    let wp_version = ver_out
        .stdout
        .lines()
        .find(|l| l.contains("$wp_version ="))
        .and_then(|l| l.split('\'').nth(1))
        .unwrap_or("unknown")
        .to_string();

    let mut php_ver_cmd = ssh_base.clone();
    php_ver_cmd.push(ssh_target.clone());
    php_ver_cmd.push("php -v".to_string());
    let php_out = exec::run(config.dry_run, "ssh", &php_ver_cmd)
        .await
        .unwrap_or_default();
    let php_version = php_out
        .stdout
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_string();

    let mut du_cmd = ssh_base.clone();
    du_cmd.push(ssh_target.clone());
    du_cmd.push(format!("du -sm {}", source.remote_path));
    let du_out = exec::run(config.dry_run, "ssh", &du_cmd)
        .await
        .unwrap_or_default();
    let size_mb: u64 = du_out
        .stdout
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut db_name = None;
    let mut db_user = None;
    if source.db.is_none() {
        let mut grep_name = ssh_base.clone();
        grep_name.push(ssh_target.clone());
        grep_name.push(format!("grep DB_NAME {}/wp-config.php", source.remote_path));
        if let Ok(out) = exec::run(config.dry_run, "ssh", &grep_name).await {
            db_name = out
                .stdout
                .lines()
                .find(|l| l.contains("DB_NAME"))
                .and_then(|l| l.split('\'').nth(3))
                .map(|s| s.to_string());
        }

        let mut grep_user = ssh_base.clone();
        grep_user.push(ssh_target.clone());
        grep_user.push(format!("grep DB_USER {}/wp-config.php", source.remote_path));
        if let Ok(out) = exec::run(config.dry_run, "ssh", &grep_user).await {
            db_user = out
                .stdout
                .lines()
                .find(|l| l.contains("DB_USER"))
                .and_then(|l| l.split('\'').nth(3))
                .map(|s| s.to_string());
        }
    }

    Ok(OperationResult::ok(OperationData::ImportInspection {
        wp_version,
        php_version,
        size_mb,
        db_name,
        db_user,
    }))
}

pub async fn import(
    state: &AgentState,
    site_id: i64,
    source: &ImportSource,
    resync: bool,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut steps = Steps::new();
    let mut record = state.store.get(site_id).await?;

    if config.dry_run {
        return Ok(OperationResult::ok(OperationData::None));
    }

    let key = TempKey::new(&source.auth)?;
    let mut ssh_base = vec![
        "-p".to_string(),
        source.port.to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
    ];
    ssh_base.extend(key.ssh_args());
    let ssh_target = format!("{}@{}", source.user, source.host);

    // 1. Validate source
    let mut check_cmd = ssh_base.clone();
    check_cmd.push(ssh_target.clone());
    check_cmd.push("echo ok".to_string());
    steps
        .step(
            "Validate source",
            exec::run(config.dry_run, "ssh", &check_cmd),
        )
        .await?;

    if !resync {
        // 2. Allocate system user
        let uid = state.store.next_uid(config.uid_base).await;
        record.uid = uid;
        steps
            .step(
                "Allocate system user",
                filesystem::ensure_user(config, &record.domain, uid),
            )
            .await?;

        // 3. Create filesystem
        steps
            .step(
                "Create filesystem",
                filesystem::create_tree(config, &record.domain, uid),
            )
            .await?;
    }

    // 4. Copy files
    let mut rsync_e = "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new".to_string();
    rsync_e.push_str(&format!(" -p {}", source.port));
    if let Some(p) = &key.path {
        rsync_e.push_str(&format!(" -i {}", p.display()));
    }
    let dst = config.site_root(&record.domain).join("public_html");
    steps
        .step(
            "Copy files",
            exec::run(
                config.dry_run,
                "rsync",
                &[
                    "-az".to_string(),
                    "--delete".to_string(),
                    "--exclude".to_string(),
                    "wp-config.php".to_string(),
                    "--exclude".to_string(),
                    "wp-content/cache/".to_string(),
                    "-e".to_string(),
                    rsync_e,
                    format!("{}:{}/", ssh_target, source.remote_path),
                    format!("{}/", dst.display()),
                ],
            ),
        )
        .await?;

    // 5. Export database
    let db_info = source
        .db
        .as_ref()
        .ok_or_else(|| Error::invalid("remote db credentials required for import"))?;
    let mut dump_cmd = ssh_base.clone();
    dump_cmd.push(ssh_target.clone());
    dump_cmd.push(format!(
        "mysqldump -h{} -P{} -u{} -p'{}' --single-transaction {}",
        db_info.host, db_info.port, db_info.user, db_info.password, db_info.name
    ));

    let dump_out = steps
        .step(
            "Export database",
            exec::run(config.dry_run, "ssh", &dump_cmd),
        )
        .await?;
    let dump_file = std::env::temp_dir().join(format!("wp_import_db_{}.sql", uuid::Uuid::new_v4()));
    std::fs::write(&dump_file, &dump_out.stdout)
        .map_err(|e| Error::internal(format!("failed to write db dump: {e}")))?;
    let _dump_cleanup = DumpCleanup(dump_file.clone());

    let credentials = database::Credentials {
        name: record.db_name.clone(),
        user: filesystem::system_user(&record.domain),
        password: database::generate_password(),
        host: "127.0.0.1".to_string(),
    };

    if !resync {
        // 6. Create database
        steps
            .step("Create database", database::create(config, &record))
            .await?;
    }

    // 7. Import database
    steps
        .step(
            "Import database",
            database::import(config, &record, &dump_file.display().to_string()),
        )
        .await?;

    // 8. Start container (Swapped to ensure exec works)
    let container_id = steps
        .step(
            "Start container",
            docker::start_php(config, &record, record.php_version),
        )
        .await?;
    record.container_id = Some(container_id);

    // 9. Configure WordPress
    steps
        .step(
            "Configure WordPress",
            wordpress::wp(
                config,
                &record.container_name(),
                &[
                    "config",
                    "create",
                    &format!("--dbname={}", credentials.name),
                    &format!("--dbuser={}", credentials.user),
                    &format!("--dbpass={}", credentials.password),
                    "--dbhost=127.0.0.1",
                    "--allow-root",
                    "--force",
                ],
            ),
        )
        .await?;

    // 10. Search & replace
    steps
        .step(
            "Search & replace",
            wordpress::search_replace(config, &record, &db_info.name, &record.domain),
        )
        .await
        .ok();

    // 11. Verify checksums
    if let Ok(out) = steps
        .step(
            "Verify checksums",
            wordpress::wp(
                config,
                &record.container_name(),
                &["core", "verify-checksums", "--allow-root"],
            ),
        )
        .await
    {
        steps.note("Verify checksums", out.stdout);
    }

    // 12. Write Nginx vhost
    steps
        .step("Write Nginx vhost", state.web.write_site(&record, false))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

    // 13. Health check
    let healthy = steps
        .step(
            "Health check",
            docker::healthy(config, &record.container_name()),
        )
        .await?;
    record.status = if healthy {
        SiteStatus::Online
    } else {
        SiteStatus::Failed
    };
    state.store.put(record).await?;

    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dry_run_inspect_returns_demo_data() {
        let config = Config {
            dry_run: true,
            ..Default::default()
        };
        let source = ImportSource {
            host: "remote.example.com".into(),
            port: 22,
            user: "root".into(),
            auth: SshAuth::Password("secret".into()),
            remote_path: "/var/www/html".into(),
            db: None,
        };

        let result = inspect(&config, &source).await.expect("inspect succeeds");
        assert!(result.success);
        match result.data {
            OperationData::ImportInspection {
                wp_version,
                php_version,
                size_mb,
                ..
            } => {
                assert_eq!(wp_version, "6.7.1");
                assert_eq!(php_version, "8.1.0");
                assert_eq!(size_mb, 250);
            }
            other => panic!("unexpected data: {other:?}"),
        }
    }
}
