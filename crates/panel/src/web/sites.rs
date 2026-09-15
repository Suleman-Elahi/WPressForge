use super::{Chrome, redirect_with_flash, render};
use crate::auth::{CurrentSession, CurrentUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use wp_common::models::{
    Backup, CacheSettings, CronEventInfo, DatabaseMode, Domain, Environment, JobKind, PhpVersion,
    PluginInfo, ResourceLimits, ThemeInfo, WpItemAction, WpUserInfo,
};

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub flash: Option<String>,
    /// Free-text filter on domain.
    pub q: Option<String>,
    pub status: Option<String>,
}

#[derive(Template)]
#[template(path = "sites/list.html")]
struct ListTemplate {
    chrome: Chrome,
    sites: Vec<db::sites::SiteRow>,
    query: String,
    status_filter: String,
    total: usize,
}

fn require_operator_or_above(user: &CurrentUser) -> AppResult<()> {
    if user.0.role == "viewer" {
        Err(AppError::Forbidden(
            "Read-only viewer cannot perform actions".into(),
        ))
    } else {
        Ok(())
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<ListQuery>,
) -> AppResult<Response> {
    let all = db::sites::list_for_user(&state.db, user.0.id, &user.0.role).await?;
    let total = all.len();
    let needle = query.q.clone().unwrap_or_default().to_lowercase();
    let status_filter = query.status.clone().unwrap_or_default();

    let sites: Vec<_> = all
        .into_iter()
        .filter(|row| {
            let matches_text = needle.is_empty()
                || row.site.domain.to_lowercase().contains(&needle)
                || row
                    .site
                    .title
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(&needle));
            let matches_status =
                status_filter.is_empty() || row.site.status.as_str() == status_filter;
            matches_text && matches_status
        })
        .collect();

    Ok(render(ListTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "Sites", query.flash).await,
        sites,
        query: needle,
        status_filter,
        total,
    }))
}

// ---------------------------------------------------------------------------
// Detail (tabbed)
// ---------------------------------------------------------------------------

pub const TABS: [(&str, &str); 12] = [
    ("overview", "Overview"),
    ("domains", "Domains"),
    ("wordpress", "WordPress"),
    ("php", "PHP"),
    ("database", "Database"),
    ("ssl", "SSL"),
    ("backups", "Backups"),
    ("staging", "Staging"),
    ("logs", "Logs"),
    ("settings", "Settings"),
    ("cron", "Cron"),
    ("console", "Console"),
];

#[derive(Debug, Default, Deserialize)]
pub struct DetailQuery {
    pub flash: Option<String>,
    pub tab: Option<String>,
}

#[derive(Template)]
#[template(path = "sites/detail.html")]
struct DetailTemplate {
    chrome: Chrome,
    site: db::sites::SiteRow,
    tab: String,
    tabs: &'static [(&'static str, &'static str)],
    domains: Vec<Domain>,
    backups: Vec<Backup>,
    jobs: Vec<db::jobs::JobRow>,
    php_versions: [PhpVersion; 4],
}

pub async fn detail(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
    Query(query): Query<DetailQuery>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let tab = query
        .tab
        .filter(|t| TABS.iter().any(|(key, _)| key == t))
        .unwrap_or_else(|| "overview".to_string());

    let template = DetailTemplate {
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "sites",
            site.site.domain.clone(),
            query.flash,
        )
        .await,
        domains: db::sites::domains(&state.db, id).await?,
        backups: db::sites::backups(&state.db, id, 12).await?,
        jobs: db::jobs::list_for_site(&state.db, id, 8).await?,
        php_versions: PhpVersion::ALL,
        site,
        tab,
        tabs: &TABS,
    };

    Ok(render(template))
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/new.html")]
struct NewTemplate {
    chrome: Chrome,
    servers: Vec<db::servers::ServerRow>,
    php_versions: [PhpVersion; 4],
}

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<super::FlashQuery>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let servers = db::servers::list(&state.db).await?;
    Ok(render(NewTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "New site", query.flash).await,
        servers,
        php_versions: PhpVersion::ALL,
    }))
}

#[derive(Debug, Deserialize)]
pub struct NewSiteForm {
    pub server_id: i64,
    pub domain: String,
    #[serde(default)]
    pub title: String,
    pub php_version: String,
    #[serde(default = "default_cpu")]
    pub cpu_cores: f32,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default = "default_workers")]
    pub php_workers: u16,
    #[serde(default)]
    pub database_mode: String,
    #[serde(default)]
    pub install_wordpress: Option<String>,
    #[serde(default)]
    pub request_ssl: Option<String>,
    #[serde(default)]
    pub fastcgi_cache: Option<String>,
}

