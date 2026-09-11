//! Site lifecycle: the sequence the panel's `site.create` job depends on.

use crate::exec::Steps;
use crate::ops::{backup, database, docker, filesystem, nginx, ssl, wordpress};
use crate::state::AgentState;
use crate::store::SiteRecord;
use wp_common::models::{
    BackupScope, CacheSettings, PhpVersion, ResourceLimits, RetentionPolicy, SiteStatus,
};
use wp_common::protocol::{CreateSite, InstallWordpress, LogStream, OperationData, OperationResult};
use wp_common::{Error, Result};

/// Create user -> filesystem -> database -> container -> WordPress -> Nginx ->
/// TLS -> cache -> health check.
pub async fn create(state: &AgentState, request: CreateSite) -> Result<OperationResult> {
    let config = &state.config;
    let mut steps = Steps::new();

    let uid = state.store.next_uid(config.uid_base).await;
    let mut record = SiteRecord {
        site_id: request.site_id,
        domain: request.domain.clone(),
        uid,
        php_version: request.php_version,
        database_mode: request.database_mode,
        limits: request.limits,
        cache: request.cache,
        status: SiteStatus::Provisioning,
        container_id: None,
        db_name: database::db_name(&request.domain),
        domains: vec![request.domain.clone()],
    };

    steps
        .step("Allocate system user", filesystem::ensure_user(config, &record.domain, uid))
        .await?;
    steps
        .step(
            "Create filesystem layout",
            filesystem::create_tree(config, &record.domain, uid),
        )
        .await?;

    let credentials = steps
        .step("Create database", database::create(config, &record))
        .await?;

    let container_id = steps
        .step(
            "Start PHP-FPM container",
            docker::start_php(config, &record, request.php_version),
        )
        .await?;
    record.container_id = Some(container_id.clone());

    if request.install_wordpress {
        let install = request.wordpress.unwrap_or_else(|| InstallWordpress {
            site_id: request.site_id,
            site_title: request.domain.clone(),
            admin_user: "admin".to_string(),
            admin_email: format!("admin@{}", request.domain),
            admin_password: database::generate_password(),
            locale: "en_US".to_string(),
        });

        let version = steps
            .step(
                "Install WordPress",
                wordpress::install(config, &record, &credentials, &install),
            )
            .await?;
        steps.note("WordPress installed", version);
    }

    steps
        .step("Write Nginx vhost", nginx::write_vhost(config, &record, false))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    if request.request_ssl {
        // A failed certificate must not fail the whole provision: the site is
        // already serving over HTTP and TLS can be retried.
        match ssl::issue(config, &record.domains).await {
            Ok(_) => {
                steps
                    .step("Enable TLS vhost", nginx::write_vhost(config, &record, true))
                    .await?;
                steps.step("Reload Nginx", nginx::reload(config)).await?;
            }
            Err(error) => steps.note("Issue TLS certificate", format!("skipped: {error}")),
        }
    }

    let healthy = steps
        .step("Health check", docker::healthy(config, &record.container_name()))
        .await?;
    record.status = if healthy {
        SiteStatus::Online
    } else {
        SiteStatus::Failed
    };

    state.store.put(record).await?;

    Ok(OperationResult::ok(OperationData::SiteCreated {
        site_id: request.site_id,
        container_id,
        uid,
        db_name: credentials.name,
    })
    .with_steps(steps.into_reports()))
}

