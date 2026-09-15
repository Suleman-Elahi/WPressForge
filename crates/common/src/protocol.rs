//! The panel never runs shell commands on a node. It sends one of the
//! operations below to the agent, which owns the privileged implementation.
//!
//! Wire format: `POST {agent_url}/v1/operations` with a bearer token, body is
//! [`OperationEnvelope`], response is [`OperationResult`].

use crate::models::{
    BackupScope, CacheSettings, CronMode, DatabaseMode, PhpVersion, ResourceLimits,
    RetentionPolicy, ServerMetrics, SiteStatus, WpItemAction,
};
use serde::{Deserialize, Serialize};

/// Restic backup target: repository URL, password, and environment variables
/// (AWS credentials, etc.). Passed per operation so the agent stores no
/// long-lived secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResticTarget {
    pub repo: String,
    pub password: String,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationEnvelope {
    pub protocol_version: u32,
    /// Panel job id, echoed in agent logs so traces line up.
    pub job_id: Option<i64>,
    /// Idempotency key: replaying the same key is a no-op.
    pub request_id: String,
    pub operation: Operation,
}

impl OperationEnvelope {
    pub fn new(operation: Operation) -> Self {
        Self {
            protocol_version: crate::PROTOCOL_VERSION,
            job_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
            operation,
        }
    }

