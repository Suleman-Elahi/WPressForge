use super::{Chrome, FlashQuery, redirect_with_flash, render};
use crate::auth::{self, CurrentSession, CurrentUser};
use crate::db;
use crate::error::AppResult;
use crate::state::AppState;
use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
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
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "dashboard",
            "Overview",
            query.flash,
        )
        .await,
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
    totp_secret: Option<String>,
    totp_uri: Option<String>,
    totp_confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    smtp_host: Option<String>,
    smtp_port: u16,
    smtp_from: Option<String>,
    smtp_user: Option<String>,
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

    let mut totp_secret = None;
    let mut totp_uri = None;
    if let Some(secret_b32) = &user.0.totp_secret {
        totp_uri = Some(crate::totp::otpauth_uri(secret_b32, &user.0.email));
        totp_secret = Some(secret_b32.clone());
    }

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
        totp_secret,
        totp_uri,
        totp_confirmed_at: user.0.totp_confirmed_at,
        smtp_host: state.config.smtp_host.clone(),
        smtp_port: state.config.smtp_port,
        smtp_from: state.config.smtp_from.clone(),
        smtp_user: state.config.smtp_user.clone(),
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
        return Err(crate::error::AppError::BadRequest(
            "token name is required".into(),
        ));
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
// TOTP
// ---------------------------------------------------------------------------

pub async fn totp_enable(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    let bytes = crate::totp::generate_secret();
    let secret_b32 = crate::totp::base32_encode(&bytes);
    db::users::set_totp_secret(&state.db, user.0.id, &secret_b32).await?;
    Ok(Redirect::to("/settings").into_response())
}

#[derive(Debug, Deserialize)]
pub struct TotpCodeForm {
    pub code: String,
}

pub async fn totp_confirm(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TotpCodeForm>,
) -> AppResult<Response> {
    if let Some(secret_b32) = &user.0.totp_secret {
        if let Some(bytes) = crate::totp::base32_decode(secret_b32) {
            if crate::totp::verify(&bytes, &form.code) {
                db::users::confirm_totp(&state.db, user.0.id).await?;
                return Ok(redirect_with_flash(
                    "/settings",
                    "2FA has been successfully enabled.",
                ));
            }
        }
    }
    Ok(redirect_with_flash(
        "/settings",
        "Invalid 2FA code. Try again.",
    ))
}

pub async fn totp_disable(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<TotpCodeForm>,
) -> AppResult<Response> {
    if let Some(secret_b32) = &user.0.totp_secret {
        if let Some(bytes) = crate::totp::base32_decode(secret_b32) {
            if crate::totp::verify(&bytes, &form.code) {
                db::users::disable_totp(&state.db, user.0.id).await?;
                return Ok(redirect_with_flash("/settings", "2FA has been disabled."));
            }
        }
    }
    Ok(redirect_with_flash("/settings", "Invalid 2FA code."))
}

// ---------------------------------------------------------------------------
// Login / logout
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: Option<String>,
    email: String,
    pending_totp_token: Option<String>,
}

pub async fn login_form() -> Response {
    render(LoginTemplate {
        error: None,
        email: String::new(),
        pending_totp_token: None,
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
    let ip = if ip.is_empty() {
        "127.0.0.1".to_string()
    } else {
        ip
    };

    // Check throttle before any crypto work.
    if auth::is_locked(&state.db, &ip).await? {
        auth::record_attempt(&state.db, &ip, &form.email, false).await?;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            render(LoginTemplate {
                error: Some("Too many attempts. Try again in 15 minutes.".into()),
                email: form.email,
                pending_totp_token: None,
            }),
        )
            .into_response());
    }

    match auth::login(&state, &form.email, &form.password, user_agent, &ip).await? {
        Some(outcome) => {
            auth::clear_failures(&state.db, &ip).await?;
            auth::record_attempt(&state.db, &ip, &outcome.user.email, true).await?;

            if outcome.user.totp_confirmed_at.is_some() {
                let timestamp = chrono::Utc::now().timestamp();
                let payload = format!("{}:{}", outcome.user.email, timestamp);
                let pending_token = state.csrf.token(&payload);

                return Ok(render(LoginTemplate {
                    error: None,
                    email: outcome.user.email.clone(),
                    pending_totp_token: Some(format!("{}:{}", payload, pending_token)),
                })
                .into_response());
            }

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
                    pending_totp_token: None,
                }),
            )
                .into_response())
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TotpLoginForm {
    pub email: String,
    pub pending_token: String,
    pub code: String,
}

