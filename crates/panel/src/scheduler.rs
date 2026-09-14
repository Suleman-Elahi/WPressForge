//! Backup scheduler. Checks every minute for due schedules and enqueues
//! backup.create jobs.

use crate::db;
use crate::state::AppState;
use chrono::Utc;
use serde_json::json;
use wp_common::models::JobKind;

pub async fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        scheduler_loop(state).await;
    });
}

async fn scheduler_loop(state: AppState) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        ticker.tick().await;

        if let Err(e) = tick(&state).await {
            tracing::warn!(error = %e, "scheduler tick failed");
        }
    }
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let due = db::schedules::due(&state.db, &now).await?;

    for schedule in due {
        // Skip if a backup job for this site is already queued or running.
        if db::jobs::has_active(&state.db, schedule.site_id, JobKind::BackupCreate).await? {
            continue;
        }

        // Resolve the server for this site.
        let site = match db::sites::get(&state.db, schedule.site_id).await? {
            Some(s) => s,
            None => continue,
        };

        let server_id = Some(site.site.server_id);

        let payload = json!({
            "scope": schedule.scope,
            "destination_id": schedule.destination_id,
        });

        let _job = db::jobs::enqueue(
            &state.db,
            JobKind::BackupCreate,
            server_id,
            Some(schedule.site_id),
            Some(payload),
            "scheduler",
        )
        .await?;

        // Compute next run time.
        let next_run = Utc::now() + chrono::Duration::minutes(schedule.interval_minutes);

        db::schedules::mark_scheduled(
            &state.db,
            schedule.id,
            &Utc::now().to_rfc3339(),
            &next_run.to_rfc3339(),
        )
        .await?;

        tracing::info!(
            schedule_id = schedule.id,
            site_id = schedule.site_id,
            "enqueued scheduled backup"
        );
    }

    Ok(())
}
