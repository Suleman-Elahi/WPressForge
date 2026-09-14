//! The job system. Every mutating action the user triggers becomes a row in
//! `jobs`, is picked up by a worker, translated into a typed agent operation and
//! reported back step by step.
//!
//! When the target server has no reachable agent and the panel runs with demo
//! data, the worker walks through the same step plan locally so the UI and the
//! state machine can be exercised end to end. That path is clearly labelled in
//! the job message and never touches a real host.

use crate::db::{self, jobs::JobRow, servers::ServerRow};
use crate::state::AppState;
use chrono::{Duration as ChronoDuration, Utc};
use std::time::{Duration, Instant};
use wp_common::Error as AgentError;
use wp_common::models::{
    BackupScope, CronMode, JobKind, JobStatus, PhpVersion, ServerStatus, SiteStatus, SslIssuer,
    WpItemAction,
};
use wp_common::protocol::{CreateSite, Operation, OperationData};

/// Ordered step plan per job kind. Also drives the progress bar.
pub fn plan(kind: JobKind) -> &'static [&'static str] {
    match kind {
        JobKind::SiteCreate => &[
            "Allocate system user",
            "Create filesystem layout",
            "Create database",
            "Start PHP-FPM container",
            "Install WordPress",
            "Write Nginx vhost",
            "Issue TLS certificate",
            "Enable FastCGI cache",
            "Health check",
        ],
        JobKind::SiteDelete => &[
            "Stop container",
            "Remove Nginx vhost",
            "Drop database",
            "Remove files",
            "Release system user",
        ],
        JobKind::SiteClone => &[
            "Snapshot source",
            "Copy files",
            "Copy database",
            "Search & replace URLs",
            "Start container",
            "Write Nginx vhost",
            "Health check",
        ],
        JobKind::SiteStart => &["Start container", "Health check"],
        JobKind::SiteStop => &["Drain requests", "Stop container"],
        JobKind::SiteRestart => &["Stop container", "Start container", "Health check"],
        JobKind::PhpSwitch => &[
            "Pull target PHP image",
            "Start new container",
            "Health check new container",
            "Switch Nginx upstream",
            "Verify traffic",
            "Remove old container",
        ],
        JobKind::WordpressInstall => &["Download core", "Configure wp-config", "Run installer"],
        JobKind::WordpressUpdate => &["Backup", "Update core", "Update database", "Health check"],
        JobKind::BackupCreate => &[
            "Dump database",
            "Snapshot files",
            "Upload to destination",
            "Prune retention",
        ],
        JobKind::BackupRestore => &[
            "Fetch snapshot",
            "Restore database",
            "Restore files",
            "Fix permissions",
            "Health check",
        ],
        JobKind::SslIssue => &[
            "Verify DNS",
            "Request certificate",
            "Install certificate",
            "Reload Nginx",
        ],
        JobKind::SslRenew => &["Renew certificate", "Reload Nginx"],
        JobKind::CacheClear => &["Purge FastCGI cache", "Flush object cache"],
        JobKind::StagingCreate => &[
            "Create staging site",
            "Copy files",
            "Copy database",
            "Search & replace URLs",
            "Health check",
        ],
        JobKind::StagingPush => &[
            "Backup production",
            "Copy files",
            "Copy database",
            "Search & replace URLs",
            "Health check",
        ],
        // M2: WordPress management
        JobKind::PluginAction => &["Run WP-CLI", "Verify site responds"],
        JobKind::PluginUpdateAll => &["Backup", "Update plugins", "Verify site responds"],
        JobKind::ThemeAction => &["Run WP-CLI", "Verify site responds"],
        JobKind::WpUserPasswordReset => &["Generate password", "Apply"],
        JobKind::CronRun => &["Run due events"],
        JobKind::CronModeSet => &["Update wp-config", "Write system cron"],
        JobKind::ImportRun => &[
            "Validate source",
            "Allocate system user",
            "Create filesystem layout",
            "Copy files",
            "Export database",
            "Create database",
            "Import database",
            "Start container",
            "Configure WordPress",
            "Search & replace",
            "Verify checksums",
            "Write Nginx vhost",
            "Health check",
        ],
    }
}

