//! Operation handlers. `dispatch` is the only entry point the HTTP layer uses.

pub mod backup;
pub mod database;
pub mod docker;
pub mod filesystem;
pub mod import;
pub mod metrics;
pub mod mu_plugin;
pub mod nginx;
pub mod site;
pub mod ssl;
pub mod webserver;
pub mod wordpress;

use crate::state::AgentState;
use wp_common::protocol::{Operation, OperationData, OperationResult};

pub async fn dispatch(state: &AgentState, operation: Operation) -> OperationResult {
    let name = operation.name();
    tracing::info!(operation = name, "handling");

    let result = match operation {
        Operation::Ping => Ok(OperationResult::ok(OperationData::Pong {
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: wp_common::PROTOCOL_VERSION,
        })),

        Operation::GetServerMetrics => metrics::collect(&state.config, &state.store, &state.web)
            .await
            .map(|metrics| OperationResult::ok(OperationData::Metrics(metrics))),

        Operation::GetSiteMetrics { site_ids } => site::get_site_metrics(state, site_ids)
            .await
            .map(|samples| OperationResult::ok(OperationData::SiteMetrics(samples))),

        Operation::CreateSite(request) => site::create(state, request).await,
        Operation::CloneSite(request) => site::clone(state, request).await,
        Operation::DeleteSite {
            site_id,
            keep_backups,
        } => site::delete(state, site_id, keep_backups).await,
        Operation::StartSite { site_id } => site::start(state, site_id).await,
        Operation::StopSite { site_id } => site::stop(state, site_id).await,
        Operation::RestartSite { site_id } => site::restart(state, site_id).await,
        Operation::GetSiteStatus { site_id } => site::status(state, site_id).await,
        Operation::SwitchPhp { site_id, version } => {
            site::switch_php(state, site_id, version).await
        }
        Operation::SetLimits { site_id, limits } => site::set_limits(state, site_id, limits).await,

        Operation::AddDomain { site_id, domain } => site::add_domain(state, site_id, domain).await,
        Operation::RemoveDomain { site_id, domain } => {
            site::remove_domain(state, site_id, domain).await
        }

        Operation::IssueCertificate { site_id, domains } => {
            site::issue_certificate(state, site_id, domains).await
        }
        Operation::RenewCertificate { site_id } => site::renew_certificate(state, site_id).await,

        Operation::SetCache { site_id, settings } => {
            site::set_cache(state, site_id, settings).await
        }
        Operation::ClearCache { site_id } => site::clear_cache(state, site_id).await,

        Operation::WpCli { site_id, args } => {
            let record = state.store.get(site_id).await;
            match record {
                Ok(record) => wordpress::passthrough(&state.config, &record, &args)
                    .await
                    .map(|output| {
                        OperationResult::ok(OperationData::CommandOutput {
                            stdout: output.stdout,
                            stderr: output.stderr,
                            exit_code: output.exit_code,
                        })
                    }),
                Err(error) => Err(error),
            }
        }

        Operation::InstallWordpress(request) => site::install_wordpress(state, request).await,
        Operation::UpdateWordpress { site_id } => site::update_wordpress(state, site_id).await,

        // -- wordpress management (M2) --------------------------------------
        Operation::ListPlugins { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::list_plugins(&state.config, &record).await {
                    Ok(list) => OperationResult::ok(OperationData::Plugins(list)),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::ListThemes { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(match wordpress::list_themes(&state.config, &record).await {
                Ok(list) => OperationResult::ok(OperationData::Themes(list)),
                Err(e) => OperationResult::err(e),
            })
        }
        Operation::ListWpUsers { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::list_wp_users(&state.config, &record).await {
                    Ok(list) => OperationResult::ok(OperationData::WpUsers(list)),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::ListCronEvents { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::list_cron_events(&state.config, &record).await {
                    Ok(list) => OperationResult::ok(OperationData::CronEvents(list)),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::CoreCheckUpdate { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::core_check_update(&state.config, &record).await {
                    Ok((current, latest)) => {
                        OperationResult::ok(OperationData::CoreUpdate { current, latest })
                    }
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::PluginAction {
            site_id,
            slug,
            action,
        } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::plugin_action(&state.config, &record, &slug, action).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::ThemeAction {
            site_id,
            slug,
            action,
        } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::theme_action(&state.config, &record, &slug, action).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::UpdateAllPlugins { site_id } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::update_all_plugins(&state.config, &record).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::ResetWpPassword {
            site_id,
            user_login,
        } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::reset_wp_password(&state.config, &record, &user_login).await {
                    Ok(password) => OperationResult::ok(OperationData::GeneratedPassword {
                        user_login,
                        password,
                    }),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::RunCronEvent { site_id, hook } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::run_cron_event(&state.config, &record, &hook).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::SetWpCron { site_id, mode } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::set_wp_cron(&state.config, &record, mode).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }
        Operation::PurgeUrls { site_id, urls } => {
            let record = match state.store.get(site_id).await {
                Ok(r) => r,
                Err(e) => return OperationResult::err(e),
            };
            Ok(
                match wordpress::purge_urls(&state.config, &record, &urls).await {
                    Ok(()) => OperationResult::ok(OperationData::None),
                    Err(e) => OperationResult::err(e),
                },
            )
        }

        Operation::CreateBackup {
            site_id,
            scope,
            target,
            retention: _,
        } => site::backup(state, site_id, scope, target).await,
        Operation::RestoreBackup {
            site_id,
            snapshot_id,
            scope,
            target,
        } => site::restore(state, site_id, &snapshot_id, scope, target).await,
        Operation::ListBackups { site_id, target: _ } => {
            let _ = site_id;
            Ok(OperationResult::ok(OperationData::Backups(Vec::new())))
        }
        Operation::InitBackupRepo { target } => {
            Ok(match backup::init(&state.config, &target).await {
                Ok(()) => OperationResult::ok(OperationData::None),
                Err(e) => OperationResult::err(e),
            })
        }

        Operation::InspectImportSource { source } => import::inspect(&state.config, &source).await,
        Operation::ImportSite {
            site_id,
            source,
            resync,
        } => import::import(state, site_id, &source, resync).await,

        Operation::TailLogs {
            site_id,
            stream,
            lines,
            grep,
        } => site::tail_logs(state, site_id, stream, lines, grep).await,
    };

    match result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(operation = name, %error, "operation failed");
            OperationResult::err(error)
        }
    }
}
