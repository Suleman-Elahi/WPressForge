//! Team and user management: list users, invite, update roles, remove.

use super::{Chrome, FlashQuery, redirect_with_flash, render};
use crate::auth::{self, CurrentSession, CurrentUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// User list
// ---------------------------------------------------------------------------

pub struct UserWithSites {
    pub summary: db::teams::UserSummary,
    pub sites: Vec<db::teams::SiteAccess>,
}

#[derive(Template)]
#[template(path = "settings/users.html")]
struct UsersTemplate {
    chrome: Chrome,
    users: Vec<UserWithSites>,
    all_sites: Vec<db::sites::SiteRow>,
    invitations: Vec<db::teams::Invitation>,
    can_manage: bool,
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let raw_users = db::teams::list_all(&state.db).await?;
    let mut users = Vec::with_capacity(raw_users.len());
    for u in raw_users {
        let sites = db::teams::site_grants_for_user(&state.db, u.id)
            .await
            .unwrap_or_default();
        users.push(UserWithSites { summary: u, sites });
    }
    let all_sites = db::sites::list(&state.db).await.unwrap_or_default();
    let invitations = db::teams::list_invitations(&state.db).await?;

    Ok(render(UsersTemplate {
        chrome: Chrome::new(&state, &user, &session, "settings", "Team", query.flash).await,
        can_manage: db::teams::has_global_access(&user.0.role),
        users,
        all_sites,
        invitations,
    }))
}

// ---------------------------------------------------------------------------
// Invite user
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct InviteForm {
    pub email: String,
    pub role: String,
}

pub async fn invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(form): Form<InviteForm>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let email = form.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(AppError::BadRequest("Invalid email address".into()));
    }

    let role = form.role.trim();
    if !db::teams::ROLES.contains(&role) {
        return Err(AppError::BadRequest("Invalid role".into()));
    }

    // Cannot invite at your own level or higher.
    if !db::teams::can_manage(&user.0.role, role) {
        return Err(AppError::BadRequest(
            "Cannot invite a user at that privilege level".into(),
        ));
    }

    // Check for existing user.
    if let Ok(Some(_)) = db::users::by_email(&state.db, &email).await {
        return Ok(redirect_with_flash(
            "/settings/users",
            "User already exists.",
        ));
    }

    let token = auth::random_token();
    db::teams::create_invitation(&state.db, &email, role, &token, user.0.id, 7).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "team.invite",
        &email,
        Some(role),
        true,
    )
    .await?;

    let email_note = match crate::email::send_invitation(&state.config, &email, role, &token).await
    {
        Ok(_) => {
            if state.config.smtp_host.is_some() {
                " (invitation email sent via SMTP)"
            } else {
                " (email logged to console; SMTP unconfigured)"
            }
        }
        Err(_) => " (failed sending email)",
    };

    Ok(redirect_with_flash(
        "/settings/users",
        &format!("Invitation created{email_note}. Registration link: /register?token={token}"),
    ))
}

// ---------------------------------------------------------------------------
// Accept invitation (registration)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterTemplate {
    error: Option<String>,
    token: String,
    email: String,
    role: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterQuery {
    pub token: String,
}

pub async fn register_form(
    State(state): State<AppState>,
    Query(query): Query<RegisterQuery>,
) -> AppResult<Response> {
    let invitation = db::teams::get_invitation_by_token(&state.db, &query.token)
        .await?
        .ok_or(AppError::BadRequest("Invalid or expired invitation".into()))?;

    Ok(render(RegisterTemplate {
        error: None,
        token: query.token,
        email: invitation.email,
        role: invitation.role,
    }))
}

#[derive(Debug, Deserialize)]
pub struct RegisterForm {
    pub token: String,
    pub name: String,
    pub password: String,
}

