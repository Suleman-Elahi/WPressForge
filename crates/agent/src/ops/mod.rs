//! Operation handlers. `dispatch` is the only entry point the HTTP layer uses.

pub mod backup;
pub mod database;
pub mod docker;
pub mod filesystem;
pub mod metrics;
pub mod nginx;
pub mod site;
pub mod ssl;
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

        Operation::GetServerMetrics => metrics::collect(&state.config, &state.store)
            .await
            .map(|metrics| OperationResult::ok(OperationData::Metrics(metrics))),

        Operation::CreateSite(request) => site::create(state, request).await,
        // Cloning arrives with the staging/cloning milestone; refusing loudly is
        // better than half-copying a production site.
        Operation::CloneSite(_) => Err(wp_common::Error::Unsupported(
            "clone_site is not implemented in this agent version".into(),
        )),
        Operation::DeleteSite { site_id, keep_backups } => {
            site::delete(state, site_id, keep_backups).await
        }
        Operation::StartSite { site_id } => site::start(state, site_id).await,
        Operation::StopSite { site_id } => site::stop(state, site_id).await,
        Operation::RestartSite { site_id } => site::restart(state, site_id).await,
        Operation::GetSiteStatus { site_id } => site::status(state, site_id).await,
        Operation::SwitchPhp { site_id, version } => site::switch_php(state, site_id, version).await,
        Operation::SetLimits { site_id, limits } => site::set_limits(state, site_id, limits).await,

        Operation::AddDomain { site_id, domain } => site::add_domain(state, site_id, domain).await,
        Operation::RemoveDomain { site_id, domain } => {
            site::remove_domain(state, site_id, domain).await
        }

        Operation::IssueCertificate { site_id, domains } => {
            site::issue_certificate(state, site_id, domains).await
        }
        Operation::RenewCertificate { site_id } => site::renew_certificate(state, site_id).await,

        Operation::SetCache { site_id, settings } => site::set_cache(state, site_id, settings).await,
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

        Operation::CreateBackup { site_id, scope } => site::backup(state, site_id, scope).await,
        Operation::RestoreBackup {
            site_id,
            snapshot_id,
            scope,
        } => site::restore(state, site_id, &snapshot_id, scope).await,
        Operation::ListBackups { site_id } => {
            let _ = site_id;
            Ok(OperationResult::ok(OperationData::Backups(Vec::new())))
        }

        Operation::TailLogs {
            site_id,
            stream,
            lines,
        } => site::tail_logs(state, site_id, stream, lines).await,
    };

    match result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(operation = name, %error, "operation failed");
            OperationResult::err(error)
        }
    }
}
