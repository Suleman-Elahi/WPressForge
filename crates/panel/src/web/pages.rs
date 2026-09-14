use super::{redirect_with_flash, render, Chrome, FlashQuery};
use crate::auth::{self, CurrentSession, CurrentUser};
use crate::db;
use crate::error::AppResult;
use crate::state::AppState;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;
use wp_common::models::{AuditEntry, SiteStatus};

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    chrome: Chrome,
    servers: Vec<db::servers::ServerRow>,
    sites: Vec<db::sites::SiteRow>,
    jobs: Vec<db::jobs::JobRow>,
    total_sites: i64,
    online_sites: i64,
    total_servers: i64,
    active_jobs: i64,
}

pub async fn dashboard(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let servers = db::servers::list(&state.db).await?;
    let mut sites = db::sites::list(&state.db).await?;
    sites.truncate(6);
    let jobs = db::jobs::list(&state.db, 8).await?;

    let template = DashboardTemplate {
        chrome: Chrome::new(&state, &user, &session, "dashboard", "Overview", query.flash).await,
        total_servers: servers.len() as i64,
        total_sites: db::sites::count(&state.db).await?,
        online_sites: db::sites::count_by_status(&state.db, SiteStatus::Online).await?,
        active_jobs: db::jobs::count_active(&state.db).await?,
        servers,
        sites,
        jobs,
    };

    Ok(render(template))
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "audit.html")]
struct AuditTemplate {
    chrome: Chrome,
    entries: Vec<AuditEntry>,
}

pub async fn audit(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let entries = db::audit::list(&state.db, 200).await?;
    Ok(render(AuditTemplate {
        chrome: Chrome::new(&state, &user, &session, "audit", "Audit log", query.flash).await,
        entries,
    }))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    chrome: Chrome,
    email: String,
    role: String,
    member_since: String,
    last_login: String,
    version: &'static str,
    uptime: String,
    workers: usize,
    database: String,
    demo_data: bool,
    tokens: Vec<db::tokens::ApiToken>,
}

pub async fn settings(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let uptime = humantime::format_duration(std::time::Duration::from_secs(
        state.started_at.elapsed().as_secs(),
    ))
    .to_string();
    let tokens = db::tokens::list(&state.db, user.0.id).await?;

    Ok(render(SettingsTemplate {
        chrome: Chrome::new(&state, &user, &session, "settings", "Settings", query.flash).await,
        email: user.0.email.clone(),
        role: user.0.role.clone(),
        member_since: wp_common::fmt::timestamp(user.0.created_at),
        last_login: match user.0.last_login_at {
            Some(when) => wp_common::fmt::relative(when),
            None => "this session".to_string(),
        },
        version: env!("CARGO_PKG_VERSION"),
        uptime,
        workers: state.config.workers,
        database: state.config.database.display().to_string(),
        demo_data: state.config.demo_data,
        tokens,
    }))
}

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    pub name: String,
}

pub async fn create_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TokenForm>,
) -> AppResult<Response> {
    let name = form.name.trim();
    if name.is_empty() {
        return Err(crate::error::AppError::BadRequest("token name is required".into()));
    }
    let (_id, plaintext) = db::tokens::create(&state.db, user.0.id, name).await?;
    Ok(redirect_with_flash(
        "/settings",
        &format!("Token created: {plaintext} — copy it now, it won't be shown again."),
    ))
}

pub async fn revoke_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    db::tokens::revoke(&state.db, user.0.id, id).await?;
    Ok(redirect_with_flash("/settings", "Token revoked."))
}

// ---------------------------------------------------------------------------
// Login / logout
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
    email: String,
}

pub async fn login_form() -> Response {
    render(LoginTemplate {
        error: None,
        email: String::new(),
    })
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
}

pub async fn login_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let ip = if ip.is_empty() { "127.0.0.1".to_string() } else { ip };

    // Check throttle before any crypto work.
    if auth::is_locked(&state.db, &ip).await? {
        auth::record_attempt(&state.db, &ip, &form.email, false).await?;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            render(LoginTemplate {
                error: Some("Too many attempts. Try again in 15 minutes.".into()),
                email: form.email,
            }),
        )
            .into_response());
    }

    match auth::login(&state, &form.email, &form.password, user_agent, &ip).await? {
        Some(outcome) => {
            auth::clear_failures(&state.db, &ip).await?;
            auth::record_attempt(&state.db, &ip, &outcome.user.email, true).await?;
            db::audit::record(
                &state.db,
                &outcome.user.email,
                "auth.login",
                "panel",
                None,
                true,
            )
            .await?;

            let mut response = Redirect::to("/").into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                auth::session_cookie(&outcome.token, state.config.secure_cookies)
                    .parse()
                    .expect("valid cookie"),
            );
            Ok(response)
        }
        None => {
            auth::record_attempt(&state.db, &ip, &form.email, false).await?;
            db::audit::record(
                &state.db,
                &form.email,
                "auth.login",
                "panel",
                Some("invalid credentials"),
                false,
            )
            .await?;

            Ok((
                StatusCode::UNAUTHORIZED,
                render(LoginTemplate {
                    error: Some("Incorrect email or password.".into()),
                    email: form.email,
                }),
            )
                .into_response())
        }
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        if let Some(token) = cookie
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| k.trim() == auth::COOKIE_NAME)
            .map(|(_, v)| v.trim())
        {
            auth::logout(&state, token).await;
        }
    }

    let mut response = Redirect::to("/login").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        auth::clear_cookie(state.config.secure_cookies)
            .parse()
            .expect("valid cookie"),
    );
    response
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "notifications.html")]
struct NotificationsTemplate {
    chrome: Chrome,
    open: Vec<db::notifications::Notification>,
    recent: Vec<db::notifications::Notification>,
}

pub async fn notifications(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let open = db::notifications::list_open(&state.db, 50).await?;
    let recent = db::notifications::list_recent(&state.db, 20).await?;
    Ok(render(NotificationsTemplate {
        chrome: Chrome::new(&state, &user, &session, "notifications", "Notifications", query.flash).await,
        open,
        recent,
    }))
}

pub async fn resolve_notification(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    db::notifications::resolve(&state.db, id).await?;
    Ok(super::redirect_with_flash("/notifications", "Notification resolved"))
}

/// HTMX fragment: notification badge (open count).
pub async fn notification_badge(
    State(state): State<AppState>,
) -> AppResult<Response> {
    let count = db::notifications::count_open(&state.db).await.unwrap_or(0);
    let html = if count > 0 {
        format!(
            "<a href=\"/notifications\" class=\"btn sm\" style=\"position:relative\">Notifications <span class=\"pill bad\" style=\"margin-left:.25rem\">{count}</span></a>"
        )
    } else {
        "<a href=\"/notifications\" class=\"btn sm ghost\">Notifications</a>".into()
    };
    Ok(axum::response::Html(html).into_response())
}