    pub fn with_job(mut self, job_id: i64) -> Self {
        self.job_id = Some(job_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Operation {
    // -- node ---------------------------------------------------------------
    Ping,
    GetServerMetrics,
    GetSiteMetrics {
        site_ids: Vec<i64>,
    },

    // -- site lifecycle -----------------------------------------------------
    CreateSite(CreateSite),
    DeleteSite {
        site_id: i64,
        keep_backups: bool,
    },
    StartSite {
        site_id: i64,
    },
    StopSite {
        site_id: i64,
    },
    RestartSite {
        site_id: i64,
    },
    GetSiteStatus {
        site_id: i64,
    },
    CloneSite(CloneSite),

    // -- php ----------------------------------------------------------------
    SwitchPhp {
        site_id: i64,
        version: PhpVersion,
    },
    SetLimits {
        site_id: i64,
        limits: ResourceLimits,
    },

    // -- domains / ssl ------------------------------------------------------
    AddDomain {
        site_id: i64,
        domain: String,
    },
    RemoveDomain {
        site_id: i64,
        domain: String,
    },
    IssueCertificate {
        site_id: i64,
        domains: Vec<String>,
    },
    RenewCertificate {
        site_id: i64,
    },

    // -- cache --------------------------------------------------------------
    SetCache {
        site_id: i64,
        settings: CacheSettings,
    },
    ClearCache {
        site_id: i64,
    },

    // -- wordpress ----------------------------------------------------------
    /// Runs a whitelisted WP-CLI subcommand inside the site container.
    WpCli {
        site_id: i64,
        args: Vec<String>,
    },
    InstallWordpress(InstallWordpress),
    UpdateWordpress {
        site_id: i64,
    },

    // -- wordpress management (M2) ------------------------------------------
    ListPlugins {
        site_id: i64,
    },
    ListThemes {
        site_id: i64,
    },
    ListWpUsers {
        site_id: i64,
    },
    ListCronEvents {
        site_id: i64,
    },
    CoreCheckUpdate {
        site_id: i64,
    },
    PluginAction {
        site_id: i64,
        slug: String,
        action: WpItemAction,
    },
    ThemeAction {
        site_id: i64,
        slug: String,
        action: WpItemAction,
    },
    UpdateAllPlugins {
        site_id: i64,
    },
    ResetWpPassword {
        site_id: i64,
        user_login: String,
    },
    RunCronEvent {
        site_id: i64,
        hook: String,
    },
    SetWpCron {
        site_id: i64,
        mode: CronMode,
    },
    PurgeUrls {
        site_id: i64,
        urls: Vec<String>,
    },

    // -- backups ------------------------------------------------------------
    CreateBackup {
        site_id: i64,
        scope: BackupScope,
        target: ResticTarget,
        retention: RetentionPolicy,
    },
    RestoreBackup {
        site_id: i64,
        snapshot_id: String,
        scope: BackupScope,
        target: ResticTarget,
    },
    ListBackups {
        site_id: i64,
        target: ResticTarget,
    },
    InitBackupRepo {
        target: ResticTarget,
    },

    // -- logs ---------------------------------------------------------------
    TailLogs {
        site_id: i64,
        stream: LogStream,
        lines: u32,
        grep: Option<String>,
    },

    // -- imports (M6) -------------------------------------------------------
    InspectImportSource {
        source: ImportSource,
    },
    ImportSite(ImportRequest),
}

impl Operation {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::GetServerMetrics => "get_server_metrics",
            Self::GetSiteMetrics { .. } => "get_site_metrics",
            Self::CreateSite(_) => "create_site",
            Self::DeleteSite { .. } => "delete_site",
            Self::StartSite { .. } => "start_site",
            Self::StopSite { .. } => "stop_site",
            Self::RestartSite { .. } => "restart_site",
            Self::GetSiteStatus { .. } => "get_site_status",
            Self::CloneSite(_) => "clone_site",
            Self::SwitchPhp { .. } => "switch_php",
            Self::SetLimits { .. } => "set_limits",
            Self::AddDomain { .. } => "add_domain",
            Self::RemoveDomain { .. } => "remove_domain",
            Self::IssueCertificate { .. } => "issue_certificate",
            Self::RenewCertificate { .. } => "renew_certificate",
            Self::SetCache { .. } => "set_cache",
            Self::ClearCache { .. } => "clear_cache",
            Self::WpCli { .. } => "wp_cli",
            Self::InstallWordpress(_) => "install_wordpress",
            Self::UpdateWordpress { .. } => "update_wordpress",
            Self::ListPlugins { .. } => "list_plugins",
            Self::ListThemes { .. } => "list_themes",
            Self::ListWpUsers { .. } => "list_wp_users",
            Self::ListCronEvents { .. } => "list_cron_events",
            Self::CoreCheckUpdate { .. } => "core_check_update",
            Self::PluginAction { .. } => "plugin_action",
            Self::ThemeAction { .. } => "theme_action",
            Self::UpdateAllPlugins { .. } => "update_all_plugins",
            Self::ResetWpPassword { .. } => "reset_wp_password",
            Self::RunCronEvent { .. } => "run_cron_event",
            Self::SetWpCron { .. } => "set_wp_cron",
            Self::PurgeUrls { .. } => "purge_urls",
            Self::CreateBackup { .. } => "create_backup",
            Self::RestoreBackup { .. } => "restore_backup",
            Self::ListBackups { .. } => "list_backups",
            Self::InitBackupRepo { .. } => "init_backup_repo",
            Self::TailLogs { .. } => "tail_logs",
            Self::InspectImportSource { .. } => "inspect_import_source",
            Self::ImportSite(_) => "import_site",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSite {
    pub site_id: i64,
    pub domain: String,
    pub php_version: PhpVersion,
    pub database_mode: DatabaseMode,
    pub limits: ResourceLimits,
    pub cache: CacheSettings,
    pub install_wordpress: bool,
    pub request_ssl: bool,
    pub wordpress: Option<InstallWordpress>,
}

/// Everything the agent needs to take over an existing WordPress install.
///
/// Carries the site definition as well as the source, because an import creates
/// a site the agent has never seen: looking it up in the local store first is
/// what made every import fail with "site N is not managed by this agent".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub site_id: i64,
    pub domain: String,
    pub php_version: PhpVersion,
    pub database_mode: DatabaseMode,
    pub limits: ResourceLimits,
    pub cache: CacheSettings,
    pub source: ImportSource,
    /// Re-run against a site the agent already manages, keeping its database
    /// and `wp-config.php` in place.
    pub resync: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallWordpress {
    pub site_id: i64,
    pub site_title: String,
    pub admin_user: String,
    pub admin_email: String,
    /// Generated by the panel; the agent never logs it.
    pub admin_password: String,
    pub locale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneSite {
    pub source_site_id: i64,
    pub source_domain: String,
    pub target_site_id: i64,
    pub target_domain: String,
    pub php_version: PhpVersion,
    /// When true, clone is treated as staging (blog_public=0, WP_ENVIRONMENT_TYPE=staging).
    pub staging: bool,
    pub search_replace: bool,
    pub request_ssl: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSource {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub remote_path: String,
    pub db: Option<RemoteDb>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    Password(String),
    PrivateKey(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDb {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    NginxAccess,
    NginxError,
    PhpError,
    PhpSlow,
    WpDebug,
    Agent,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NginxAccess => "nginx-access",
            Self::NginxError => "nginx-error",
            Self::PhpError => "php-error",
            Self::PhpSlow => "php-slow",
            Self::WpDebug => "wp-debug",
            Self::Agent => "agent",
        }
    }
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::Error>,
    #[serde(default)]
    pub data: OperationData,
    /// Ordered progress log the panel streams into the job view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepReport>,
}

impl OperationResult {
    pub fn ok(data: OperationData) -> Self {
        Self {
            success: true,
            error: None,
            data,
            steps: Vec::new(),
        }
    }

    pub fn err(error: crate::Error) -> Self {
        Self {
            success: false,
            error: Some(error),
            data: OperationData::None,
            steps: Vec::new(),
        }
    }

    pub fn with_steps(mut self, steps: Vec<StepReport>) -> Self {
        self.steps = steps;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// NOTE: internally tagged enums (`tag = "type"`) cannot serialise a newtype
// variant that wraps a sequence — serde fails at runtime with "cannot serialize
// tagged newtype variant ... containing a sequence". Every list-carrying variant
// therefore uses a named field.
pub enum OperationData {
    #[default]
    None,
    Pong {
        agent_version: String,
        protocol_version: u32,
    },
    Metrics(ServerMetrics),
    SiteCreated {
        site_id: i64,
        container_id: String,
        uid: u32,
        db_name: String,
    },
    SiteStatus {
        site_id: i64,
        status: SiteStatus,
        php_version: PhpVersion,
        wp_version: Option<String>,
        disk_usage_mb: u64,
    },
    Certificate {
        domains: Vec<String>,
        expires_at: chrono::DateTime<chrono::Utc>,
    },
    Backup {
        snapshot_id: String,
        size_bytes: u64,
        /// Bytes attributable to site files, and to the database dump. The panel
        /// stores both so the backups table can explain what a snapshot holds.
        #[serde(default)]
        files_bytes: u64,
        #[serde(default)]
        db_bytes: u64,
    },
    Backups {
        backups: Vec<crate::models::Backup>,
    },
    CommandOutput {
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    Lines {
        lines: Vec<String>,
    },
    Plugins {
        plugins: Vec<crate::models::PluginInfo>,
    },
    Themes {
        themes: Vec<crate::models::ThemeInfo>,
    },
    WpUsers {
        users: Vec<crate::models::WpUserInfo>,
    },
    CronEvents {
        events: Vec<crate::models::CronEventInfo>,
    },
    CoreUpdate {
        current: String,
        latest: Option<String>,
    },
    GeneratedPassword {
        user_login: String,
        password: String,
    },
    SiteMetrics {
        samples: Vec<crate::models::SiteMetricSample>,
    },
    ImportInspection {
        wp_version: String,
        php_version: String,
        size_mb: u64,
        db_name: Option<String>,
        db_user: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub name: String,
    pub ok: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl StepReport {
    pub fn ok(name: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            name: name.into(),
            ok: true,
            duration_ms,
            detail: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        Backup, BackupScope, CacheSettings, CronEventInfo, DatabaseMode, PluginInfo,
        ResourceLimits, ServerMetrics, SiteMetricSample, SiteStatus, ThemeInfo, WpUserInfo,
    };

    /// Every `OperationData` variant must survive a JSON round trip.
    ///
    /// This exists because `#[serde(tag = "type")]` cannot serialise a newtype
    /// variant wrapping a sequence: it compiles, then fails at runtime with
    /// "cannot serialize tagged newtype variant ... containing a sequence".
    /// Several list-returning operations shipped broken for exactly that reason.
    #[test]
    fn every_operation_data_variant_round_trips() {
        let now = chrono::Utc::now();

        let variants = vec![
            OperationData::None,
            OperationData::Pong {
                agent_version: "0.1.0".into(),
                protocol_version: crate::PROTOCOL_VERSION,
            },
            OperationData::Metrics(ServerMetrics::default()),
            OperationData::SiteCreated {
                site_id: 1,
                container_id: "abc".into(),
                uid: 10001,
                db_name: "wp_example".into(),
            },
            OperationData::SiteStatus {
                site_id: 1,
                status: SiteStatus::Online,
                php_version: crate::models::PhpVersion::Php84,
                wp_version: Some("6.7.1".into()),
                disk_usage_mb: 12,
            },
            OperationData::Certificate {
                domains: vec!["example.com".into()],
                expires_at: now,
            },
            OperationData::Backup {
                snapshot_id: "deadbeef".into(),
                size_bytes: 42,
                files_bytes: 30,
                db_bytes: 12,
            },
            OperationData::Backups {
                backups: vec![Backup {
                    id: 1,
                    site_id: 1,
                    snapshot_id: "deadbeef".into(),
                    size_bytes: 42,
                    scope: BackupScope::Full,
                    destination: "s3".into(),
                    created_at: now,
                }],
            },
            OperationData::CommandOutput {
                stdout: "ok".into(),
                stderr: String::new(),
                exit_code: 0,
            },
            OperationData::Lines {
                lines: vec!["line one".into(), "line two".into()],
            },
            OperationData::Plugins {
                plugins: vec![PluginInfo {
                    name: "Akismet".into(),
                    slug: "akismet".into(),
                    status: "active".into(),
                    version: "5.3".into(),
                    update_version: None,
                    auto_update: false,
                }],
            },
            OperationData::Themes {
                themes: vec![ThemeInfo {
                    name: "Twenty Twenty-Five".into(),
                    slug: "twentytwentyfive".into(),
                    status: "active".into(),
                    version: "1.0".into(),
                    update_version: None,
                }],
            },
            OperationData::WpUsers {
                users: vec![WpUserInfo {
                    id: 1,
                    login: "admin".into(),
                    email: "admin@example.com".into(),
                    role: "administrator".into(),
                }],
            },
            OperationData::CronEvents {
                events: vec![CronEventInfo {
                    hook: "wp_version_check".into(),
                    next_run_relative: "in 4 hours".into(),
                    schedule: "twicedaily".into(),
                }],
            },
            OperationData::CoreUpdate {
                current: "6.7.1".into(),
                latest: Some("6.8".into()),
            },
            OperationData::GeneratedPassword {
                user_login: "admin".into(),
                password: "secret".into(),
            },
            OperationData::SiteMetrics {
                samples: vec![SiteMetricSample {
                    site_id: 1,
                    cpu_percent: 1.5,
                    memory_mb: 128,
                    php_busy_workers: 2,
                    cache_hit_ratio: Some(0.9),
                }],
            },
            OperationData::ImportInspection {
                wp_version: "6.7.1".into(),
                php_version: "8.3".into(),
                size_mb: 100,
                db_name: Some("wp".into()),
                db_user: Some("wp".into()),
            },
        ];

        for variant in variants {
            let json = serde_json::to_string(&variant)
                .unwrap_or_else(|e| panic!("serialising {variant:?} failed: {e}"));
            let back: OperationData = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("deserialising {json} failed: {e}"));
            assert_eq!(
                std::mem::discriminant(&variant),
                std::mem::discriminant(&back),
                "variant changed across the round trip: {json}"
            );
        }
    }

    /// The same guarantee for the request side.
    #[test]
    fn representative_operations_round_trip() {
        let operations = vec![
            Operation::Ping,
            Operation::GetServerMetrics,
            Operation::CreateSite(CreateSite {
                site_id: 1,
                domain: "example.com".into(),
                php_version: crate::models::PhpVersion::Php84,
                database_mode: DatabaseMode::Shared,
                limits: ResourceLimits::default(),
                cache: CacheSettings::default(),
                install_wordpress: true,
                request_ssl: true,
                wordpress: None,
            }),
            Operation::TailLogs {
                site_id: 1,
                stream: LogStream::NginxError,
                lines: 100,
                grep: Some("php".into()),
            },
            Operation::WpCli {
                site_id: 1,
                args: vec!["plugin".into(), "list".into()],
            },
        ];

        for operation in operations {
            let json = serde_json::to_string(&operation).expect("serialise operation");
            let back: Operation = serde_json::from_str(&json).expect("deserialise operation");
            assert_eq!(operation.name(), back.name(), "operation changed: {json}");
        }
    }
}