fn default_cpu() -> f32 {
    1.0
}
fn default_memory() -> u32 {
    1024
}
fn default_workers() -> u16 {
    8
}

fn checked(value: &Option<String>) -> bool {
    value.is_some()
}

/// Very light domain validation: the agent re-validates before touching Nginx.
fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain.contains('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !domain.starts_with('-')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<NewSiteForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let domain = form.domain.trim().trim_start_matches("www.").to_lowercase();
    if !valid_domain(&domain) {
        return Err(AppError::BadRequest(format!(
            "`{domain}` is not a valid domain"
        )));
    }
    if db::sites::domain_exists(&state.db, &domain).await? {
        return Err(AppError::BadRequest(format!("{domain} already exists")));
    }
    if db::servers::get(&state.db, form.server_id).await?.is_none() {
        return Err(AppError::BadRequest("unknown server".into()));
    }

    let php_version = PhpVersion::parse(&form.php_version)
        .ok_or_else(|| AppError::BadRequest("unsupported PHP version".into()))?;
    let database_mode = match form.database_mode.as_str() {
        "dedicated" => DatabaseMode::Dedicated,
        _ => DatabaseMode::Shared,
    };
    let limits = ResourceLimits {
        cpu_cores: form.cpu_cores.clamp(0.25, 32.0),
        memory_mb: form.memory_mb.clamp(256, 65_536),
        php_workers: form.php_workers.clamp(2, 128),
    };
    let cache = CacheSettings {
        fastcgi_cache: checked(&form.fastcgi_cache),
        ..CacheSettings::default()
    };
    let request_ssl = checked(&form.request_ssl);

    let site_id = db::sites::create(
        &state.db,
        db::sites::NewSite {
            server_id: form.server_id,
            domain: &domain,
            title: Some(form.title.trim()).filter(|t| !t.is_empty()),
            php_version,
            database_mode,
            environment: Environment::Production,
            parent_site_id: None,
            limits,
            cache,
            request_ssl,
        },
    )
    .await?;

    if !db::teams::has_global_access(&user.0.role) {
        let _ = db::teams::grant_site_access(&state.db, site_id, user.0.id, "operator").await;
    }

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::SiteCreate,
        Some(form.server_id),
        Some(site_id),
        Some(json!({
            "domain": domain,
            "php_version": php_version.as_str(),
            "database_mode": database_mode.as_str(),
            "limits": limits,
            "cache": cache,
            "install_wordpress": checked(&form.install_wordpress),
            "request_ssl": request_ssl,
        })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "site.create",
        &domain,
        Some(&format!("php {php_version}, job {job_id}")),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        &format!("Creating {domain}"),
    ))
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// One endpoint for the simple, payload-free site actions.
pub async fn action(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, action)): Path<(i64, String)>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let (kind, flash) = match action.as_str() {
        "start" => (JobKind::SiteStart, "Starting site"),
        "stop" => (JobKind::SiteStop, "Stopping site"),
        "restart" => (JobKind::SiteRestart, "Restarting site"),
        "backup" => (JobKind::BackupCreate, "Backup queued"),
        "clear-cache" => (JobKind::CacheClear, "Clearing cache"),
        "issue-ssl" => (JobKind::SslIssue, "Requesting certificate"),
        "renew-ssl" => (JobKind::SslRenew, "Renewing certificate"),
        "update-wordpress" => (JobKind::WordpressUpdate, "Updating WordPress"),
        "delete" => (JobKind::SiteDelete, "Deleting site"),
        other => {
            return Err(AppError::BadRequest(format!("unknown action `{other}`")));
        }
    };

    let payload = match kind {
        JobKind::SslIssue => Some(json!({ "domains": [site.site.domain] })),
        JobKind::SiteDelete => Some(json!({ "keep_backups": true })),
        _ => None,
    };

    let job_id = db::jobs::enqueue(
        &state.db,
        kind,
        Some(site.site.server_id),
        Some(id),
        payload,
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        kind.as_str(),
        &site.site.domain,
        Some(&format!("job {job_id}")),
        true,
    )
    .await?;

    let target = if kind == JobKind::SiteDelete {
        "/sites".to_string()
    } else {
        format!("/sites/{id}")
    };
    Ok(redirect_with_flash(&target, flash))
}