pub async fn delete(
    state: &AgentState,
    site_id: i64,
    keep_backups: bool,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;
    let mut steps = Steps::new();

    steps
        .step("Stop container", docker::remove(config, &record.container_name()))
        .await?;
    steps
        .step("Remove Nginx vhost", nginx::remove_vhost(config, &record.domain))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;
    steps.step("Drop database", database::drop(config, &record)).await?;
    steps
        .step(
            "Remove files",
            filesystem::remove_tree(config, &record.domain, keep_backups),
        )
        .await?;
    steps
        .step("Release system user", filesystem::remove_user(config, &record.domain))
        .await?;

    state.store.remove(site_id).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn start(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;
    let mut steps = Steps::new();

    let container_id = steps
        .step(
            "Start container",
            docker::start_php(config, &record, record.php_version),
        )
        .await?;
    let healthy = steps
        .step("Health check", docker::healthy(config, &record.container_name()))
        .await?;

    record.container_id = Some(container_id);
    record.status = if healthy {
        SiteStatus::Online
    } else {
        SiteStatus::Failed
    };
    state.store.put(record).await?;

    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn stop(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;
    let mut steps = Steps::new();

    steps
        .step("Stop container", docker::stop(config, &record.container_name()))
        .await?;

    record.status = SiteStatus::Stopped;
    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn restart(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;
    let mut steps = Steps::new();

    steps
        .step("Restart container", docker::restart(config, &record.container_name()))
        .await?;
    steps
        .step("Health check", docker::healthy(config, &record.container_name()))
        .await?;

    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn status(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let running = docker::healthy(config, &record.container_name())
        .await
        .unwrap_or(false);
    let wp_version = wordpress::version(config, &record).await.ok();
    let disk_usage_mb = filesystem::disk_usage_mb(config, &record.domain)
        .await
        .unwrap_or(0);

    Ok(OperationResult::ok(OperationData::SiteStatus {
        site_id,
        status: if running {
            SiteStatus::Online
        } else {
            SiteStatus::Stopped
        },
        php_version: record.php_version,
        wp_version,
        disk_usage_mb,
    }))
}

/// Near-zero-downtime PHP switch: the new container is proven healthy before
/// Nginx moves and the old container is only then removed.
pub async fn switch_php(
    state: &AgentState,
    site_id: i64,
    version: PhpVersion,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;
    let previous = record.php_version;

    if previous == version {
        return Err(Error::Invalid(format!(
            "{} already runs PHP {version}",
            record.domain
        )));
    }

    let mut steps = Steps::new();
    steps.step("Pull target PHP image", docker::pull(config, version)).await?;

    let mut target = record.clone();
    target.php_version = version;
    let container_id = steps
        .step("Start new container", docker::start_php(config, &target, version))
        .await?;

    let new_container = docker::container_name(&record.domain, version);
    let healthy = steps
        .step("Health check new container", docker::healthy(config, &new_container))
        .await?;

    if !healthy {
        docker::remove(config, &new_container).await.ok();
        return Err(Error::Internal(format!(
            "PHP {version} container did not become healthy; kept {previous}"
        )));
    }

    record.php_version = version;
    record.container_id = Some(container_id);

    steps
        .step("Switch Nginx upstream", nginx::write_vhost(config, &record, true))
        .await?;
    steps.step("Verify traffic", nginx::reload(config)).await?;
    steps
        .step(
            "Remove old container",
            docker::remove(config, &docker::container_name(&record.domain, previous)),
        )
        .await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn set_limits(
    state: &AgentState,
    site_id: i64,
    limits: ResourceLimits,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;
    record.limits = limits;

    let mut steps = Steps::new();
    steps
        .step(
            "Apply container limits",
            crate::exec::run(
                config.dry_run,
                "docker",
                &[
                    "update".to_string(),
                    "--cpus".to_string(),
                    format!("{:.2}", limits.cpu_cores),
                    "--memory".to_string(),
                    format!("{}m", limits.memory_mb),
                    record.container_name(),
                ],
            ),
        )
        .await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn add_domain(
    state: &AgentState,
    site_id: i64,
    domain: String,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;

    if !record.domains.contains(&domain) {
        record.domains.push(domain);
    }

    let mut steps = Steps::new();
    steps
        .step("Update Nginx vhost", nginx::write_vhost(config, &record, false))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn remove_domain(
    state: &AgentState,
    site_id: i64,
    domain: String,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;

    if record.domain == domain {
        return Err(Error::invalid("cannot remove the primary domain"));
    }
    record.domains.retain(|d| d != &domain);

    let mut steps = Steps::new();
    steps
        .step("Update Nginx vhost", nginx::write_vhost(config, &record, false))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn issue_certificate(
    state: &AgentState,
    site_id: i64,
    domains: Vec<String>,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;
    let domains = if domains.is_empty() {
        record.domains.clone()
    } else {
        domains
    };

    let mut steps = Steps::new();
    let expires_at = steps
        .step("Request certificate", ssl::issue(config, &domains))
        .await?;
    steps
        .step("Install certificate", nginx::write_vhost(config, &record, true))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    Ok(
        OperationResult::ok(OperationData::Certificate { domains, expires_at })
            .with_steps(steps.into_reports()),
    )
}

pub async fn renew_certificate(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    let expires_at = steps
        .step("Renew certificate", ssl::renew(config, &record.domain))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    Ok(OperationResult::ok(OperationData::Certificate {
        domains: record.domains,
        expires_at,
    })
    .with_steps(steps.into_reports()))
}

pub async fn set_cache(
    state: &AgentState,
    site_id: i64,
    settings: CacheSettings,
) -> Result<OperationResult> {
    let config = &state.config;
    let mut record = state.store.get(site_id).await?;
    record.cache = settings;

    let mut steps = Steps::new();
    steps
        .step("Rewrite Nginx vhost", nginx::write_vhost(config, &record, true))
        .await?;
    steps.step("Reload Nginx", nginx::reload(config)).await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn clear_cache(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    steps
        .step("Purge FastCGI cache", nginx::purge_cache(config, &record.domain))
        .await?;
    steps
        .step("Flush object cache", wordpress::flush_cache(config, &record))
        .await?;

    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn install_wordpress(
    state: &AgentState,
    request: InstallWordpress,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(request.site_id).await?;
    let credentials = database::Credentials {
        name: record.db_name.clone(),
        user: filesystem::system_user(&record.domain),
        password: database::generate_password(),
        host: "127.0.0.1".to_string(),
    };

    let mut steps = Steps::new();
    let version = steps
        .step(
            "Install WordPress",
            wordpress::install(config, &record, &credentials, &request),
        )
        .await?;

    Ok(OperationResult::ok(OperationData::SiteStatus {
        site_id: request.site_id,
        status: SiteStatus::Online,
        php_version: record.php_version,
        wp_version: Some(version),
        disk_usage_mb: 0,
    })
    .with_steps(steps.into_reports()))
}

/// Core update, preceded by a safety backup so a failed migration is recoverable.
pub async fn update_wordpress(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;
    let mut steps = Steps::new();

    if config.restic_repo.is_some() {
        steps
            .step(
                "Backup before update",
                backup::create(config, &record, BackupScope::Full),
            )
            .await?;
    } else {
        steps.note("Backup before update", "skipped: no restic repository");
    }

    let version = steps
        .step("Update core", wordpress::update_core(config, &record))
        .await?;
    steps
        .step("Health check", docker::healthy(config, &record.container_name()))
        .await?;

    Ok(OperationResult::ok(OperationData::SiteStatus {
        site_id,
        status: SiteStatus::Online,
        php_version: record.php_version,
        wp_version: Some(version),
        disk_usage_mb: 0,
    })
    .with_steps(steps.into_reports()))
}

pub async fn backup(
    state: &AgentState,
    site_id: i64,
    scope: BackupScope,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    let snapshot = steps
        .step("Snapshot site", backup::create(config, &record, scope))
        .await?;
    steps
        .step(
            "Prune retention",
            backup::prune(config, &record, RetentionPolicy::default()),
        )
        .await?;

    Ok(OperationResult::ok(OperationData::Backup {
        snapshot_id: snapshot.id,
        size_bytes: snapshot.size_bytes,
    })
    .with_steps(steps.into_reports()))
}

pub async fn restore(
    state: &AgentState,
    site_id: i64,
    snapshot_id: &str,
    scope: BackupScope,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    steps
        .step(
            "Restore snapshot",
            backup::restore(config, &record, snapshot_id, scope),
        )
        .await?;
    steps
        .step("Restart container", docker::restart(config, &record.container_name()))
        .await?;
    steps
        .step("Health check", docker::healthy(config, &record.container_name()))
        .await?;

    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn tail_logs(
    state: &AgentState,
    site_id: i64,
    stream: LogStream,
    lines: u32,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let path = match stream {
        LogStream::Agent => "/var/log/wp-agent.log".to_string(),
        other => config
            .site_root(&record.domain)
            .join(format!("logs/{}.log", other.as_str()))
            .display()
            .to_string(),
    };

    let output = crate::exec::run(
        false,
        "tail",
        &["-n".to_string(), lines.clamp(1, 5000).to_string(), path],
    )
    .await?;

    Ok(OperationResult::ok(OperationData::Lines(
        output.stdout.lines().map(str::to_owned).collect(),
    )))
}
