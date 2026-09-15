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
    if let Some(secret_b32) = &user.0.totp_secret
        && let Some(bytes) = crate::totp::base32_decode(secret_b32)
        && crate::totp::verify(&bytes, &form.code)
    {
        db::users::confirm_totp(&state.db, user.0.id).await?;
        return Ok(redirect_with_flash(
            "/settings",
            "2FA has been successfully enabled.",
        ));
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
    if let Some(secret_b32) = &user.0.totp_secret
        && let Some(bytes) = crate::totp::base32_decode(secret_b32)
        && crate::totp::verify(&bytes, &form.code)
    {
        db::users::disable_totp(&state.db, user.0.id).await?;
        return Ok(redirect_with_flash("/settings", "2FA has been disabled."));
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
    /// Absent on the second stage of a 2FA login.
    #[serde(default)]
    pub password: Option<String>,
    /// Signed `email:timestamp:mac` handed out with the 2FA challenge.
    #[serde(default)]
    pub pending_token: Option<String>,
    /// The six digit TOTP code, second stage only.
    #[serde(default)]
    pub code: Option<String>,
}

/// How long a 2FA challenge stays valid.
const TOTP_CHALLENGE_SECONDS: i64 = 300;

/// Validates the signed challenge token and returns the email it was issued for.
fn verify_pending_token(state: &AppState, presented: &str) -> Option<String> {
    // Format: <email>:<issued_at>:<mac over "<email>:<issued_at>">
    let (payload, mac) = presented.rsplit_once(':')?;
    let (email, issued_at) = payload.rsplit_once(':')?;

    if !state.csrf.verify(payload, mac) {
        return None;
    }

    let issued_at: i64 = issued_at.parse().ok()?;
    if chrono::Utc::now().timestamp() - issued_at > TOTP_CHALLENGE_SECONDS {
        return None;
    }

    Some(email.to_string())
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
    let ip = client_ip(&headers);

    // Check throttle before any crypto work. Applies to both stages.
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

    // ---- second stage: a TOTP code against a signed challenge -------------
    if let Some(pending) = form.pending_token.as_deref() {
        let code = form.code.clone().unwrap_or_default();

        let Some(email) = verify_pending_token(&state, pending) else {
            auth::record_attempt(&state.db, &ip, &form.email, false).await?;
            return Ok(login_error(
                StatusCode::UNAUTHORIZED,
                "That sign-in attempt expired. Start again.",
                &form.email,
            ));
        };

        let Some(user) = db::users::by_email(&state.db, &email).await? else {
            return Ok(login_error(
                StatusCode::UNAUTHORIZED,
                "That sign-in attempt expired. Start again.",
                &form.email,
            ));
        };

        let verified = user
            .totp_secret
            .as_deref()
            .and_then(crate::totp::base32_decode)
            .map(|secret| crate::totp::verify(&secret, code.trim()))
            .unwrap_or(false);

        if !verified {
            auth::record_attempt(&state.db, &ip, &email, false).await?;
            db::audit::record(
                &state.db,
                &email,
                "auth.login",
                "panel",
                Some("invalid 2FA code"),
                false,
            )
            .await?;

            // Re-issue the challenge so the operator can retry within the window.
            return Ok((
                StatusCode::UNAUTHORIZED,
                render(LoginTemplate {
                    error: Some("Incorrect 2FA code.".into()),
                    email: email.clone(),
                    pending_totp_token: Some(pending.to_string()),
                }),
            )
                .into_response());
        }

        let token = auth::start_session(&state, &user, user_agent, &ip).await?;
        auth::clear_failures(&state.db, &ip).await?;
        auth::record_attempt(&state.db, &ip, &email, true).await?;
        db::audit::record(
            &state.db,
            &email,
            "auth.login",
            "panel",
            Some("password + 2FA"),
            true,
        )
        .await?;

        return Ok(session_response(&state, &token));
    }

    // ---- first stage: email + password ------------------------------------
    let password = form.password.clone().unwrap_or_default();
    let Some(user) = auth::verify_credentials(&state, &form.email, &password).await? else {
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
        return Ok(login_error(
            StatusCode::UNAUTHORIZED,
            "Incorrect email or password.",
            &form.email,
        ));
    };

    // 2FA enabled: no session yet, just a signed five minute challenge.
    if user.totp_confirmed_at.is_some() {
        let payload = format!("{}:{}", user.email, chrono::Utc::now().timestamp());
        let mac = state.csrf.token(&payload);
        return Ok(render(LoginTemplate {
            error: None,
            email: user.email.clone(),
            pending_totp_token: Some(format!("{payload}:{mac}")),
        })
        .into_response());
    }

    let token = auth::start_session(&state, &user, user_agent, &ip).await?;
    auth::clear_failures(&state.db, &ip).await?;
    auth::record_attempt(&state.db, &ip, &user.email, true).await?;
    db::audit::record(&state.db, &user.email, "auth.login", "panel", None, true).await?;

    Ok(session_response(&state, &token))
}

/// Client IP: first hop of `x-forwarded-for`, else loopback.
fn client_ip(headers: &HeaderMap) -> String {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    if forwarded.is_empty() {
        "127.0.0.1".to_string()
    } else {
        forwarded
    }
}

fn login_error(status: StatusCode, message: &str, email: &str) -> Response {
    (
        status,
        render(LoginTemplate {
            error: Some(message.to_string()),
            email: email.to_string(),
            pending_totp_token: None,
        }),
    )
        .into_response()
}

/// Redirect to the dashboard with the session cookie attached.
fn session_response(state: &AppState, token: &str) -> Response {
    let mut response = Redirect::to("/").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        auth::session_cookie(token, state.config.secure_cookies)
            .parse()
            .expect("valid cookie"),
    );
    response
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())
        && let Some(token) = cookie
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| k.trim() == auth::COOKIE_NAME)
            .map(|(_, v)| v.trim())
    {
        auth::logout(&state, token).await;
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