#[derive(Deserialize)]
pub struct RestoreForm {
    pub scope: String,
}

/// Restore a backup snapshot with the given scope.
pub async fn restore_backup(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, snapshot_id)): Path<(i64, String)>,
    Form(form): Form<RestoreForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let scope: wp_common::models::BackupScope = match form.scope.as_str() {
        "full" => wp_common::models::BackupScope::Full,
        "files_only" => wp_common::models::BackupScope::FilesOnly,
        "database_only" => wp_common::models::BackupScope::DatabaseOnly,
        _ => wp_common::models::BackupScope::Full,
    };

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::BackupRestore,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "snapshot_id": snapshot_id, "scope": scope })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "backup_restore",
        &site.site.domain,
        Some(&format!("job {job_id} snapshot {snapshot_id}")),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        "Restore queued",
    ))
}

/// Sync backups from the agent's restic repository.
pub async fn sync_backups(
    State(state): State<AppState>,
    user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Without a destination there is no repository to read, so say so instead of
    // reporting a successful sync (which is what an earlier stub did).
    let Some((destination, target)) =
        db::destinations::for_site(&state.db, &state.secrets, id).await?
    else {
        return Ok(redirect_with_flash(
            &format!("/sites/{id}?tab=backups"),
            "No backup destination is configured for this site.",
        ));
    };

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::ListBackups {
                site_id: id,
                target,
            },
        )
        .await;

    let flash = match result {
        Ok(wp_common::protocol::OperationData::Backups { backups }) => {
            let found = backups.len();
            for backup in backups {
                db::sites::upsert_backup_at(
                    &state.db,
                    id,
                    &db::sites::NewBackup {
                        snapshot_id: backup.snapshot_id,
                        scope: backup.scope,
                        size_bytes: backup.size_bytes,
                        files_bytes: 0,
                        db_bytes: 0,
                        restic_repo: Some(destination.name.clone()),
                    },
                    backup.created_at,
                )
                .await?;
            }

            db::audit::record(
                &state.db,
                &user.0.email,
                "backup.sync",
                &site.site.domain,
                Some(&format!("{found} snapshots from {}", destination.name)),
                true,
            )
            .await?;

            match found {
                0 => "No snapshots found in the repository for this site.".to_string(),
                1 => "Synced 1 snapshot from the node.".to_string(),
                n => format!("Synced {n} snapshots from the node."),
            }
        }
        Ok(_) => "Unexpected response from the agent.".to_string(),
        Err(error) => {
            db::audit::record(
                &state.db,
                &user.0.email,
                "backup.sync",
                &site.site.domain,
                Some(&error.to_string()),
                false,
            )
            .await?;
            format!("Could not read the repository: {error}")
        }
    };

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=backups"),
        &flash,
    ))
}

// ---------------------------------------------------------------------------
// Cloning & staging
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/clone.html")]
struct ClonePage {
    chrome: Chrome,
    site: db::sites::SiteRow,
}

#[derive(Deserialize)]
pub struct CloneForm {
    pub target_domain: String,
    /// HTML checkboxes submit `on` or are omitted entirely, so this cannot be a
    /// `bool`: serde would reject both cases with 422.
    #[serde(default)]
    pub request_ssl: Option<String>,
}

/// Show the clone form for a site.
pub async fn clone_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let chrome = Chrome::new(&state, &user, &session, "sites", "Clone Site", None).await;
    Ok(render(ClonePage { chrome, site }))
}

