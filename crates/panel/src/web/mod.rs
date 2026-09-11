//! Server-rendered UI. Askama templates, HTMX for the few places that need
//! live updates, no client-side framework and no build step.

pub mod jobs;
pub mod pages;
pub mod servers;
pub mod sites;

use crate::auth::CurrentUser;
use crate::state::AppState;
use askama::Template;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

/// Chrome shared by every full page render: sidebar state and the top bar.
pub struct Chrome {
    pub title: String,
    pub section: &'static str,
    pub user_name: String,
    pub user_initials: String,
    pub active_jobs: i64,
    pub flash: Option<String>,
}

impl Chrome {
    pub async fn new(
        state: &AppState,
        user: &CurrentUser,
        section: &'static str,
        title: impl Into<String>,
        flash: Option<String>,
    ) -> Self {
        let active_jobs = crate::db::jobs::count_active(&state.db).await.unwrap_or(0);
        Self {
            title: title.into(),
            section,
            user_name: user.0.display_name().to_string(),
            user_initials: user.0.initials(),
            active_jobs,
            flash,
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
    Html(template.render().unwrap_or_else(|_| {
        format!("<h1>{}</h1><p>{}</p>", status.as_u16(), message)
    }))
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
        .route("/servers", get(servers::list))
        .route("/servers/new", get(servers::new_form))
        .route("/servers", post(servers::create))
        .route("/servers/{id}", get(servers::detail))
        .route("/servers/{id}/delete", post(servers::delete))
        .route("/sites", get(sites::list))
        .route("/sites/new", get(sites::new_form))
        .route("/sites", post(sites::create))
        .route("/sites/{id}", get(sites::detail))
        .route("/sites/{id}/actions/{action}", post(sites::action))
        .route("/sites/{id}/php", post(sites::switch_php))
        .route("/sites/{id}/cache", post(sites::update_cache))
        .route("/sites/{id}/limits", post(sites::update_limits))
        .route("/sites/{id}/domains", post(sites::add_domain))
        .route("/sites/{id}/domains/remove", post(sites::remove_domain))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::detail))
        // HTMX fragments
        .route("/partials/jobs/active", get(jobs::active_fragment))
        .route("/partials/jobs/{id}", get(jobs::progress_fragment))
        .route("/partials/sites/{id}/status", get(sites::status_fragment))
        .route("/partials/servers/{id}/metrics", get(servers::metrics_fragment))
}
