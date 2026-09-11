use super::{redirect_with_flash, render, Chrome};
use crate::auth::CurrentUser;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::Form;
use serde::Deserialize;
use serde_json::json;
use wp_common::models::{
    Backup, CacheSettings, DatabaseMode, Domain, Environment, JobKind, PhpVersion, ResourceLimits,
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

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListQuery>,
) -> AppResult<Response> {
    let all = db::sites::list(&state.db).await?;
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
        chrome: Chrome::new(&state, &user, "sites", "Sites", query.flash).await,
        sites,
        query: needle,
        status_filter,
        total,
    }))
}

// ---------------------------------------------------------------------------
// Detail (tabbed)
// ---------------------------------------------------------------------------

pub const TABS: [(&str, &str); 9] = [
    ("overview", "Overview"),
    ("domains", "Domains"),
    ("wordpress", "WordPress"),
    ("php", "PHP"),
    ("database", "Database"),
    ("ssl", "SSL"),
    ("backups", "Backups"),
    ("logs", "Logs"),
    ("settings", "Settings"),
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
    Path(id): Path<i64>,
    Query(query): Query<DetailQuery>,
) -> AppResult<Response> {
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let tab = query
        .tab
        .filter(|t| TABS.iter().any(|(key, _)| key == t))
        .unwrap_or_else(|| "overview".to_string());

    let template = DetailTemplate {
        chrome: Chrome::new(
            &state,
            &user,
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
    Query(query): Query<super::FlashQuery>,
) -> AppResult<Response> {
    let servers = db::servers::list(&state.db).await?;
    Ok(render(NewTemplate {
        chrome: Chrome::new(&state, &user, "sites", "New site", query.flash).await,
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
    let domain = form.domain.trim().trim_start_matches("www.").to_lowercase();
    if !valid_domain(&domain) {
        return Err(AppError::BadRequest(format!("`{domain}` is not a valid domain")));
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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;

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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
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
        Some(&format!("{} cpu / {} MB", limits.cpu_cores, limits.memory_mb)),
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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let domain = form.domain.trim().to_lowercase();
    if !valid_domain(&domain) {
        return Err(AppError::BadRequest(format!("`{domain}` is not a valid domain")));
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
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
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
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let site = db::sites::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let active_jobs: Vec<_> = db::jobs::list_for_site(&state.db, id, 5)
        .await?
        .into_iter()
        .filter(|row| !row.job.is_terminal())
        .collect();

    Ok(super::no_store(render(StatusFragment { site, active_jobs })))
}