/// Create a clone of the site.
pub async fn clone_create(
    State(state): State<AppState>,
    user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
    Form(form): Form<CloneForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let source = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    // Validate target domain.
    if form.target_domain.is_empty() {
        return Err(AppError::BadRequest("target domain is required".into()));
    }
    if form.target_domain == source.site.domain {
        return Err(AppError::BadRequest(
            "target domain cannot be the same as source".into(),
        ));
    }
    if db::sites::domain_exists(&state.db, &form.target_domain).await? {
        return Err(AppError::BadRequest("domain already exists".into()));
    }

    // Create the target site row in the panel.
    let target_id = db::sites::create(
        &state.db,
        db::sites::NewSite {
            server_id: source.site.server_id,
            domain: &form.target_domain,
            title: Some(&format!(
                "{} (clone)",
                source.site.title.as_deref().unwrap_or(&source.site.domain)
            )),
            php_version: source.site.php_version,
            database_mode: source.site.database_mode,
            environment: source.site.environment,
            parent_site_id: Some(source.site.id),
            limits: source.site.limits,
            cache: source.site.cache,
            request_ssl: checked(&form.request_ssl),
        },
    )
    .await?;

    if !db::teams::has_global_access(&user.0.role) {
        let _ = db::teams::grant_site_access(&state.db, target_id, user.0.id, "operator").await;
    }

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::SiteClone,
        Some(source.site.server_id),
        Some(target_id),
        Some(json!({
            "source_site_id": source.site.id,
            "source_domain": source.site.domain,
            "target_site_id": target_id,
            "target_domain": form.target_domain,
            "request_ssl": checked(&form.request_ssl),
        })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "site.clone",
        &source.site.domain,
        Some(&format!("job {job_id} -> {}", form.target_domain)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        "Clone job queued",
    ))
}

// Staging create and push use the same clone operation with staging=true.
pub async fn staging_create(
    State(state): State<AppState>,
    user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let source = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    // Generate staging domain.
    let target_domain = format!("staging.{}", source.site.domain);
    if db::sites::domain_exists(&state.db, &target_domain).await? {
        return Err(AppError::BadRequest("staging domain already exists".into()));
    }

    // Create the target site row.
    let target_id = db::sites::create(
        &state.db,
        db::sites::NewSite {
            server_id: source.site.server_id,
            domain: &target_domain,
            title: Some(&format!(
                "{} (staging)",
                source.site.title.as_deref().unwrap_or(&source.site.domain)
            )),
            php_version: source.site.php_version,
            database_mode: source.site.database_mode,
            environment: wp_common::models::Environment::Staging,
            parent_site_id: Some(source.site.id),
            limits: source.site.limits,
            cache: source.site.cache,
            request_ssl: false, // No TLS for staging by default.
        },
    )
    .await?;

    if !db::teams::has_global_access(&user.0.role) {
        let _ = db::teams::grant_site_access(&state.db, target_id, user.0.id, "operator").await;
    }

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::StagingCreate,
        Some(source.site.server_id),
        Some(target_id),
        Some(json!({
            "source_site_id": source.site.id,
            "source_domain": source.site.domain,
            "target_site_id": target_id,
            "target_domain": target_domain,
            "request_ssl": false,
        })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "staging.create",
        &source.site.domain,
        Some(&format!("job {job_id} -> staging.{}", source.site.domain)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        "Staging site creation queued",
    ))
}

pub async fn staging_push(
    State(state): State<AppState>,
    user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let staging_site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    // Verify this is actually a staging site.
    if staging_site.site.environment != wp_common::models::Environment::Staging {
        return Err(AppError::BadRequest("site is not a staging site".into()));
    }

    // Verify it has a parent (production) site.
    let parent_id = staging_site
        .site
        .parent_site_id
        .ok_or_else(|| AppError::BadRequest("staging site has no parent production site".into()))?;
    let parent = db::sites::get_for_user(&state.db, parent_id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::StagingPush,
        Some(staging_site.site.server_id),
        Some(parent.site.id), // Target the production site.
        Some(json!({
            "source_site_id": staging_site.site.id,
            "source_domain": staging_site.site.domain,
            "target_site_id": parent.site.id,
            "target_domain": parent.site.domain,
            "request_ssl": parent.site.ssl.enabled,
        })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "staging.push",
        &staging_site.site.domain,
        Some(&format!("job {job_id} -> {}", parent.site.domain)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        "Push to production queued",
    ))
}

#[derive(Debug, Deserialize)]
pub struct PhpForm {
    pub php_version: String,
}

pub async fn switch_php(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<PhpForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let version = PhpVersion::parse(&form.php_version)
        .ok_or_else(|| AppError::BadRequest("unsupported PHP version".into()))?;

    if version == site.site.php_version {
        return Ok(redirect_with_flash(
            &format!("/sites/{id}?tab=php"),
            "Site already runs that PHP version",
        ));
    }

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::PhpSwitch,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "php_version": version.as_str(), "from": site.site.php_version.as_str() })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();

    db::audit::record(
        &state.db,
        &user.0.email,
        "php.switch",
        &site.site.domain,
        Some(&format!("{} -> {}", site.site.php_version, version)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/jobs/{job_id}"),
        &format!("Switching to PHP {version}"),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CacheForm {
    #[serde(default)]
    pub fastcgi_cache: Option<String>,
    #[serde(default)]
    pub redis_object_cache: Option<String>,
    #[serde(default)]
    pub opcache: Option<String>,
    #[serde(default)]
    pub brotli: Option<String>,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u32,
}

fn default_ttl() -> u32 {
    3600
}

pub async fn update_cache(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<CacheForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let settings = CacheSettings {
        fastcgi_cache: checked(&form.fastcgi_cache),
        ttl_seconds: form.ttl_seconds.clamp(30, 2_592_000),
        redis_object_cache: checked(&form.redis_object_cache),
        opcache: checked(&form.opcache),
        brotli: checked(&form.brotli),
    };

    db::sites::set_cache(&state.db, id, settings).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "cache.update",
        &site.site.domain,
        None,
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=settings"),
        "Cache settings saved",
    ))
}

