//! Domain model shared by panel and agent.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Servers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: i64,
    pub name: String,
    /// Base URL of the agent, e.g. `https://10.0.0.4:8443`.
    pub agent_url: String,
    pub hostname: String,
    pub ip_address: String,
    pub provider: Option<String>,
    pub region: Option<String>,
    pub status: ServerStatus,
    pub agent_version: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Provisioning,
    Online,
    Degraded,
    Offline,
}

impl ServerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Online => "online",
            Self::Degraded => "degraded",
            Self::Offline => "offline",
        }
    }

    /// CSS modifier used by the status dot in the UI.
    pub fn tone(&self) -> &'static str {
        match self {
            Self::Online => "ok",
            Self::Provisioning => "info",
            Self::Degraded => "warn",
            Self::Offline => "bad",
        }
    }
}

impl fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Live metrics reported by an agent heartbeat.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerMetrics {
    pub cpu_percent: f32,
    pub memory_percent: f32,
    pub memory_total_mb: u64,
    pub disk_percent: f32,
    pub disk_total_gb: u64,
    pub load_1m: f32,
    pub sites: u32,
    pub containers: u32,
    pub services: Vec<ServiceHealth>,
    pub php_versions: Vec<PhpUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub name: String,
    pub healthy: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhpUsage {
    pub version: PhpVersion,
    pub sites: u32,
}

// ---------------------------------------------------------------------------
// Sites
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: i64,
    pub server_id: i64,
    /// Primary domain, also the on-disk directory name.
    pub domain: String,
    pub title: Option<String>,
    pub status: SiteStatus,
    pub php_version: PhpVersion,
    pub wp_version: Option<String>,
    pub database_mode: DatabaseMode,
    pub limits: ResourceLimits,
    pub cache: CacheSettings,
    pub ssl: SslState,
    pub environment: Environment,
    /// Dedicated Linux UID/GID owning `/var/www/<domain>`.
    pub uid: u32,
    pub disk_usage_mb: u64,
    pub created_at: DateTime<Utc>,
}

impl Site {
    pub fn root_path(&self) -> String {
        format!("/var/www/{}", self.domain)
    }

    pub fn container_name(&self) -> String {
        format!("wp-{}", self.domain.replace('.', "-"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteStatus {
    Provisioning,
    Online,
    Stopped,
    Failed,
    Suspended,
}

impl SiteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Online => "online",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Suspended => "suspended",
        }
    }

    pub fn tone(&self) -> &'static str {
        match self {
            Self::Online => "ok",
            Self::Provisioning => "info",
            Self::Stopped | Self::Suspended => "muted",
            Self::Failed => "bad",
        }
    }
}

impl fmt::Display for SiteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Production,
    Staging,
}

impl Environment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Staging => "staging",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhpVersion {
    #[serde(rename = "8.1")]
    Php81,
    #[serde(rename = "8.2")]
    Php82,
    #[serde(rename = "8.3")]
    Php83,
    #[serde(rename = "8.4")]
    Php84,
}

impl PhpVersion {
    pub const ALL: [PhpVersion; 4] = [Self::Php81, Self::Php82, Self::Php83, Self::Php84];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Php81 => "8.1",
            Self::Php82 => "8.2",
            Self::Php83 => "8.3",
            Self::Php84 => "8.4",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Upstream image the agent pulls for this version.
    pub fn image(&self) -> String {
        format!("wp-panel/php-fpm:{}", self.as_str())
    }
}

impl fmt::Display for PhpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseMode {
    /// Shared MariaDB on the host, one schema + user per site.
    Shared,
    /// Dedicated MariaDB container per site.
    Dedicated,
}

