//! Session authentication: Argon2 password hashing, opaque session cookies and
//! an axum middleware that guards every route except `/login` and `/static`.

use crate::db::users::{self, User};
use crate::state::AppState;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use chrono::Duration;
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

fn cookie_value(request: &Request, name: &str) -> Option<String> {
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

    if let Some(_token) = bearer {
        // API tokens are hashed in `api_tokens`; wiring is part of the API
        // milestone, so for now only session auth is accepted.
        return (StatusCode::UNAUTHORIZED, "api tokens not enabled yet").into_response();
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