#[derive(Debug, Deserialize)]
pub struct LimitsForm {
    pub cpu_cores: f32,
    pub memory_mb: u32,
    pub php_workers: u16,
}

pub async fn update_limits(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<LimitsForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let limits = ResourceLimits {
        cpu_cores: form.cpu_cores.clamp(0.25, 32.0),
        memory_mb: form.memory_mb.clamp(256, 65_536),
        php_workers: form.php_workers.clamp(2, 128),
    };

    db::sites::set_limits(&state.db, id, limits).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "site.limits",
        &site.site.domain,
        Some(&format!(
            "{} cpu / {} MB",
            limits.cpu_cores, limits.memory_mb
        )),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=php"),
        "Resource limits saved. Restart the site to apply.",
    ))
}

#[derive(Debug, Deserialize)]
pub struct DomainForm {
    pub domain: String,
}

pub async fn add_domain(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<DomainForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let domain = form.domain.trim().to_lowercase();
    if !valid_domain(&domain) {
        return Err(AppError::BadRequest(format!(
            "`{domain}` is not a valid domain"
        )));
    }

    db::sites::add_domain(&state.db, id, &domain, false).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "domain.add",
        &site.site.domain,
        Some(&domain),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=domains"),
        "Domain added. Point DNS at the server, then issue a certificate.",
    ))
}

pub async fn remove_domain(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<DomainForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let domain = form.domain.trim().to_lowercase();

    if domain == site.site.domain {
        return Err(AppError::BadRequest(
            "the primary domain cannot be removed".into(),
        ));
    }

    db::sites::remove_domain(&state.db, id, &domain).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "domain.remove",
        &site.site.domain,
        Some(&domain),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=domains"),
        "Domain removed. Reissue the certificate to drop it from the SAN list.",
    ))
}

// ---------------------------------------------------------------------------
// HTMX fragment: status pill + live counters
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/status.html")]
struct StatusFragment {
    site: db::sites::SiteRow,
    active_jobs: Vec<db::jobs::JobRow>,
}

pub async fn status_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let active_jobs: Vec<_> = db::jobs::list_for_site(&state.db, id, 5)
        .await?
        .into_iter()
        .filter(|row| !row.job.is_terminal())
        .collect();

    Ok(super::no_store(render(StatusFragment {
        site,
        active_jobs,
    })))
}

// ---------------------------------------------------------------------------
// M2: WordPress management fragments
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/plugins.html")]
struct PluginsFragment {
    site: db::sites::SiteRow,
    plugins: Vec<PluginInfo>,
    error: Option<String>,
    csrf_token: String,
}

pub async fn plugins_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::ListPlugins { site_id: id },
        )
        .await;
    let (plugins, error) = match result {
        Ok(wp_common::protocol::OperationData::Plugins { plugins: list }) => (list, None),
        Ok(_) => (vec![], Some("unexpected agent response".into())),
        Err(e) => (vec![], Some(e.to_string())),
    };
    Ok(super::no_store(render(PluginsFragment {
        site,
        plugins,
        error,
        csrf_token: state.csrf.token(&session.0),
    })))
}

#[derive(Debug, Deserialize)]
pub struct PluginActionForm {
    pub slug: String,
    pub action: String,
}

