use super::{parse_ts, parse_ts_opt, Db};
use chrono::{DateTime, Duration, Utc};
use sqlx::{AssertSqlSafe, Row};

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub role: String,
    pub totp_secret: Option<String>,
    pub totp_confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl User {
    /// Initials shown in the top-right avatar.
    pub fn initials(&self) -> String {
        let source = if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        };
        source
            .split(|c: char| c.is_whitespace() || c == '.' || c == '@')
            .filter(|p| !p.is_empty())
            .take(2)
            .filter_map(|p| p.chars().next())
            .map(|c| c.to_ascii_uppercase())
            .collect()
    }

    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

fn map(row: &sqlx::sqlite::SqliteRow) -> User {
    User {
        id: row.get("id"),
        email: row.get("email"),
        name: row.get("name"),
        password_hash: row.get("password_hash"),
        role: row.get("role"),
        totp_secret: row.try_get("totp_secret").ok().flatten(),
        totp_confirmed_at: row
            .try_get::<Option<String>, _>("totp_confirmed_at")
            .ok()
            .flatten()
            .map(|s| parse_ts(&s)),
        created_at: parse_ts(row.get::<String, _>("created_at").as_str()),
        last_login_at: parse_ts_opt(row.get("last_login_at")),
    }
}

const COLUMNS: &str =
    "id, email, name, password_hash, role, totp_secret, totp_confirmed_at, created_at, last_login_at";

pub async fn count(db: &Db) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(db).await
}

pub async fn by_email(db: &Db, email: &str) -> sqlx::Result<Option<User>> {
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {COLUMNS} FROM users WHERE email = ?1")))
        .bind(email)
        .fetch_optional(db)
        .await?;
    Ok(row.as_ref().map(map))
}

pub async fn create(
    db: &Db,
    email: &str,
    name: &str,
    password_hash: &str,
    role: &str,
) -> sqlx::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO users (email, name, password_hash, role, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
    )
    .bind(email)
    .bind(name)
    .bind(password_hash)
    .bind(role)
    .bind(super::now_string())
    .fetch_one(db)
    .await?;
    Ok(row.get("id"))
}

pub async fn touch_login(db: &Db, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET last_login_at = ?2 WHERE id = ?1")
        .bind(id)
        .bind(super::now_string())
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub async fn create_session(
    db: &Db,
    token: &str,
    user_id: i64,
    user_agent: &str,
    ip: &str,
    ttl: Duration,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO sessions (token, user_id, user_agent, ip, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(token)
    .bind(user_id)
    .bind(user_agent)
    .bind(ip)
    .bind(super::now_string())
    .bind((Utc::now() + ttl).to_rfc3339())
    .execute(db)
    .await?;
    Ok(())
}

/// Resolves a session cookie to a user, ignoring expired rows.
pub async fn user_for_session(db: &Db, token: &str) -> sqlx::Result<Option<User>> {
    let row = sqlx::query(
        "SELECT u.id, u.email, u.name, u.password_hash, u.role, u.created_at, u.last_login_at
         FROM users u
         JOIN sessions s ON s.user_id = u.id
         WHERE s.token = ?1 AND s.expires_at > ?2",
    )
    .bind(token)
    .bind(super::now_string())
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(map))
}

pub async fn delete_session(db: &Db, token: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?1")
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn purge_expired_sessions(db: &Db) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?1")
        .bind(super::now_string())
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}