impl DatabaseMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Dedicated => "dedicated",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Shared => "Shared server",
            Self::Dedicated => "Dedicated container",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_cores: f32,
    pub memory_mb: u32,
    pub php_workers: u16,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_cores: 1.0,
            memory_mb: 1024,
            php_workers: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CacheSettings {
    pub fastcgi_cache: bool,
    /// Cache lifetime in seconds.
    pub ttl_seconds: u32,
    pub redis_object_cache: bool,
    pub opcache: bool,
    pub brotli: bool,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            fastcgi_cache: true,
            ttl_seconds: 3600,
            redis_object_cache: false,
            opcache: true,
            brotli: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SslState {
    pub enabled: bool,
    pub issuer: SslIssuer,
    pub expires_at: Option<DateTime<Utc>>,
    pub auto_renew: bool,
}

impl Default for SslState {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: SslIssuer::LetsEncrypt,
            expires_at: None,
            auto_renew: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SslIssuer {
    /// Wire value matches the string stored in the panel database.
    #[serde(rename = "letsencrypt")]
    LetsEncrypt,
    Custom,
    None,
}

impl SslIssuer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LetsEncrypt => "letsencrypt",
            Self::Custom => "custom",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    pub id: i64,
    pub site_id: i64,
    pub name: String,
    pub primary: bool,
    pub redirect_to_primary: bool,
    pub dns_ok: bool,
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub server_id: Option<i64>,
    pub site_id: Option<i64>,
    pub kind: JobKind,
    pub status: JobStatus,
    /// 0-100.
    pub progress: u8,
    pub message: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn tone(&self) -> &'static str {
        match self {
            Self::Queued => "muted",
            Self::Running => "info",
            Self::Succeeded => "ok",
            Self::Failed => "bad",
            Self::Cancelled => "warn",
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every long-running action is a job. The string form is what is persisted
/// and displayed (`site.create`, `backup.restore`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[serde(rename = "site.create")]
    SiteCreate,
    #[serde(rename = "site.delete")]
    SiteDelete,
    #[serde(rename = "site.clone")]
    SiteClone,
    #[serde(rename = "site.start")]
    SiteStart,
    #[serde(rename = "site.stop")]
    SiteStop,
    #[serde(rename = "site.restart")]
    SiteRestart,
    #[serde(rename = "php.switch")]
    PhpSwitch,
    #[serde(rename = "wordpress.install")]
    WordpressInstall,
    #[serde(rename = "wordpress.update")]
    WordpressUpdate,
    #[serde(rename = "backup.create")]
    BackupCreate,
    #[serde(rename = "backup.restore")]
    BackupRestore,
    #[serde(rename = "ssl.issue")]
    SslIssue,
    #[serde(rename = "ssl.renew")]
    SslRenew,
    #[serde(rename = "cache.clear")]
    CacheClear,
    #[serde(rename = "staging.create")]
    StagingCreate,
    #[serde(rename = "staging.push")]
    StagingPush,
}

impl JobKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SiteCreate => "site.create",
            Self::SiteDelete => "site.delete",
            Self::SiteClone => "site.clone",
            Self::SiteStart => "site.start",
            Self::SiteStop => "site.stop",
            Self::SiteRestart => "site.restart",
            Self::PhpSwitch => "php.switch",
            Self::WordpressInstall => "wordpress.install",
            Self::WordpressUpdate => "wordpress.update",
            Self::BackupCreate => "backup.create",
            Self::BackupRestore => "backup.restore",
            Self::SslIssue => "ssl.issue",
            Self::SslRenew => "ssl.renew",
            Self::CacheClear => "cache.clear",
            Self::StagingCreate => "staging.create",
            Self::StagingPush => "staging.push",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        const ALL: [JobKind; 16] = [
            JobKind::SiteCreate,
            JobKind::SiteDelete,
            JobKind::SiteClone,
            JobKind::SiteStart,
            JobKind::SiteStop,
            JobKind::SiteRestart,
            JobKind::PhpSwitch,
            JobKind::WordpressInstall,
            JobKind::WordpressUpdate,
            JobKind::BackupCreate,
            JobKind::BackupRestore,
            JobKind::SslIssue,
            JobKind::SslRenew,
            JobKind::CacheClear,
            JobKind::StagingCreate,
            JobKind::StagingPush,
        ];
        ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Human readable label used in the jobs list.
    pub fn label(&self) -> &'static str {
        match self {
            Self::SiteCreate => "Create site",
            Self::SiteDelete => "Delete site",
            Self::SiteClone => "Clone site",
            Self::SiteStart => "Start site",
            Self::SiteStop => "Stop site",
            Self::SiteRestart => "Restart site",
            Self::PhpSwitch => "Switch PHP version",
            Self::WordpressInstall => "Install WordPress",
            Self::WordpressUpdate => "Update WordPress",
            Self::BackupCreate => "Create backup",
            Self::BackupRestore => "Restore backup",
            Self::SslIssue => "Issue certificate",
            Self::SslRenew => "Renew certificate",
            Self::CacheClear => "Clear cache",
            Self::StagingCreate => "Create staging",
            Self::StagingPush => "Push staging to production",
        }
    }
}