pub async fn plugin_action(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<PluginActionForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let action = match form.action.as_str() {
        "activate" => WpItemAction::Activate,
        "deactivate" => WpItemAction::Deactivate,
        "update" => WpItemAction::Update,
        "delete" => WpItemAction::Delete,
        "install" => WpItemAction::Install,
        other => return Err(AppError::BadRequest(format!("unknown action `{other}`"))),
    };

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::PluginAction,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "slug": form.slug, "action": action.as_str() })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();
    db::audit::record(
        &state.db,
        &user.0.email,
        "plugin.action",
        &site.site.domain,
        Some(&format!("job {job_id}: {} {}", action.as_str(), form.slug)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=wordpress"),
        &format!("Plugin {} queued", action.as_str()),
    ))
}

#[derive(Template)]
#[template(path = "sites/themes.html")]
struct ThemesFragment {
    site: db::sites::SiteRow,
    themes: Vec<ThemeInfo>,
    error: Option<String>,
    csrf_token: String,
}

pub async fn themes_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::ListThemes { site_id: id },
        )
        .await;
    let (themes, error) = match result {
        Ok(wp_common::protocol::OperationData::Themes { themes: list }) => (list, None),
        Ok(_) => (vec![], Some("unexpected agent response".into())),
        Err(e) => (vec![], Some(e.to_string())),
    };
    Ok(super::no_store(render(ThemesFragment {
        site,
        themes,
        error,
        csrf_token: state.csrf.token(&session.0),
    })))
}

#[derive(Debug, Deserialize)]
pub struct ThemeActionForm {
    pub slug: String,
    pub action: String,
}

pub async fn theme_action(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<ThemeActionForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let action = match form.action.as_str() {
        "activate" => WpItemAction::Activate,
        "update" => WpItemAction::Update,
        "delete" => WpItemAction::Delete,
        "install" => WpItemAction::Install,
        other => return Err(AppError::BadRequest(format!("unknown action `{other}`"))),
    };

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::ThemeAction,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "slug": form.slug, "action": action.as_str() })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();
    db::audit::record(
        &state.db,
        &user.0.email,
        "theme.action",
        &site.site.domain,
        Some(&format!("job {job_id}: {} {}", action.as_str(), form.slug)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=wordpress"),
        &format!("Theme {} queued", action.as_str()),
    ))
}

#[derive(Template)]
#[template(path = "sites/wpusers.html")]
struct WpUsersFragment {
    site: db::sites::SiteRow,
    users: Vec<WpUserInfo>,
    error: Option<String>,
    csrf_token: String,
}

pub async fn wpusers_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::ListWpUsers { site_id: id },
        )
        .await;
    let (users, error) = match result {
        Ok(wp_common::protocol::OperationData::WpUsers { users: list }) => (list, None),
        Ok(_) => (vec![], Some("unexpected agent response".into())),
        Err(e) => (vec![], Some(e.to_string())),
    };
    Ok(super::no_store(render(WpUsersFragment {
        site,
        users,
        error,
        csrf_token: state.csrf.token(&session.0),
    })))
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordForm {
    pub user_login: String,
}

pub async fn reset_password(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<ResetPasswordForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::WpUserPasswordReset,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "user_login": form.user_login })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();
    db::audit::record(
        &state.db,
        &user.0.email,
        "wpuser.reset_password",
        &site.site.domain,
        Some(&format!("job {job_id}: {}", form.user_login)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=wordpress"),
        "Password reset queued",
    ))
}

#[derive(Template)]
#[template(path = "sites/cron.html")]
struct CronFragment {
    site: db::sites::SiteRow,
    events: Vec<CronEventInfo>,
    error: Option<String>,
    csrf_token: String,
}

pub async fn cron_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::ListCronEvents { site_id: id },
        )
        .await;
    let (events, error) = match result {
        Ok(wp_common::protocol::OperationData::CronEvents { events: list }) => (list, None),
        Ok(_) => (vec![], Some("unexpected agent response".into())),
        Err(e) => (vec![], Some(e.to_string())),
    };
    Ok(super::no_store(render(CronFragment {
        site,
        events,
        error,
        csrf_token: state.csrf.token(&session.0),
    })))
}

#[derive(Debug, Deserialize)]
pub struct CronRunForm {
    pub hook: String,
}

