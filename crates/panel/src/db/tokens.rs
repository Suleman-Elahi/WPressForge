//! API token management. Tokens are bearer credentials that allow external
//! tools (CI, WP-CLI plugins) to authenticate against the panel API.
//!
//! The plaintext is shown once on creation; only the SHA-256 hash is stored.

use super::Db;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;

#[derive(Debug, Clone)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub user_id: i64,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Hash a plaintext token for storage.
pub fn hash_token(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    format!("{:x}", digest)
}

/// Create a new API token. Returns `(id, plaintext)`.
pub async fn create(db: &Db, user_id: i64, name: &str) -> sqlx::Result<(i64, String)> {
    let plaintext = format!("wpp_{}", super::super::auth::random_token());
    let hash = hash_token(&plaintext);

    let row = sqlx::query(
        "INSERT INTO api_tokens (user_id, name, token_hash, created_at)
         VALUES (?1, ?2, ?3, ?4) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(&hash)
    .bind(super::now_string())
    .fetch_one(db)
    .await?;

    Ok((row.get("id"), plaintext))
}

/// List tokens for a user (hashes not exposed).
pub async fn list(db: &Db, user_id: i64) -> sqlx::Result<Vec<ApiToken>> {
    let rows = sqlx::query(
        "SELECT id, name, user_id, created_at, last_used_at
         FROM api_tokens WHERE user_id = ?1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|row| ApiToken {
            id: row.get("id"),
            name: row.get("name"),
            user_id: row.get("user_id"),
            created_at: super::parse_ts(row.get::<String, _>("created_at").as_str()),
            last_used_at: row
                .try_get::<Option<String>, _>("last_used_at")
                .ok()
                .flatten()
                .map(|s| super::parse_ts(&s)),
        })
        .collect())
}

/// Revoke (delete) a token.
pub async fn revoke(db: &Db, user_id: i64, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM api_tokens WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Look up the user for a presented plaintext token.
/// On match, updates `last_used_at` (fire-and-forget).
pub async fn user_for_token(db: &Db, presented: &str) -> sqlx::Result<Option<super::users::User>> {
    let hash = hash_token(presented);
    let row = sqlx::query(
        "SELECT u.id, u.email, u.password_hash, u.role, u.totp_secret,
                u.totp_confirmed_at, u.created_at, u.last_login_at
         FROM api_tokens t
         JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = ?1",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await?;

    match row {
        Some(row) => {
            // Fire-and-forget: update last_used_at.
            let _ = sqlx::query("UPDATE api_tokens SET last_used_at = ?1 WHERE token_hash = ?2")
                .bind(super::now_string())
                .bind(&hash)
                .execute(db)
                .await;

            Ok(Some(super::users::User {
                id: row.get("id"),
                email: row.get("email"),
                name: row.try_get("name").unwrap_or_default(),
                password_hash: row.get("password_hash"),
                role: row.get("role"),
                totp_secret: row.try_get("totp_secret").ok().flatten(),
                totp_confirmed_at: row
                    .try_get::<Option<String>, _>("totp_confirmed_at")
                    .ok()
                    .flatten()
                    .map(|s| super::parse_ts(&s)),
                created_at: super::parse_ts(row.get::<String, _>("created_at").as_str()),
                last_login_at: row
                    .try_get::<Option<String>, _>("last_login_at")
                    .ok()
                    .flatten()
                    .map(|s| super::parse_ts(&s)),
            }))
        }
        None => Ok(None),
    }
}
