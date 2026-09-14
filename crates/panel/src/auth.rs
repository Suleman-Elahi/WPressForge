//! Session authentication: Argon2 password hashing, opaque session cookies and
//! an axum middleware that guards every route except `/login` and `/static`.

use crate::db::users::{self, User};
use crate::state::AppState;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use chrono::{Duration, Utc};
use rand::RngCore;

pub const COOKIE_NAME: &str = "wp_panel_session";
pub const SESSION_TTL_DAYS: i64 = 14;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    // 16 random bytes, base64 encoded, is the recommended Argon2 salt size.
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| anyhow::anyhow!("encoding salt: {e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 256 bits of entropy, URL-safe. Used for session cookies, API tokens and
/// generated passwords.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn random_password() -> String {
    random_token().chars().take(20).collect()
}

pub struct LoginOutcome {
    pub token: String,
    pub user: User,
}

pub async fn login(
    state: &AppState,
    email: &str,
    password: &str,
    user_agent: &str,
    ip: &str,
) -> anyhow::Result<Option<LoginOutcome>> {
    let Some(user) = users::by_email(&state.db, email.trim()).await? else {
        // Constant-ish work on unknown accounts to avoid a trivial oracle.
        let _ = verify_password(password, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$0000000000000000000000000000000000000000000");
        return Ok(None);
    };

    if !verify_password(password, &user.password_hash) {
        return Ok(None);
    }

    let token = random_token();
    users::create_session(
        &state.db,
        &token,
        user.id,
        user_agent,
        ip,
        Duration::days(SESSION_TTL_DAYS),
    )
    .await?;
    users::touch_login(&state.db, user.id).await?;

    Ok(Some(LoginOutcome { token, user }))
}

pub async fn logout(state: &AppState, token: &str) {
    let _ = users::delete_session(&state.db, token).await;
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        SESSION_TTL_DAYS * 24 * 3600
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub fn clear_cookie(secure: bool) -> String {
    let mut cookie = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub(crate) fn cookie_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
}

/// Guard for HTML routes: unauthenticated browsers are redirected to /login,
/// unauthenticated HTMX requests get a client-side redirect header instead so
/// partial swaps do not paint a login form inside a panel.
///
/// For authenticated requests, also enforces CSRF on state-changing methods
/// (POST, PUT, DELETE, PATCH) by checking the `x-csrf-token` header or the
/// `csrf_token` form field.
pub async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = cookie_value(&request, COOKIE_NAME);

    let user = match token.as_deref() {
        Some(token) => users::user_for_session(&state.db, token).await.ok().flatten(),
        None => None,
    };

    match user {
        Some(user) => {
            // CSRF check for state-changing methods.
            if !request.method().is_safe() {
                // Try header first (HTMX path — no body buffering needed).
                let presented = request
                    .headers()
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);

                if let Some(presented) = presented {
                    if !crate::csrf::check_token(&state.csrf, token.as_deref(), Some(&presented)) {
                        tracing::warn!(path = %request.uri().path(), "CSRF token rejected (header)");
                        let _ = crate::db::audit::record(
                            &state.db,
                            &user.email,
                            "auth.csrf_reject",
                            request.uri().path(),
                            None,
                            false,
                        )
                        .await;
                        return (StatusCode::FORBIDDEN, "Invalid form token").into_response();
                    }
                } else {
                    // Buffer body to read csrf_token form field.
                    let (parts, body) = request.into_parts();
                    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
                        Ok(b) => b,
                        Err(_) => {
                            return (StatusCode::BAD_REQUEST, "invalid request body")
                                .into_response()
                        }
                    };

                    let presented: Option<String> = serde_urlencoded::from_bytes::<
                        std::collections::HashMap<String, String>,
                    >(&bytes)
                    .ok()
                    .and_then(|m| m.get("csrf_token").cloned());

                    if !crate::csrf::check_token(
                        &state.csrf,
                        token.as_deref(),
                        presented.as_deref(),
                    ) {
                        tracing::warn!(path = %parts.uri.path(), "CSRF token rejected (form)");
                        let _ = crate::db::audit::record(
                            &state.db,
                            &user.email,
                            "auth.csrf_reject",
                            parts.uri.path(),
                            None,
                            false,
                        )
                        .await;
                        return (StatusCode::FORBIDDEN, "Invalid form token").into_response();
                    }

                    // Rebuild request with the consumed body.
                    request = Request::from_parts(parts, Body::from(bytes));
                }
            }

            request
                .extensions_mut()
                .insert(CurrentSession(token.unwrap_or_default()));
            request.extensions_mut().insert(CurrentUser(user));
            next.run(request).await
        }
        None => {
            if request.headers().contains_key("hx-request") {
                let mut response = StatusCode::UNAUTHORIZED.into_response();
                response
                    .headers_mut()
                    .insert("hx-redirect", header::HeaderValue::from_static("/login"));
                response
            } else {
                Redirect::to("/login").into_response()
            }
        }
    }
}

