//! Server-rendered UI. Askama templates, HTMX for the few places that need
//! live updates, no client-side framework and no build step.

pub mod destinations;
pub mod imports;
pub mod jobs;
pub mod pages;
pub mod servers;
pub mod sites;
pub mod users;

use crate::auth::{CurrentSession, CurrentUser};
use crate::state::AppState;
use askama::Template;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;

/// Chrome shared by every full page render: sidebar state and the top bar.
pub struct Chrome {
    pub title: String,
    pub section: &'static str,
    pub user_name: String,
    pub user_initials: String,
    pub user_role: String,
    pub active_jobs: i64,
    pub flash: Option<String>,
    pub csrf_token: String,
}

impl Chrome {
    pub async fn new(
        state: &AppState,
        user: &CurrentUser,
        session: &CurrentSession,
        section: &'static str,
        title: impl Into<String>,
        flash: Option<String>,
    ) -> Self {
        let active_jobs = crate::db::jobs::count_active(&state.db).await.unwrap_or(0);
        let csrf_token = state.csrf.token(&session.0);
        Self {
            title: title.into(),
            section,
            user_name: user.0.display_name().to_string(),
            user_initials: user.0.initials(),
            user_role: user.0.role.clone(),
            active_jobs,
            flash,
            csrf_token,
        }
    }
}

/// Query string used by every page that can carry a flash message.
#[derive(Debug, Default, Deserialize)]
pub struct FlashQuery {
    pub flash: Option<String>,
}

/// Renders a template into an HTML response.
pub fn render<T: Template>(template: T) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(error) => {
            tracing::error!(%error, "template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "template render failed",
                )),
            )
                .into_response()
        }
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTemplate<'a> {
    code: u16,
    title: &'a str,
    message: &'a str,
}

/// Standalone error page (no chrome: it must render even when state is broken).
pub fn render_error(status: StatusCode, message: &str) -> Html<String> {
    let template = ErrorTemplate {
        code: status.as_u16(),
        title: status.canonical_reason().unwrap_or("Error"),
        message,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| format!("<h1>{}</h1><p>{}</p>", status.as_u16(), message)),
    )
}

/// Redirect back to `path` with a flash message attached.
pub fn redirect_with_flash(path: &str, flash: &str) -> Response {
    let separator = if path.contains('?') { '&' } else { '?' };
    Redirect::to(&format!("{path}{separator}flash={}", urlencode(flash))).into_response()
}

fn urlencode(input: &str) -> String {
    input
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Fragments returned to HTMX carry cache headers that keep them out of the
/// browser cache; full pages are always dynamic too.
pub fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(pages::dashboard))
        .route("/audit", get(pages::audit))
        .route("/settings", get(pages::settings))
        .route("/settings/tokens", post(pages::create_token))
        .route("/settings/tokens/{id}/revoke", post(pages::revoke_token))
        .route("/settings/totp/enable", post(pages::totp_enable))
        .route("/settings/totp/confirm", post(pages::totp_confirm))
        .route("/settings/totp/disable", post(pages::totp_disable))
        .route("/notifications", get(pages::notifications))
        .route(
            "/notifications/{id}/resolve",
            post(pages::resolve_notification),
        )
        .route("/partials/notifications", get(pages::notification_badge))
        .route("/servers", get(servers::list))
        .route("/servers/new", get(servers::new_form))
        .route("/servers", post(servers::create))
        .route("/servers/{id}", get(servers::detail))
        .route("/servers/{id}/delete", post(servers::delete))
        .route("/sites", get(sites::list))
        .route("/sites/new", get(sites::new_form))
        .route("/sites", post(sites::create))
        .route("/imports/new", get(imports::new_form))
        .route("/imports/inspect", post(imports::inspect))
        .route("/imports", post(imports::create))
        .route("/sites/{id}", get(sites::detail))
        .route("/sites/{id}/actions/{action}", post(sites::action))
        .route("/sites/{id}/php", post(sites::switch_php))
        .route("/sites/{id}/cache", post(sites::update_cache))
        .route("/sites/{id}/limits", post(sites::update_limits))
        .route("/sites/{id}/domains", post(sites::add_domain))
        .route("/sites/{id}/domains/remove", post(sites::remove_domain))
        // M2: WordPress management
        .route("/sites/{id}/plugins/action", post(sites::plugin_action))
        .route("/sites/{id}/themes/action", post(sites::theme_action))
        .route(
            "/sites/{id}/wpusers/reset-password",
            post(sites::reset_password),
        )
        .route("/sites/{id}/cron/run", post(sites::cron_run))
        .route("/sites/{id}/console", get(sites::console_form))
        .route("/sites/{id}/console", post(sites::console_submit))
        // M3: Backup restore
        .route(
            "/sites/{id}/backups/{snapshot}/restore",
            post(sites::restore_backup),
        )
        .route("/sites/{id}/backups/sync", post(sites::sync_backups))
        // M4: Clone & staging
        .route("/sites/{id}/clone", get(sites::clone_form))
        .route("/sites/{id}/clone", post(sites::clone_create))
        .route("/sites/{id}/staging/create", post(sites::staging_create))
        .route("/sites/{id}/staging/push", post(sites::staging_push))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::detail))
        // HTMX fragments
        .route("/partials/jobs/active", get(jobs::active_fragment))
        .route("/partials/jobs/{id}", get(jobs::progress_fragment))
        .route("/partials/sites/{id}/status", get(sites::status_fragment))
        .route(
            "/partials/servers/{id}/metrics",
            get(servers::metrics_fragment),
        )
        // M2: WordPress fragments
        .route("/partials/sites/{id}/plugins", get(sites::plugins_fragment))
        .route("/partials/sites/{id}/themes", get(sites::themes_fragment))
        .route("/partials/sites/{id}/wpusers", get(sites::wpusers_fragment))
        .route("/partials/sites/{id}/cron", get(sites::cron_fragment))
        .route("/partials/sites/{id}/logs", get(sites::logs_fragment))
        // M3: Backup destinations
        .route("/settings/destinations", get(destinations::list))
        .route("/settings/destinations/new", get(destinations::new_form))
        .route("/settings/destinations", post(destinations::create))
        .route(
            "/settings/destinations/{id}/delete",
            post(destinations::delete),
        )
        .route("/settings/destinations/{id}/test", post(destinations::test))
        .route("/partials/sites/{id}/metrics", get(sites::metrics_fragment))
        .route(
            "/partials/sites/{id}/backup-schedule",
            get(sites::backup_schedule_fragment),
        )
        .route(
            "/sites/{id}/backup-schedule",
            post(sites::upsert_backup_schedule),
        )
        .route(
            "/sites/{id}/backup-schedule/delete",
            post(sites::delete_backup_schedule),
        )
        // M7: Team management
        .route("/settings/users", get(users::list))
        .route("/settings/users/invite", post(users::invite))
        .route("/settings/users/{id}/role", post(users::update_role))
        .route("/settings/users/{id}/remove", post(users::remove))
        .route(
            "/settings/users/invitations/{id}/revoke",
            post(users::revoke_invitation),
        )
        .route("/settings/users/{id}/sites", post(users::grant_site))
        .route(
            "/settings/users/{id}/sites/{site_id}/revoke",
            post(users::revoke_site),
        )
        // SMTP test
        .route("/settings/smtp/test", post(pages::test_smtp))
}
