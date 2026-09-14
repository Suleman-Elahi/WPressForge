//! Site lifecycle: the sequence the panel's `site.create` job depends on.

use crate::exec::{self, Steps};
use crate::ops::{backup, database, docker, filesystem, mu_plugin, ssl, wordpress};
use crate::state::AgentState;
use crate::store::SiteRecord;
use wp_common::models::{
    BackupScope, CacheSettings, PhpVersion, ResourceLimits, RetentionPolicy, SiteMetricSample,
    SiteStatus,
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
        .step("Write Nginx vhost", state.web.write_site(&record, false))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

    // Write the cache mu-plugin (hooks WordPress to purge Nginx cache on changes).
    steps
        .step(
            "Write cache mu-plugin",
            mu_plugin::write_mu_plugin(config, &record),
        )
        .await?;

    if request.request_ssl {
        // A failed certificate must not fail the whole provision: the site is
        // already serving over HTTP and TLS can be retried.
        match ssl::issue(config, &record.domains).await {
            Ok(_) => {
                steps
                    .step("Enable TLS vhost", state.web.write_site(&record, true))
                    .await?;
                steps.step("Reload Nginx", state.web.reload()).await?;
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
        .step("Remove Nginx vhost", state.web.remove_site(&record.domain))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;
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
        .step("Switch Nginx upstream", state.web.write_site(&record, true))
        .await?;
    steps.step("Verify traffic", state.web.reload()).await?;
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
    let mut record = state.store.get(site_id).await?;

    if !record.domains.contains(&domain) {
        record.domains.push(domain);
    }

    let mut steps = Steps::new();
    steps
        .step("Update Nginx vhost", state.web.write_site(&record, false))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn remove_domain(
    state: &AgentState,
    site_id: i64,
    domain: String,
) -> Result<OperationResult> {
    let mut record = state.store.get(site_id).await?;

    if record.domain == domain {
        return Err(Error::invalid("cannot remove the primary domain"));
    }
    record.domains.retain(|d| d != &domain);

    let mut steps = Steps::new();
    steps
        .step("Update Nginx vhost", state.web.write_site(&record, false))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

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
        .step("Install certificate", state.web.write_site(&record, true))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

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
    steps.step("Reload Nginx", state.web.reload()).await?;

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
        .step("Rewrite Nginx vhost", state.web.write_site(&record, true))
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

    // Ensure the mu-plugin is present (may have been removed or is first install).
    steps
        .step(
            "Write cache mu-plugin",
            mu_plugin::write_mu_plugin(config, &record),
        )
        .await?;

    state.store.put(record).await?;
    Ok(OperationResult::ok(OperationData::None).with_steps(steps.into_reports()))
}

pub async fn clear_cache(state: &AgentState, site_id: i64) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    steps
        .step("Purge FastCGI cache", state.web.purge(&record.domain))
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
        // Build a fallback target from the config for the safety backup.
        let target = wp_common::protocol::ResticTarget {
            repo: config.restic_repo.clone().unwrap_or_default(),
            password: String::new(),
            env: Vec::new(),
        };
        steps
            .step(
                "Backup before update",
                backup::create(config, &record, BackupScope::Full, &target),
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
    target: wp_common::protocol::ResticTarget,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    let snapshot = steps
        .step("Snapshot site", backup::create(config, &record, scope, &target))
        .await?;
    steps
        .step(
            "Prune retention",
            backup::prune(config, &record, RetentionPolicy::default(), &target),
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
    target: wp_common::protocol::ResticTarget,
) -> Result<OperationResult> {
    let config = &state.config;
    let record = state.store.get(site_id).await?;

    let mut steps = Steps::new();
    steps
        .step(
            "Restore snapshot",
            backup::restore(config, &record, snapshot_id, scope, &target),
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
    grep_pattern: Option<String>,
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

    // Validate grep pattern: max 64 chars, no regex metacharacters (use -F for fixed string).
    let safe_grep = grep_pattern
        .filter(|p| !p.is_empty() && p.len() <= 64)
        .map(|p| p.replace(|c: char| matches!(c, '\\' | '[' | ']' | '(' | ')' | '+' | '?' | '{' | '}' | '^' | '$' | '.' | '*' | '|' | '&' | ';'), ""));

    let output = if let Some(ref pattern) = safe_grep {
        crate::exec::run(
            false,
            "sh",
            &[
                "-c",
                &format!("tail -n {} {} | grep -F {}", lines.clamp(1, 5000), path, pattern),
            ],
        )
        .await?
    } else {
        crate::exec::run(
            false,
            "tail",
            &["-n".to_string(), lines.clamp(1, 5000).to_string(), path],
        )
        .await?
    };

    Ok(OperationResult::ok(OperationData::Lines(
        output.stdout.lines().map(str::to_owned).collect(),
    )))
}

// ---------------------------------------------------------------------------
// Cloning & staging
// ---------------------------------------------------------------------------

pub async fn clone(
    state: &AgentState,
    request: wp_common::protocol::CloneSite,
) -> Result<OperationResult> {
    let config = &state.config;
    let source = state.store.get(request.source_site_id).await?;

    // Refuse if source and target are the same domain.
    if source.domain == request.target_domain {
        return Err(Error::Invalid("source and target domains are the same".into()));
    }

    let mut steps = Steps::new();

    // 1. Allocate target UID and create site record.
    let target_uid = state.store.next_uid(config.uid_base).await;
    let target_record = SiteRecord {
        site_id: request.target_site_id,
        domain: request.target_domain.clone(),
        uid: target_uid,
        php_version: request.php_version,
        database_mode: source.database_mode.clone(),
        limits: source.limits.clone(),
        cache: source.cache.clone(),
        status: SiteStatus::Provisioning,
        container_id: None,
        db_name: database::db_name(&request.target_domain),
        domains: vec![request.target_domain.clone()],
    };

    // 2. Allocate system user.
    steps
        .step(
            "Allocate system user",
            filesystem::ensure_user(config, &target_record.domain, target_uid),
        )
        .await?;

    // 3. Create filesystem layout.
    steps
        .step(
            "Create filesystem layout",
            filesystem::create_tree(config, &target_record.domain, target_uid),
        )
        .await?;

    // 4. Copy files (exclude cache, upgrade, and SQL dumps).
    let src_root = config.site_root(&source.domain);
    let dst_root = config.site_root(&target_record.domain);
    let src_public = src_root.join("public_html");
    let dst_public = dst_root.join("public_html");

    steps
        .step(
            "Copy files",
            exec::run(
                config.dry_run,
                "rsync",
                &[
                    "-a",
                    "--delete",
                    "--exclude",
                    "wp-content/cache/",
                    "--exclude",
                    "wp-content/upgrade/",
                    "--exclude",
                    "*.sql",
                    &format!("{}/", src_public.display()),
                    &format!("{}/", dst_public.display()),
                ],
            ),
        )
        .await?;

    // 5. Create database.
    let credentials = steps
        .step("Create database", database::create(config, &target_record))
        .await?;

    // 6. Copy database.
    steps
        .step(
            "Copy database",
            exec::run(
                config.dry_run,
                "sh",
                &[
                    "-c",
                    &format!(
                        "mysqldump -u{} -p'{}' {} | mysql -u{} -p'{}' {}",
                        source.db_name,
                        "",  // Source DB password (from wp-config)
                        source.db_name,
                        credentials.user,
                        credentials.password,
                        credentials.name,
                    ),
                ],
            ),
        )
        .await?;

    // 7. Rewrite wp-config in target.
    steps
        .step(
            "Configure database in target",
            wordpress::set_db_config(config, &target_record, &credentials),
        )
        .await?;

    // 8. Start PHP-FPM container.
    let container_id = steps
        .step(
            "Start PHP-FPM container",
            docker::start_php(config, &target_record, request.php_version),
        )
        .await?;

    // 9. Search and replace URLs.
    if request.search_replace {
        steps
            .step(
                "Search & replace URLs",
                wordpress::search_replace(config, &target_record, &source.domain, &request.target_domain),
            )
            .await?;
    }

    // 10. Staging hygiene.
    if request.staging {
        steps
            .step(
                "Set blog_public to 0",
                wordpress::wp(
                    config,
                    &target_record.container_name(),
                    &["option", "update", "blog_public", "0"],
                ),
            )
            .await?;
        steps
            .step(
                "Set environment type to staging",
                exec::run(
                    config.dry_run,
                    "sh",
                    &[
                        "-c",
                        &format!(
                            "docker exec {} wp config set WP_ENVIRONMENT_TYPE staging --raw --allow-root",
                            target_record.container_name(),
                        ),
                    ],
                ),
            )
            .await?;
    }

    // 11. Write Nginx vhost.
    steps
        .step(
            "Write Nginx vhost",
            state.web.write_site(&target_record, false),
        )
        .await?;
    steps.step("Reload Nginx", state.web.reload()).await?;

    // 12. Health check.
    let healthy = steps
        .step(
            "Health check",
            docker::healthy(config, &target_record.container_name()),
        )
        .await?;

    let status = if healthy {
        SiteStatus::Online
    } else {
        SiteStatus::Failed
    };

    state.store.put(target_record).await?;

    Ok(OperationResult::ok(OperationData::SiteCreated {
        site_id: request.target_site_id,
        container_id,
        uid: target_uid,
        db_name: credentials.name,
    })
    .with_steps(steps.into_reports()))
}

// ---------------------------------------------------------------------------
// Site metrics
// ---------------------------------------------------------------------------

pub async fn get_site_metrics(
    state: &AgentState,
    site_ids: Vec<i64>,
) -> Result<Vec<SiteMetricSample>> {
    let config = &state.config;

    // Collect container stats via docker stats --no-stream.
    let output = crate::exec::run(
        false,
        "docker",
        &[
            "stats",
            "--no-stream",
            "--format",
            "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}",
        ],
    )
    .await?;

    // Parse docker stats into a map: container_name -> (cpu%, mem_bytes)
    let mut container_stats: std::collections::HashMap<String, (f32, u64)> =
        std::collections::HashMap::new();
    for line in output.stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let name = parts[0].trim();
            let cpu_pct: f32 = parts[1]
                .trim()
                .trim_end_matches('%')
                .parse()
                .unwrap_or(0.0);
            let mem_str = parts[2].trim();
            let mem_bytes = parse_memory_value(mem_str);
            container_stats.insert(name.to_string(), (cpu_pct, mem_bytes));
        }
    }

    let mut samples = Vec::new();
    for site_id in &site_ids {
        if let Ok(record) = state.store.get(*site_id).await {
            let container_name = record.container_name();
            let (cpu_percent, mem_bytes) = container_stats
                .get(&container_name)
                .copied()
                .unwrap_or((0.0, 0));

            // PHP-FPM busy workers: try to read from status page via docker exec.
            let php_busy = get_php_fpm_busy(state, &record).await;

            samples.push(SiteMetricSample {
                site_id: *site_id,
                cpu_percent,
                memory_mb: mem_bytes / (1024 * 1024),
                php_busy_workers: php_busy,
                cache_hit_ratio: None,
            });
        }
    }

    Ok(samples)
}

fn parse_memory_value(s: &str) -> u64 {
    let s = s.trim();
    if let Some(pos) = s.find('/') {
        let used = s[..pos].trim();
        return parse_single_memory(used);
    }
    parse_single_memory(s)
}

fn parse_single_memory(s: &str) -> u64 {
    let s = s.trim();
    let (num, unit) = if let Some(pos) = s.rfind(|c: char| c.is_alphabetic()) {
        (&s[..=pos], s[pos..].trim())
    } else {
        (s, "")
    };
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    match unit.to_lowercase().as_str() {
        "b" => n as u64,
        "kb" | "kib" => (n * 1024.0) as u64,
        "mb" | "mib" => (n * 1024.0 * 1024.0) as u64,
        "gb" | "gib" => (n * 1024.0 * 1024.0 * 1024.0) as u64,
        "tb" | "tib" => (n * 1024.0 * 1024.0 * 1024.0 * 1024.0) as u64,
        _ => n as u64,
    }
}

async fn get_php_fpm_busy(
    state: &AgentState,
    record: &SiteRecord,
) -> u32 {
    let config = &state.config;
    // Try reading PHP-FPM status page.
    let result = crate::exec::run(
        config.dry_run,
        "sh",
        &[
            "-c",
            &format!(
                "docker exec {} curl -s http://127.0.0.1:9000/status 2>/dev/null | grep 'active processes:' | awk '{{print $3}}'",
                record.container_name()
            ),
        ],
    )
    .await;
    match result {
        Ok(output) => output.stdout.trim().parse().unwrap_or(0),
        Err(_) => 0,
    }
}