/// Spawns `count` workers plus the server heartbeat loop.
pub fn spawn(state: AppState, count: usize) {
    for worker in 0..count.max(1) {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                match db::jobs::claim_next(&state.db).await {
                    Ok(Some(job)) => {
                        let id = job.job.id;
                        tracing::info!(
                            worker,
                            job = id,
                            kind = job.job.kind.as_str(),
                            "job started"
                        );
                        if let Err(error) = run(&state, job).await {
                            tracing::warn!(job = id, %error, "job failed");
                            let _ = db::jobs::finish(
                                &state.db,
                                id,
                                JobStatus::Failed,
                                "Failed",
                                Some(&error.to_string()),
                            )
                            .await;
                        }
                    }
                    Ok(None) => {
                        // Idle: wake on the next enqueue, but re-check
                        // periodically in case another process inserted a job.
                        tokio::select! {
                            _ = state.job_signal.notified() => {}
                            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "claiming job");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    tokio::spawn(heartbeat_loop(state));
}

/// Polls every server's agent so the dashboard shows fresh status and metrics.
async fn heartbeat_loop(state: AppState) {
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    loop {
        ticker.tick().await;

        let servers = match db::servers::list(&state.db).await {
            Ok(servers) => servers,
            Err(error) => {
                tracing::error!(%error, "listing servers for heartbeat");
                continue;
            }
        };

        for row in servers {
            if is_demo(&row) {
                continue; // Keep the example server visibly "online".
            }

            let conn = crate::agent::ServerConnection {
                url: row.server.agent_url.clone(),
                token: row.agent_token.clone(),
                fingerprint: row.agent_fingerprint.clone(),
            };

            let result = state.agent.metrics(&conn).await;

            let (status, version, metrics) = match result {
                Ok(result) => match result.data {
                    OperationData::Metrics(metrics) => (ServerStatus::Online, None, Some(metrics)),
                    _ => (ServerStatus::Degraded, None, None),
                },
                Err(error) => {
                    tracing::debug!(server = row.server.id, %error, "heartbeat failed");
                    (ServerStatus::Offline, None, None)
                }
            };

            let _ = db::servers::record_heartbeat(
                &state.db,
                row.server.id,
                status,
                version,
                metrics.as_ref(),
            )
            .await;

            // Store server metrics history.
            if let Some(ref m) = metrics {
                let _ = db::metrics::insert_server(
                    &state.db,
                    row.server.id,
                    m.cpu_percent as f64,
                    m.memory_percent as f64,
                    m.disk_percent as f64,
                    m.load_1m as f64,
                    m.sites as i64,
                )
                .await;
            }

            // Collect per-site metrics for online servers.
            if status == ServerStatus::Online {
                let site_ids: Vec<i64> = db::sites::list_for_server(&state.db, row.server.id)
                    .await
                    .map(|rows| rows.into_iter().map(|r| r.site.id).collect())
                    .unwrap_or_default();

                if !site_ids.is_empty() {
                    match state
                        .agent
                        .query(
                            &conn,
                            wp_common::protocol::Operation::GetSiteMetrics {
                                site_ids: site_ids.clone(),
                            },
                        )
                        .await
                    {
                        Ok(wp_common::protocol::OperationData::SiteMetrics(samples)) => {
                            for sample in &samples {
                                let _ = db::metrics::insert_site(&state.db, sample).await;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(server = row.server.id, %e, "site metrics query failed");
                        }
                        _ => {}
                    }
                }
            }
        }

        // Retention: delete old history and downsample.
        let _ = db::metrics::cleanup(&state.db, 30).await;
        let _ = db::metrics::downsample(&state.db).await;

        let _ = db::users::purge_expired_sessions(&state.db).await;
        let _ = crate::auth::prune_old_attempts(&state.db).await;
    }
}

fn is_demo(row: &ServerRow) -> bool {
    row.agent_token == "demo-token"
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

async fn run(state: &AppState, job: JobRow) -> anyhow::Result<()> {
    let job_id = job.job.id;
    let kind = job.job.kind;
    let payload = db::jobs::payload(&state.db, job_id)
        .await?
        .unwrap_or_default();

    let server = match job.job.server_id {
        Some(id) => db::servers::get(&state.db, id).await?,
        None => None,
    };

    let operation = build_operation(state, &job, &payload).await;

    let outcome = match (&server, &operation) {
        (Some(server), Some(operation)) if !is_demo(server) => state
            .agent
            .send(
                &crate::agent::ServerConnection {
                    url: server.server.agent_url.clone(),
                    token: server.agent_token.clone(),
                    fingerprint: server.agent_fingerprint.clone(),
                },
                operation.clone(),
                Some(job_id),
            )
            .await
            .map(Some),
        _ => Ok(None),
    };

    match outcome {
        // Real agent answered.
        Ok(Some(result)) if result.success => {
            for step in &result.steps {
                db::jobs::add_step(
                    &state.db,
                    job_id,
                    &step.name,
                    step.ok,
                    step.duration_ms,
                    step.detail.as_deref(),
                )
                .await?;
            }
            apply_effects(state, &job, Some(&result.data)).await?;
            db::jobs::finish(&state.db, job_id, JobStatus::Succeeded, "Completed", None).await?;
        }
        Ok(Some(result)) => {
            let error = result
                .error
                .unwrap_or_else(|| AgentError::internal("agent reported failure"));
            mark_site_failed(state, &job).await?;
            db::jobs::finish(
                &state.db,
                job_id,
                JobStatus::Failed,
                "Failed",
                Some(&error.to_string()),
            )
            .await?;
        }
        // No agent (demo server, or no operation mapping yet).
        Ok(None) => {
            simulate(state, &job).await?;
        }
        Err(error) => {
            if state.config.demo_data && matches!(error, AgentError::Unreachable(_)) {
                tracing::warn!(job = job_id, "agent unreachable, running simulated plan");
                simulate(state, &job).await?;
            } else {
                mark_site_failed(state, &job).await?;
                db::jobs::finish(
                    &state.db,
                    job_id,
                    JobStatus::Failed,
                    "Failed",
                    Some(&error.to_string()),
                )
                .await?;
            }
        }
    }

    let _ = kind; // kept for tracing clarity above
    Ok(())
}

/// Walks the step plan locally, so the panel is usable without a node.
async fn simulate(state: &AppState, job: &JobRow) -> anyhow::Result<()> {
    let job_id = job.job.id;
    let steps = plan(job.job.kind);

    for (index, name) in steps.iter().enumerate() {
        let started = Instant::now();
        db::jobs::progress(
            &state.db,
            job_id,
            ((index * 100) / steps.len().max(1)) as u8,
            name,
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(450)).await;
        db::jobs::add_step(
            &state.db,
            job_id,
            name,
            true,
            started.elapsed().as_millis() as u64,
            None,
        )
        .await?;
    }

    apply_effects(state, job, None).await?;
    db::jobs::finish(
        &state.db,
        job_id,
        JobStatus::Succeeded,
        "Completed (simulated: no agent attached)",
        None,
    )
    .await?;
    Ok(())
}

/// Maps a job row + payload to the agent operation that performs it.
/// Now async because backup operations need to resolve destinations from the database.
async fn build_operation(
    state: &AppState,
    job: &JobRow,
    payload: &serde_json::Value,
) -> Option<Operation> {
    let site_id = job.job.site_id?;

    Some(match job.job.kind {
        JobKind::SiteCreate => Operation::CreateSite(CreateSite {
            site_id,
            domain: job.site_domain.clone().unwrap_or_default(),
            php_version: payload
                .get("php_version")
                .and_then(|v| v.as_str())
                .and_then(PhpVersion::parse)
                .unwrap_or(PhpVersion::Php84),
            database_mode: serde_json::from_value(
                payload.get("database_mode").cloned().unwrap_or_default(),
            )
            .unwrap_or(wp_common::models::DatabaseMode::Shared),
            limits: serde_json::from_value(payload.get("limits").cloned().unwrap_or_default())
                .unwrap_or_default(),
            cache: serde_json::from_value(payload.get("cache").cloned().unwrap_or_default())
                .unwrap_or_default(),
            install_wordpress: payload
                .get("install_wordpress")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            request_ssl: payload
                .get("request_ssl")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            wordpress: None,
        }),
        JobKind::SiteDelete => Operation::DeleteSite {
            site_id,
            keep_backups: payload
                .get("keep_backups")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        },
        JobKind::SiteStart => Operation::StartSite { site_id },
        JobKind::SiteStop => Operation::StopSite { site_id },
        JobKind::SiteRestart => Operation::RestartSite { site_id },
        JobKind::PhpSwitch => Operation::SwitchPhp {
            site_id,
            version: payload
                .get("php_version")
                .and_then(|v| v.as_str())
                .and_then(PhpVersion::parse)
                .unwrap_or(PhpVersion::Php84),
        },
        JobKind::CacheClear => Operation::ClearCache { site_id },
        JobKind::WordpressUpdate => Operation::UpdateWordpress { site_id },
        JobKind::SslIssue => Operation::IssueCertificate {
            site_id,
            domains: payload
                .get("domains")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_else(|| vec![job.site_domain.clone().unwrap_or_default()]),
        },
        JobKind::SslRenew => Operation::RenewCertificate { site_id },
        JobKind::BackupCreate => {
            let destination_id = payload.get("destination_id").and_then(|v| v.as_i64());
            let target = match destination_id {
                Some(id) => {
                    let dest = db::destinations::get(&state.db, id).await.ok().flatten()?;
                    let creds =
                        db::destinations::decrypt_credentials(&dest, &state.secrets).ok()?;
                    let endpoint = dest.endpoint.as_deref().unwrap_or("s3.amazonaws.com");
                    wp_common::protocol::ResticTarget {
                        repo: format!("s3:{}/{}", endpoint, dest.bucket),
                        password: creds.restic_password,
                        env: vec![
                            ("AWS_ACCESS_KEY_ID".into(), creds.access_key_id),
                            ("AWS_SECRET_ACCESS_KEY".into(), creds.secret),
                        ],
                    }
                }
                None => wp_common::protocol::ResticTarget {
                    repo: String::new(),
                    password: String::new(),
                    env: Vec::new(),
                },
            };
            Operation::CreateBackup {
                site_id,
                scope: serde_json::from_value(payload.get("scope").cloned().unwrap_or_default())
                    .unwrap_or(BackupScope::Full),
                target,
                retention: wp_common::models::RetentionPolicy::default(),
            }
        }
        JobKind::BackupRestore => {
            let destination_id = payload.get("destination_id").and_then(|v| v.as_i64());
            let target = match destination_id {
                Some(id) => {
                    let dest = db::destinations::get(&state.db, id).await.ok().flatten()?;
                    let creds =
                        db::destinations::decrypt_credentials(&dest, &state.secrets).ok()?;
                    let endpoint = dest.endpoint.as_deref().unwrap_or("s3.amazonaws.com");
                    wp_common::protocol::ResticTarget {
                        repo: format!("s3:{}/{}", endpoint, dest.bucket),
                        password: creds.restic_password,
                        env: vec![
                            ("AWS_ACCESS_KEY_ID".into(), creds.access_key_id),
                            ("AWS_SECRET_ACCESS_KEY".into(), creds.secret),
                        ],
                    }
                }
                None => wp_common::protocol::ResticTarget {
                    repo: String::new(),
                    password: String::new(),
                    env: Vec::new(),
                },
            };
            Operation::RestoreBackup {
                site_id,
                snapshot_id: payload
                    .get("snapshot_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                scope: serde_json::from_value(payload.get("scope").cloned().unwrap_or_default())
                    .unwrap_or(BackupScope::Full),
                target,
            }
        }
        // M2: WordPress management
        JobKind::PluginAction => Operation::PluginAction {
            site_id,
            slug: payload
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            action: serde_json::from_value(payload.get("action").cloned().unwrap_or_default())
                .unwrap_or(WpItemAction::Activate),
        },
        JobKind::PluginUpdateAll => Operation::UpdateAllPlugins { site_id },
        JobKind::ThemeAction => Operation::ThemeAction {
            site_id,
            slug: payload
                .get("slug")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            action: serde_json::from_value(payload.get("action").cloned().unwrap_or_default())
                .unwrap_or(WpItemAction::Activate),
        },
        JobKind::WpUserPasswordReset => Operation::ResetWpPassword {
            site_id,
            user_login: payload
                .get("user_login")
                .and_then(|v| v.as_str())
                .unwrap_or("admin")
                .to_string(),
        },
        JobKind::CronRun => Operation::RunCronEvent {
            site_id,
            hook: payload
                .get("hook")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        JobKind::CronModeSet => Operation::SetWpCron {
            site_id,
            mode: serde_json::from_value(payload.get("mode").cloned().unwrap_or_default())
                .unwrap_or(CronMode::WpCron),
        },
        JobKind::ImportRun => {
            let source =
                serde_json::from_value(payload.get("source").cloned().unwrap_or_default()).ok()?;
            let resync = payload
                .get("resync")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Operation::ImportSite {
                site_id,
                source,
                resync,
            }
        }
        JobKind::SiteClone | JobKind::StagingCreate | JobKind::StagingPush => {
            let source_site_id = payload
                .get("source_site_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(site_id);
            let source_site = db::sites::get(&state.db, source_site_id)
                .await
                .ok()
                .flatten();
            let target_site_id = payload
                .get("target_site_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(site_id);
            let target_domain = payload
                .get("target_domain")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let staging =
                job.job.kind == JobKind::StagingCreate || job.job.kind == JobKind::StagingPush;
            let request_ssl = payload
                .get("request_ssl")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let php_version = source_site
                .as_ref()
                .map(|s| s.site.php_version)
                .unwrap_or(PhpVersion::Php84);
            let source_domain = source_site.map(|s| s.site.domain).unwrap_or_default();

            Operation::CloneSite(wp_common::protocol::CloneSite {
                source_site_id,
                source_domain,
                target_site_id,
                target_domain,
                php_version,
                staging,
                search_replace: true,
                request_ssl,
            })
        }
        JobKind::WordpressInstall => {
            Operation::InstallWordpress(serde_json::from_value(payload.clone()).unwrap_or_else(
                |_| wp_common::protocol::InstallWordpress {
                    site_id,
                    site_title: "WordPress".into(),
                    admin_user: "admin".into(),
                    admin_email: "admin@example.com".into(),
                    admin_password: "password".into(),
                    locale: "en_US".into(),
                },
            ))
        }
    })
}

/// Applies the panel-side state transition once a job succeeds.
async fn apply_effects(
    state: &AppState,
    job: &JobRow,
    data: Option<&OperationData>,
) -> anyhow::Result<()> {
    let Some(site_id) = job.job.site_id else {
        return Ok(());
    };
    let payload = db::jobs::payload(&state.db, job.job.id)
        .await?
        .unwrap_or_default();

    match job.job.kind {
        JobKind::SiteCreate
        | JobKind::SiteStart
        | JobKind::SiteRestart
        | JobKind::SiteClone
        | JobKind::ImportRun => {
            db::sites::set_status(&state.db, site_id, SiteStatus::Online).await?;
            if job.job.kind == JobKind::SiteCreate {
                db::sites::set_wp_version(&state.db, site_id, "6.7.1").await?;
                if payload
                    .get("request_ssl")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    db::sites::set_ssl(
                        &state.db,
                        site_id,
                        true,
                        SslIssuer::LetsEncrypt,
                        Some(Utc::now() + ChronoDuration::days(90)),
                    )
                    .await?;
                }
            }
        }
        JobKind::WordpressInstall => {
            db::sites::set_wp_version(&state.db, site_id, "6.7.1").await?;
        }
        JobKind::SiteStop => {
            db::sites::set_status(&state.db, site_id, SiteStatus::Stopped).await?;
        }
        JobKind::SiteDelete => {
            db::sites::delete(&state.db, site_id).await?;
        }
        JobKind::PhpSwitch => {
            if let Some(version) = payload
                .get("php_version")
                .and_then(|v| v.as_str())
                .and_then(PhpVersion::parse)
            {
                db::sites::set_php(&state.db, site_id, version).await?;
            }
        }
        JobKind::SslIssue | JobKind::SslRenew => {
            let expires = match data {
                Some(OperationData::Certificate { expires_at, .. }) => *expires_at,
                _ => Utc::now() + ChronoDuration::days(90),
            };
            db::sites::set_ssl(
                &state.db,
                site_id,
                true,
                SslIssuer::LetsEncrypt,
                Some(expires),
            )
            .await?;
        }
        JobKind::BackupCreate => {
            let (snapshot, size) = match data {
                Some(OperationData::Backup {
                    snapshot_id,
                    size_bytes,
                }) => (snapshot_id.clone(), *size_bytes),
                _ => (
                    crate::auth::random_token()[..8].to_lowercase(),
                    1_150_000_000,
                ),
            };
            db::sites::record_backup(&state.db, site_id, &snapshot, BackupScope::Full, size)
                .await?;
        }
        _ => {}
    }

    Ok(())
}

async fn mark_site_failed(state: &AppState, job: &JobRow) -> anyhow::Result<()> {
    if job.job.kind == JobKind::SiteCreate {
        if let Some(site_id) = job.job.site_id {
            db::sites::set_status(&state.db, site_id, SiteStatus::Failed).await?;
        }
    }
    Ok(())
}