pub async fn register_submit(
    State(state): State<AppState>,
    Form(form): Form<RegisterForm>,
) -> AppResult<Response> {
    let invitation = db::teams::get_invitation_by_token(&state.db, &form.token)
        .await?
        .ok_or(AppError::BadRequest("Invalid or expired invitation".into()))?;

    let name = form.name.trim();
    let password = form.password.trim();

    if password.len() < 8 {
        return Ok(render(RegisterTemplate {
            error: Some("Password must be at least 8 characters.".into()),
            token: form.token,
            email: invitation.email,
            role: invitation.role,
        }));
    }

    let hash = auth::hash_password(password)?;
    db::users::create(&state.db, &invitation.email, name, &hash, &invitation.role).await?;
    db::teams::mark_invitation_used(&state.db, invitation.id).await?;

    Ok(redirect_with_flash(
        "/login",
        "Account created. Please sign in.",
    ))
}

// ---------------------------------------------------------------------------
// Update role
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RoleForm {
    pub role: String,
}

pub async fn update_role(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<RoleForm>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let target = db::teams::get_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    let new_role = form.role.trim();
    if !db::teams::ROLES.contains(&new_role) {
        return Err(AppError::BadRequest("Invalid role".into()));
    }

    // Cannot promote someone to your own level or higher.
    if !db::teams::can_manage(&user.0.role, new_role) {
        return Err(AppError::BadRequest("Cannot assign that role".into()));
    }

    // Cannot demote yourself.
    if target.id == user.0.id {
        return Err(AppError::BadRequest("Cannot change your own role".into()));
    }

    // Cannot change someone at the same or higher level.
    if !db::teams::can_manage(&user.0.role, &target.role) {
        return Err(AppError::BadRequest("Insufficient privileges".into()));
    }

    db::teams::update_role(&state.db, id, new_role).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "team.role_change",
        &target.email,
        Some(new_role),
        true,
    )
    .await?;

    Ok(redirect_with_flash("/settings/users", "Role updated."))
}

// ---------------------------------------------------------------------------
// Remove user
// ---------------------------------------------------------------------------

pub async fn remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let target = db::teams::get_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    if target.id == user.0.id {
        return Err(AppError::BadRequest("Cannot remove yourself".into()));
    }

    if !db::teams::can_manage(&user.0.role, &target.role) {
        return Err(AppError::BadRequest("Insufficient privileges".into()));
    }

    db::teams::delete_user(&state.db, id).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "team.remove",
        &target.email,
        None,
        true,
    )
    .await?;

    Ok(redirect_with_flash("/settings/users", "User removed."))
}

// ---------------------------------------------------------------------------
// Revoke invitation
// ---------------------------------------------------------------------------

pub async fn revoke_invitation(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;
    db::teams::revoke_invitation(&state.db, id).await?;
    Ok(redirect_with_flash(
        "/settings/users",
        "Invitation revoked.",
    ))
}

// ---------------------------------------------------------------------------
// Site access grants
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GrantSiteForm {
    pub site_id: i64,
}

pub async fn grant_site(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<GrantSiteForm>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let target = db::teams::get_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    db::teams::grant_site_access(&state.db, form.site_id, target.id, "manage").await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "team.grant_site",
        &target.email,
        Some(&format!("site {}", form.site_id)),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        "/settings/users",
        "Site access granted.",
    ))
}

pub async fn revoke_site(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, site_id)): Path<(i64, i64)>,
) -> AppResult<Response> {
    require_admin_or_owner(&user)?;

    let target = db::teams::get_by_id(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    db::teams::revoke_site_access(&state.db, site_id, target.id).await?;
    db::audit::record(
        &state.db,
        &user.0.email,
        "team.revoke_site",
        &target.email,
        Some(&format!("site {site_id}")),
        true,
    )
    .await?;

    Ok(redirect_with_flash(
        "/settings/users",
        "Site access revoked.",
    ))
}

// ---------------------------------------------------------------------------
// Role middleware helper
// ---------------------------------------------------------------------------

/// Enforces that the current user is an owner or admin. Other roles get 403.
fn require_admin_or_owner(user: &CurrentUser) -> AppResult<()> {
    if db::teams::has_global_access(&user.0.role) {
        Ok(())
    } else {
        Err(AppError::Forbidden("Admin or owner role required".into()))
    }
}