/// A persisted job step, as shown in the job timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepView {
    pub name: String,
    pub ok: bool,
    pub duration_ms: u64,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl StepView {
    pub fn duration_label(&self) -> String {
        if self.duration_ms < 1000 {
            format!("{} ms", self.duration_ms)
        } else {
            format!("{:.1} s", self.duration_ms as f64 / 1000.0)
        }
    }
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub id: i64,
    pub site_id: i64,
    /// Restic snapshot id.
    pub snapshot_id: String,
    pub size_bytes: u64,
    pub scope: BackupScope,
    pub destination: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupScope {
    Full,
    FilesOnly,
    DatabaseOnly,
}

impl BackupScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::FilesOnly => "files",
            Self::DatabaseOnly => "database",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub hourly: u16,
    pub daily: u16,
    pub weekly: u16,
    pub monthly: u16,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            hourly: 24,
            daily: 14,
            weekly: 8,
            monthly: 12,
        }
    }
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: Option<String>,
    pub success: bool,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Template helpers
//
// Templates call methods instead of format filters: the formatting rules stay
// in Rust where they can be tested, and the markup stays readable.
// ---------------------------------------------------------------------------

impl Server {
    pub fn last_seen_label(&self) -> String {
        match self.last_seen_at {
            Some(when) => crate::fmt::relative(when),
            None => "never".to_string(),
        }
    }

    pub fn location_label(&self) -> String {
        match (&self.provider, &self.region) {
            (Some(provider), Some(region)) => format!("{provider} · {region}"),
            (Some(provider), None) => provider.clone(),
            (None, Some(region)) => region.clone(),
            (None, None) => "—".to_string(),
        }
    }

    pub fn created_label(&self) -> String {
        crate::fmt::relative(self.created_at)
    }
}

impl ServerMetrics {
    pub fn cpu_label(&self) -> String {
        crate::fmt::percent(self.cpu_percent)
    }
    pub fn cpu_tone(&self) -> &'static str {
        crate::fmt::utilisation_tone(self.cpu_percent)
    }
    pub fn memory_label(&self) -> String {
        crate::fmt::percent(self.memory_percent)
    }
    pub fn memory_tone(&self) -> &'static str {
        crate::fmt::utilisation_tone(self.memory_percent)
    }
    pub fn memory_total_label(&self) -> String {
        crate::fmt::megabytes(self.memory_total_mb)
    }
    pub fn disk_label(&self) -> String {
        crate::fmt::percent(self.disk_percent)
    }
    pub fn disk_tone(&self) -> &'static str {
        crate::fmt::utilisation_tone(self.disk_percent)
    }
    pub fn disk_total_label(&self) -> String {
        format!("{} GB", self.disk_total_gb)
    }
    pub fn load_label(&self) -> String {
        format!("{:.2}", self.load_1m)
    }
    pub fn has_data(&self) -> bool {
        self.memory_total_mb > 0 || self.sites > 0 || !self.services.is_empty()
    }
}