/// Guard for the JSON API: bearer token or session cookie, never a redirect.
pub async fn require_api_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(token) = cookie_value(&request, COOKIE_NAME) {
        if let Ok(Some(user)) = users::user_for_session(&state.db, &token).await {
            request.extensions_mut().insert(CurrentUser(user));
            return next.run(request).await;
        }
    }

    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);

    if let Some(token) = bearer {
        match crate::db::tokens::user_for_token(&state.db, &token).await {
            Ok(Some(user)) => {
                request.extensions_mut().insert(CurrentUser(user));
                return next.run(request).await;
            }
            Ok(None) => {
                return (StatusCode::UNAUTHORIZED, "invalid or revoked token").into_response();
            }
            Err(e) => {
                tracing::error!(%e, "api token lookup");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            }
        }
    }

    (StatusCode::UNAUTHORIZED, "authentication required").into_response()
}

/// The authenticated user, injected by the middleware above.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub User);

impl<S> axum::extract::FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(|| Redirect::to("/login").into_response())
    }
}

/// The raw session token, injected alongside [`CurrentUser`] so the CSRF
/// middleware can derive a token without a second cookie parse.
#[derive(Debug, Clone)]
pub struct CurrentSession(pub String);

impl<S> axum::extract::FromRequestParts<S> for CurrentSession
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentSession>()
            .cloned()
            .ok_or_else(|| Redirect::to("/login").into_response())
    }
}

// ---------------------------------------------------------------------------
// Login throttling
// ---------------------------------------------------------------------------

const THROTTLE_WINDOW_MINUTES: i64 = 15;
const THROTTLE_MAX_FAILURES: i64 = 6;

/// Returns `true` if this IP has exceeded the failure threshold.
pub async fn is_locked(db: &crate::db::Db, ip: &str) -> sqlx::Result<bool> {
    let window = Utc::now() - chrono::Duration::minutes(THROTTLE_WINDOW_MINUTES);
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM login_attempts WHERE ip = ?1 AND success = 0 AND created_at >= ?2",
    )
    .bind(ip)
    .bind(window.to_rfc3339())
    .fetch_one(db)
    .await?;
    Ok(count >= THROTTLE_MAX_FAILURES)
}

/// Records a login attempt (success or failure).
pub async fn record_attempt(
    db: &crate::db::Db,
    ip: &str,
    email: &str,
    success: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO login_attempts (ip, email, success, created_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(ip)
    .bind(email)
    .bind(success)
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

/// Deletes the failed-login records for this IP (called on successful login).
pub async fn clear_failures(db: &crate::db::Db, ip: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM login_attempts WHERE ip = ?1 AND success = 0")
        .bind(ip)
        .execute(db)
        .await?;
    Ok(())
}

/// Prunes login_attempts older than 24 hours.
pub async fn prune_old_attempts(db: &crate::db::Db) -> sqlx::Result<()> {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    sqlx::query("DELETE FROM login_attempts WHERE created_at < ?1")
        .bind(cutoff.to_rfc3339())
        .execute(db)
        .await?;
    Ok(())
}
