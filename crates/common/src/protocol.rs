//! The panel never runs shell commands on a node. It sends one of the
//! operations below to the agent, which owns the privileged implementation.
//!
//! Wire format: `POST {agent_url}/v1/operations` with a bearer token, body is
//! [`OperationEnvelope`], response is [`OperationResult`].

use crate::models::{
    BackupScope, CacheSettings, CronMode, DatabaseMode, PhpVersion, ResourceLimits, RetentionPolicy,
    ServerMetrics, SiteStatus, WpItemAction,
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
    GetSiteMetrics { site_ids: Vec<i64> },

    // -- site lifecycle -----------------------------------------------------
    CreateSite(CreateSite),
    DeleteSite { site_id: i64, keep_backups: bool },
    StartSite { site_id: i64 },
    StopSite { site_id: i64 },
    RestartSite { site_id: i64 },
    GetSiteStatus { site_id: i64 },
    CloneSite(CloneSite),

    // -- php ----------------------------------------------------------------
    SwitchPhp { site_id: i64, version: PhpVersion },
    SetLimits { site_id: i64, limits: ResourceLimits },

    // -- domains / ssl ------------------------------------------------------
    AddDomain { site_id: i64, domain: String },
    RemoveDomain { site_id: i64, domain: String },
    IssueCertificate { site_id: i64, domains: Vec<String> },
    RenewCertificate { site_id: i64 },

    // -- cache --------------------------------------------------------------
    SetCache { site_id: i64, settings: CacheSettings },
    ClearCache { site_id: i64 },

    // -- wordpress ----------------------------------------------------------
    /// Runs a whitelisted WP-CLI subcommand inside the site container.
    WpCli { site_id: i64, args: Vec<String> },
    InstallWordpress(InstallWordpress),
    UpdateWordpress { site_id: i64 },

    // -- wordpress management (M2) ------------------------------------------
    ListPlugins { site_id: i64 },
    ListThemes { site_id: i64 },
    ListWpUsers { site_id: i64 },
    ListCronEvents { site_id: i64 },
    CoreCheckUpdate { site_id: i64 },
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
    UpdateAllPlugins { site_id: i64 },
    ResetWpPassword { site_id: i64, user_login: String },
    RunCronEvent { site_id: i64, hook: String },
    SetWpCron { site_id: i64, mode: CronMode },
    PurgeUrls { site_id: i64, urls: Vec<String> },

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
    InitBackupRepo { target: ResticTarget },

    // -- logs ---------------------------------------------------------------
    TailLogs {
        site_id: i64,
        stream: LogStream,
        lines: u32,
        grep: Option<String>,
    },
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
    },
    Backups(Vec<crate::models::Backup>),
    CommandOutput {
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    Lines(Vec<String>),
    Plugins(Vec<crate::models::PluginInfo>),
    Themes(Vec<crate::models::ThemeInfo>),
    WpUsers(Vec<crate::models::WpUserInfo>),
    CronEvents(Vec<crate::models::CronEventInfo>),
    CoreUpdate {
        current: String,
        latest: Option<String>,
    },
    GeneratedPassword {
        user_login: String,
        password: String,
    },
    SiteMetrics(Vec<crate::models::SiteMetricSample>),
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