pub async fn cron_run(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<CronRunForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let job_id = db::jobs::enqueue(
        &state.db,
        JobKind::CronRun,
        Some(site.site.server_id),
        Some(id),
        Some(json!({ "hook": form.hook })),
        &user.0.email,
    )
    .await?;
    state.notify_jobs();
    db::audit::record(
        &state.db,
        &user.0.email,
        "cron.run",
        &site.site.domain,
        Some(&format!("job {job_id}: {}", form.hook)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{id}?tab=wordpress"),
        "Cron event queued",
    ))
}

#[derive(Template)]
#[template(path = "sites/console.html")]
struct ConsoleTemplate {
    chrome: Chrome,
    site: db::sites::SiteRow,
    output: Option<String>,
    error: Option<String>,
}

pub async fn console_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
    Query(query): Query<super::FlashQuery>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(render(ConsoleTemplate {
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "sites",
            "WP-CLI Console",
            query.flash,
        )
        .await,
        site,
        output: None,
        error: None,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ConsoleForm {
    pub command: String,
}

/// Tokenize a WP-CLI command line. Supports single/double quoted args.
/// Rejects shell metacharacters.
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command".into());
    }
    // Reject shell metacharacters
    for ch in [';', '|', '&', '`', '$', '(', ')', '<', '>', '\n', '\r'] {
        if line.contains(ch) {
            return Err(format!("character `{ch}` is not allowed"));
        }
    }
    // Simple tokenizer: split on whitespace, respect quotes
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for ch in line.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if in_single || in_double {
        return Err("unclosed quote".into());
    }
    if tokens.is_empty() {
        return Err("empty command".into());
    }
    Ok(tokens)
}

pub async fn console_submit(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
    Form(form): Form<ConsoleForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let args = match tokenize(&form.command) {
        Ok(args) => args,
        Err(e) => {
            return Ok(render(ConsoleTemplate {
                chrome: Chrome::new(&state, &user, &session, "sites", "WP-CLI Console", None).await,
                site,
                output: None,
                error: Some(e),
            }));
        }
    };

    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::WpCli { site_id: id, args },
        )
        .await;

    let (output, error) = match result {
        Ok(wp_common::protocol::OperationData::CommandOutput {
            stdout,
            stderr,
            exit_code,
        }) => {
            let mut out = stdout;
            if !stderr.is_empty() {
                out.push_str("\n--- stderr ---\n");
                out.push_str(&stderr);
            }
            if exit_code != 0 {
                out.push_str(&format!("\n(exit code: {exit_code})"));
            }
            (Some(out), None)
        }
        Ok(_) => (None, Some("unexpected agent response".into())),
        Err(e) => (None, Some(e.to_string())),
    };

    db::audit::record(
        &state.db,
        &user.0.email,
        "wpcli.run",
        &site.site.domain,
        Some(&form.command),
        error.is_none(),
    )
    .await?;

    Ok(render(ConsoleTemplate {
        chrome: Chrome::new(&state, &user, &session, "sites", "WP-CLI Console", None).await,
        site,
        output,
        error,
    }))
}

// ---------------------------------------------------------------------------
// Log viewer fragment
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    pub stream: Option<String>,
    pub lines: Option<u32>,
    pub grep: Option<String>,
}

/// Returns a `<pre class="log">` fragment for HTMX polling.
pub async fn logs_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<LogQuery>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let server = db::servers::get(&state.db, site.site.server_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let stream_name = query.stream.unwrap_or_else(|| "nginx-error".into());
    let stream = match stream_name.as_str() {
        "nginx-access" => wp_common::protocol::LogStream::NginxAccess,
        "nginx-error" => wp_common::protocol::LogStream::NginxError,
        "php-error" => wp_common::protocol::LogStream::PhpError,
        "php-slow" => wp_common::protocol::LogStream::PhpSlow,
        "wp-debug" => wp_common::protocol::LogStream::WpDebug,
        "agent" => wp_common::protocol::LogStream::Agent,
        _ => wp_common::protocol::LogStream::NginxError,
    };
    let lines = query.lines.unwrap_or(200).min(2000);
    let grep = query.grep.filter(|g| !g.is_empty() && g.len() <= 64);
    let filter = grep.clone();

    let result = state
        .agent
        .query(
            &server.connection(),
            wp_common::protocol::Operation::TailLogs {
                site_id: site.site.id,
                stream,
                lines,
                grep,
            },
        )
        .await;

    let (content, error) = match result {
        Ok(wp_common::protocol::OperationData::Lines { lines }) => (lines.join("\n"), None),
        Ok(_) => (String::new(), Some("unexpected agent response".to_string())),
        Err(e) => (String::new(), Some(e.to_string())),
    };

    // Rendered through a template so Askama does the escaping: log lines are the
    // most attacker-influenced text in the panel.
    Ok(super::no_store(render(LogsFragment {
        content,
        error,
        stream: stream_name,
        filter,
    })))
}