impl Site {
    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.domain.clone())
    }

    pub fn url(&self) -> String {
        let scheme = if self.ssl.enabled { "https" } else { "http" };
        format!("{scheme}://{}", self.domain)
    }

    pub fn wp_label(&self) -> String {
        self.wp_version.clone().unwrap_or_else(|| "not installed".into())
    }

    pub fn memory_label(&self) -> String {
        crate::fmt::megabytes(self.limits.memory_mb as u64)
    }

    pub fn cpu_label(&self) -> String {
        if self.limits.cpu_cores.fract() == 0.0 {
            format!("{:.0} vCPU", self.limits.cpu_cores)
        } else {
            format!("{:.2} vCPU", self.limits.cpu_cores)
        }
    }

    pub fn disk_label(&self) -> String {
        crate::fmt::megabytes(self.disk_usage_mb)
    }

    pub fn ssl_label(&self) -> String {
        match (self.ssl.enabled, self.ssl.expires_at) {
            (true, Some(expires)) => format!("valid, {} left", crate::fmt::until(expires)),
            (true, None) => "valid".to_string(),
            (false, _) => "not issued".to_string(),
        }
    }

    pub fn ssl_tone(&self) -> &'static str {
        match (self.ssl.enabled, self.ssl.expires_at) {
            (true, Some(expires)) => {
                let days = expires.signed_duration_since(Utc::now()).num_days();
                if days < 0 {
                    "bad"
                } else if days < 14 {
                    "warn"
                } else {
                    "ok"
                }
            }
            (true, None) => "ok",
            (false, _) => "muted",
        }
    }

    pub fn cache_label(&self) -> &'static str {
        if self.cache.fastcgi_cache {
            "FastCGI on"
        } else {
            "off"
        }
    }

    pub fn cache_ttl_label(&self) -> String {
        crate::fmt::duration_secs(self.cache.ttl_seconds as u64)
    }

    pub fn created_label(&self) -> String {
        crate::fmt::relative(self.created_at)
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, SiteStatus::Online | SiteStatus::Provisioning)
    }

    pub fn is_staging(&self) -> bool {
        self.environment == Environment::Staging
    }

    /// Database name the agent derives from the domain.
    pub fn db_name(&self) -> String {
        let base: String = self
            .domain
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("wp_{}", base.trim_matches('_').to_lowercase())
    }
}

impl Job {
    pub fn created_label(&self) -> String {
        crate::fmt::relative(self.created_at)
    }

    pub fn finished_label(&self) -> String {
        match self.finished_at {
            Some(when) => crate::fmt::relative(when),
            None => "—".to_string(),
        }
    }

    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }

    pub fn progress_percent(&self) -> u8 {
        match self.status {
            JobStatus::Succeeded => 100,
            JobStatus::Failed | JobStatus::Cancelled => self.progress.max(4),
            _ => self.progress.max(2),
        }
    }
}

impl Backup {
    pub fn size_label(&self) -> String {
        crate::fmt::bytes(self.size_bytes)
    }

    pub fn created_label(&self) -> String {
        crate::fmt::relative(self.created_at)
    }

    pub fn timestamp_label(&self) -> String {
        crate::fmt::timestamp(self.created_at)
    }

    pub fn scope_label(&self) -> &'static str {
        match self.scope {
            BackupScope::Full => "files + database",
            BackupScope::FilesOnly => "files only",
            BackupScope::DatabaseOnly => "database only",
        }
    }
}

impl AuditEntry {
    pub fn created_label(&self) -> String {
        crate::fmt::timestamp(self.created_at)
    }
}

impl Domain {
    pub fn dns_label(&self) -> &'static str {
        if self.dns_ok {
            "resolves"
        } else {
            "not verified"
        }
    }

    pub fn dns_tone(&self) -> &'static str {
        if self.dns_ok {
            "ok"
        } else {
            "warn"
        }
    }
}
