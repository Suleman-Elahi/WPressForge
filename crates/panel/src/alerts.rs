use crate::db;
use chrono::{Duration, Utc};
use sqlx::SqlitePool;
use wp_common::models::{ServerStatus, SiteStatus};

struct Rule {
    id: &'static str,
    severity: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        id: "disk.high",
        severity: "critical",
    },
    Rule {
        id: "ssl.expiring",
        severity: "warning",
    },
    Rule {
        id: "site.offline",
        severity: "critical",
    },
    Rule {
        id: "backup.stale",
        severity: "warning",
    },
];

pub async fn evaluate(state: &crate::state::AppState) {
    let db = &state.db;
    let now = Utc::now();

    // --- disk.high ---
    let servers = db::servers::list(db).await.unwrap_or_default();
    for row in &servers {
        if row.server.status == ServerStatus::Offline {
            continue;
        }
        if row.metrics.disk_percent > 85.0 {
            let target = &row.server.name;
            insert_if_absent(
                db,
                "disk.high",
                target,
                &format!(
                    "Disk usage on {} is {:.0}%",
                    target, row.metrics.disk_percent
                ),
            )
            .await;
        } else {
            resolve_rule(db, "disk.high", &row.server.name).await;
        }
    }

    // --- ssl.expiring ---
    let sites = db::sites::list_all(db).await.unwrap_or_default();
    for row in &sites {
        if let Some(expires) = &row.site.ssl.expires_at {
            let remaining = *expires - now;
            if remaining < Duration::days(14) && remaining > Duration::zero() {
                insert_if_absent(
                    db,
                    "ssl.expiring",
                    &row.site.domain,
                    &format!(
                        "TLS certificate for {} expires in {} days",
                        row.site.domain,
                        remaining.num_days()
                    ),
                )
                .await;
            } else {
                resolve_rule(db, "ssl.expiring", &row.site.domain).await;
            }
        }
    }

    // --- site.offline ---
    for row in &sites {
        if row.site.status == SiteStatus::Failed {
            insert_if_absent(
                db,
                "site.offline",
                &row.site.domain,
                &format!("Site {} is in failed state", row.site.domain),
            )
            .await;
        } else {
            resolve_rule(db, "site.offline", &row.site.domain).await;
        }
    }

    // --- backup.stale ---
    let schedules = db::schedules::list_active(db).await.unwrap_or_default();
    for schedule in &schedules {
        let last_backup = db::sites::last_backup_time(db, schedule.site_id)
            .await
            .ok()
            .flatten();
        let stale = match last_backup {
            Some(ts) => {
                let backup_time = db::parse_ts(&ts);
                (now - backup_time) > Duration::hours(48)
            }
            None => true,
        };
        if stale {
            if let Ok(domain) = db::sites::domain_by_id(db, schedule.site_id).await {
                insert_if_absent(
                    db,
                    "backup.stale",
                    &domain,
                    &format!("No backup for {domain} in over 48 hours"),
                )
                .await;
            }
        } else if let Ok(domain) = db::sites::domain_by_id(db, schedule.site_id).await {
            resolve_rule(db, "backup.stale", &domain).await;
        }
    }
}

async fn insert_if_absent(db: &SqlitePool, rule: &str, target: &str, message: &str) {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM notifications WHERE rule = ?1 AND target = ?2 AND resolved_at IS NULL LIMIT 1"
    )
    .bind(rule)
    .bind(target)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if exists.is_none() {
        let _ = sqlx::query(
            "INSERT INTO notifications (severity, rule, target, message, created_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
        )
        .bind(severity_for(rule))
        .bind(rule)
        .bind(target)
        .bind(message)
        .execute(db)
        .await;
    }
}

async fn resolve_rule(db: &SqlitePool, rule: &str, target: &str) {
    let _ = sqlx::query(
        "UPDATE notifications SET resolved_at = datetime('now')
         WHERE rule = ?1 AND target = ?2 AND resolved_at IS NULL",
    )
    .bind(rule)
    .bind(target)
    .execute(db)
    .await;
}

fn severity_for(rule: &str) -> &'static str {
    for r in RULES {
        if r.id == rule {
            return r.severity;
        }
    }
    "info"
}
