//! User management: listing, invitations, role updates, per-site access.

use super::Db;
use chrono::{Duration, Utc};
use sqlx::{AssertSqlSafe, Row};

// ---------------------------------------------------------------------------
// Role definitions
// ---------------------------------------------------------------------------

/// Roles ordered by privilege level. Higher can manage lower.
pub const ROLES: &[&str] = &["owner", "admin", "operator", "viewer"];

pub fn role_level(role: &str) -> u8 {
    match role {
        "owner" => 0,
        "admin" => 1,
        "operator" => 2,
        "viewer" => 3,
        _ => 255,
    }
}

/// Returns true if this role can see all sites without an explicit grant.
pub fn has_global_access(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

/// Returns true if the role may perform state-changing requests at all.
/// `viewer` is read-only; every other known role may act, subject to per-site
/// grants.
pub fn can_mutate(role: &str) -> bool {
    role_level(role) <= role_level("operator")
}

/// Returns true if `actor_role` can modify users at `target_role` level.
pub fn can_manage(actor_role: &str, target_role: &str) -> bool {
    role_level(actor_role) < role_level(target_role)
}

// ---------------------------------------------------------------------------
// User listing
// ---------------------------------------------------------------------------

pub struct UserSummary {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: String,
    pub last_login_at: Option<String>,
    pub created_at: String,
}

pub async fn list_all(db: &Db) -> sqlx::Result<Vec<UserSummary>> {
    let rows = sqlx::query(
        "SELECT id, email, name, role, last_login_at, created_at \
         FROM users ORDER BY role, email",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|row| UserSummary {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            role: row.get("role"),
            last_login_at: row.get("last_login_at"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub async fn get_by_id(db: &Db, id: i64) -> sqlx::Result<Option<super::users::User>> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {} FROM users WHERE id = ?1",
        super::users::COLUMNS
    )))
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(super::users::map_row))
}

pub async fn update_role(db: &Db, user_id: i64, role: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET role = ?2 WHERE id = ?1")
        .bind(user_id)
        .bind(role)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_user(db: &Db, user_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM users WHERE id = ?1")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Site-user access grants
// ---------------------------------------------------------------------------

pub struct SiteAccess {
    pub site_id: i64,
    pub domain: String,
    pub access: String,
}

pub async fn site_grants_for_user(db: &Db, user_id: i64) -> sqlx::Result<Vec<SiteAccess>> {
    let rows = sqlx::query(
        "SELECT su.site_id, s.domain, su.access \
         FROM site_users su JOIN sites s ON s.id = su.site_id \
         WHERE su.user_id = ?1 ORDER BY s.domain",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SiteAccess {
            site_id: r.get("site_id"),
            domain: r.get("domain"),
            access: r.get("access"),
        })
        .collect())
}

pub async fn grant_site_access(
    db: &Db,
    site_id: i64,
    user_id: i64,
    access: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO site_users (site_id, user_id, access, created_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(site_id, user_id) DO UPDATE SET access = ?3",
    )
    .bind(site_id)
    .bind(user_id)
    .bind(access)
    .bind(super::now_string())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn revoke_site_access(db: &Db, site_id: i64, user_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM site_users WHERE site_id = ?1 AND user_id = ?2")
        .bind(site_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Returns true if the given user can access the given site, considering their
/// role and any explicit grants.
pub async fn can_access_site(
    db: &Db,
    user_id: i64,
    user_role: &str,
    site_id: i64,
) -> sqlx::Result<bool> {
    if has_global_access(user_role) {
        return Ok(true);
    }
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM site_users WHERE site_id = ?1 AND user_id = ?2")
            .bind(site_id)
            .bind(user_id)
            .fetch_one(db)
            .await?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

pub struct Invitation {
    pub id: i64,
    pub email: String,
    pub role: String,
    pub token: String,
    pub invited_by_email: String,
    pub expires_at: String,
    pub used_at: Option<String>,
    pub created_at: String,
}

pub async fn create_invitation(
    db: &Db,
    email: &str,
    role: &str,
    token: &str,
    invited_by: i64,
    ttl_days: i64,
) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO invitation_tokens (email, role, token, invited_by, expires_at, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
    )
    .bind(email)
    .bind(role)
    .bind(token)
    .bind(invited_by)
    .bind((Utc::now() + Duration::days(ttl_days)).to_rfc3339())
    .bind(super::now_string())
    .fetch_one(db)
    .await?;
    Ok(row.get("id"))
}

pub async fn list_invitations(db: &Db) -> sqlx::Result<Vec<Invitation>> {
    let rows = sqlx::query(
        "SELECT it.id, it.email, it.role, it.token, u.email AS invited_by_email, \
                it.expires_at, it.used_at, it.created_at \
         FROM invitation_tokens it JOIN users u ON u.id = it.invited_by \
         ORDER BY it.created_at DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Invitation {
            id: r.get("id"),
            email: r.get("email"),
            role: r.get("role"),
            token: r.get("token"),
            invited_by_email: r.get("invited_by_email"),
            expires_at: r.get("expires_at"),
            used_at: r.get("used_at"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn get_invitation_by_token(db: &Db, token: &str) -> sqlx::Result<Option<Invitation>> {
    let row = sqlx::query(
        "SELECT it.id, it.email, it.role, it.token, u.email AS invited_by_email, \
                it.expires_at, it.used_at, it.created_at \
         FROM invitation_tokens it JOIN users u ON u.id = it.invited_by \
         WHERE it.token = ?1 AND it.used_at IS NULL AND it.expires_at > ?2",
    )
    .bind(token)
    .bind(super::now_string())
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(|r| Invitation {
        id: r.get("id"),
        email: r.get("email"),
        role: r.get("role"),
        token: r.get("token"),
        invited_by_email: r.get("invited_by_email"),
        expires_at: r.get("expires_at"),
        used_at: r.get("used_at"),
        created_at: r.get("created_at"),
    }))
}

pub async fn mark_invitation_used(db: &Db, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE invitation_tokens SET used_at = ?2 WHERE id = ?1")
        .bind(id)
        .bind(super::now_string())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn revoke_invitation(db: &Db, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM invitation_tokens WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