pub async fn login_totp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<TotpLoginForm>,
) -> AppResult<Response> {
    let parts: Vec<&str> = form.pending_token.splitn(3, ':').collect();
    if parts.len() != 3 {
        return Ok(Redirect::to("/login").into_response());
    }

    let token_email = parts[0];
    let timestamp_str = parts[1];
    let signature = parts[2];

    if token_email != form.email {
        return Ok(Redirect::to("/login").into_response());
    }

    let payload = format!("{}:{}", token_email, timestamp_str);
    if !state.csrf.verify(&payload, signature) {
        return Ok(Redirect::to("/login").into_response());
    }

    let timestamp: i64 = timestamp_str.parse().unwrap_or(0);
    if chrono::Utc::now().timestamp() - timestamp > 300 {
        return Ok(render(LoginTemplate {
            error: Some("Login timeout. Please try again.".into()),
            email: form.email.clone(),
            pending_totp_token: None,
        })
        .into_response());
    }

    let user = match db::users::by_email(&state.db, &form.email).await? {
        Some(u) => u,
        None => return Ok(Redirect::to("/login").into_response()),
    };

    if let Some(secret_b32) = &user.totp_secret {
        if let Some(secret_bytes) = crate::totp::base32_decode(secret_b32) {
            if !crate::totp::verify(&secret_bytes, &form.code) {
                return Ok(render(LoginTemplate {
                    error: Some("Invalid code.".into()),
                    email: form.email,
                    pending_totp_token: Some(form.pending_token),
                })
                .into_response());
            }
        } else {
            return Ok(Redirect::to("/login").into_response());
        }
    }

    // We actually need a way to decode base32. Let's just decode base64, so we should store base64 in DB, and base32 is only for the URI.

    // Ok, let's implement login success:
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
    let ip = if ip.is_empty() {
        "127.0.0.1".to_string()
    } else {
        ip
    };

    let token = crate::auth::random_token();
    db::users::create_session(
        &state.db,
        &token,
        user.id,
        user_agent,
        &ip,
        chrono::Duration::days(crate::auth::SESSION_TTL_DAYS),
    )
    .await?;
    db::users::touch_login(&state.db, user.id).await?;

    db::audit::record(&state.db, &user.email, "auth.login", "panel", None, true).await?;

    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        crate::auth::session_cookie(&token, state.config.secure_cookies)
            .parse()
            .unwrap(),
    );
    Ok(response)
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
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "notifications",
            "Notifications",
            query.flash,
        )
        .await,
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
    Ok(super::redirect_with_flash(
        "/notifications",
        "Notification resolved",
    ))
}

/// HTMX fragment: notification badge (open count).
pub async fn notification_badge(State(state): State<AppState>) -> AppResult<Response> {
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

pub async fn test_smtp(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    let msg = crate::email::EmailMessage {
        to: user.0.email.clone(),
        subject: "WPressForge SMTP Test".to_string(),
        body_text: format!(
            "Hello {},\n\n\
             This is a test email sent from WPressForge to confirm that outbound SMTP delivery is working correctly.\n\n\
             Regards,\n\
             WPressForge Team\n",
            user.0.name
        ),
    };

    match crate::email::send_email(&state.config, &msg).await {
        Ok(_) => {
            let note = if state.config.smtp_host.is_some() {
                format!("Test email delivered via SMTP to {}", user.0.email)
            } else {
                format!(
                    "Simulated email logged (SMTP unconfigured) for {}",
                    user.0.email
                )
            };
            Ok(super::redirect_with_flash("/settings", &note))
        }
        Err(e) => Ok(super::redirect_with_flash(
            "/settings",
            &format!("SMTP delivery error: {e}"),
        )),
    }
}