#[derive(Template)]
#[template(path = "sites/logs.html")]
struct LogsFragment {
    content: String,
    error: Option<String>,
    stream: String,
    /// The active filter, so an empty result can say which case it is.
    filter: Option<String>,
}

// ---------------------------------------------------------------------------
// Backup schedules
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/backup_schedule.html")]
struct BackupScheduleTemplate {
    site: db::sites::SiteRow,
    schedule: Option<db::schedules::Schedule>,
    destinations: Vec<db::destinations::Destination>,
    csrf_token: String,
    selected_dest_id: Option<i64>,
    scope: String,
    interval_minutes: i64,
    enabled: bool,
}

pub async fn backup_schedule_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let schedule = db::schedules::for_site(&state.db, id).await?;
    let destinations = db::destinations::list(&state.db).await?;
    let csrf_token = state.csrf.token(&session.0);

    let selected_dest_id = schedule.as_ref().and_then(|s| s.destination_id);
    let scope = schedule
        .as_ref()
        .map(|s| s.scope.clone())
        .unwrap_or_else(|| "full".into());
    let interval_minutes = schedule
        .as_ref()
        .map(|s| s.interval_minutes)
        .unwrap_or(1440);
    let enabled = schedule.as_ref().map(|s| s.enabled).unwrap_or(false);

    Ok(super::no_store(render(BackupScheduleTemplate {
        site,
        schedule,
        destinations,
        csrf_token,
        selected_dest_id,
        scope,
        interval_minutes,
        enabled,
    })))
}

#[derive(Deserialize)]
pub struct BackupScheduleForm {
    pub destination_id: i64,
    pub scope: String,
    pub interval_minutes: i64,
    #[serde(default)]
    pub enabled: Option<String>,
}

pub async fn upsert_backup_schedule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<BackupScheduleForm>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    let _ = db::destinations::get(&state.db, form.destination_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("invalid destination".into()))?;

    db::schedules::upsert(
        &state.db,
        db::schedules::ScheduleForm {
            site_id: id,
            destination_id: Some(form.destination_id),
            scope: form.scope,
            interval_minutes: form.interval_minutes,
            enabled: form.enabled.is_some(),
            keep_hourly: 24,
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 6,
        },
    )
    .await?;

    db::audit::record(
        &state.db,
        &user.0.email,
        "backup_schedule.update",
        &site.site.domain,
        None,
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        &format!("/sites/{}?tab=backups", id),
        "Backup schedule updated",
    ))
}

pub async fn delete_backup_schedule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_operator_or_above(&user)?;
    let site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    if let Some(schedule) = db::schedules::for_site(&state.db, id).await? {
        db::schedules::delete(&state.db, schedule.id).await?;

        db::audit::record(
            &state.db,
            &user.0.email,
            "backup_schedule.delete",
            &site.site.domain,
            None,
            true,
        )
        .await?;
    }

    Ok(redirect_with_flash(
        &format!("/sites/{}?tab=backups", id),
        "Backup schedule removed",
    ))
}

// ---------------------------------------------------------------------------
// Metrics fragment
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "sites/metrics.html")]
struct SiteMetricsFragment {
    cpu_spark: crate::web::servers::Spark,
    memory_spark: crate::web::servers::Spark,
    php_busy_spark: crate::web::servers::Spark,
}

pub async fn metrics_fragment(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let _site = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;
    let history = db::metrics::site_history(&state.db, id, 24)
        .await
        .unwrap_or_default();

    let cpu_vals: Vec<f32> = history.iter().map(|r| r.cpu as f32).collect();
    let mem_vals: Vec<f32> = history.iter().map(|r| r.memory_mb as f32).collect();
    let php_vals: Vec<f32> = history.iter().map(|r| r.php_busy as f32).collect();

    Ok(super::no_store(render(SiteMetricsFragment {
        cpu_spark: crate::web::servers::spark_from_values(&cpu_vals),
        memory_spark: crate::web::servers::spark_from_values(&mem_vals),
        php_busy_spark: crate::web::servers::spark_from_values(&php_vals),
    })))
}
