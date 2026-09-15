//! JSON API. Same data as the UI, for CLI tooling and automation.
//! Mounted under `/api/v1` behind [`crate::auth::require_api_auth`].
//!
//! Every handler scopes its query to the caller, exactly like the HTML routes.
//! An earlier version queried the tables unscoped, so a `viewer` (or any API
//! token) could enumerate every site and server in the panel.

use crate::auth::CurrentUser;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use wp_common::models::{Job, Server, ServerMetrics, Site, StepView};

#[derive(Serialize)]
pub struct ServerView {
    #[serde(flatten)]
    pub server: Server,
    pub metrics: ServerMetrics,
    pub site_count: i64,
}

#[derive(Serialize)]
pub struct SiteView {
    #[serde(flatten)]
    pub site: Site,
    pub server_name: String,
}

#[derive(Serialize)]
pub struct JobView {
    #[serde(flatten)]
    pub job: Job,
    pub site_domain: Option<String>,
    pub server_name: Option<String>,
    pub steps: Vec<StepView>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/servers", get(servers))
        .route("/servers/{id}", get(server))
        .route("/sites", get(sites))
        .route("/sites/{id}", get(site))
        .route("/jobs", get(jobs))
        .route("/jobs/{id}", get(job))
}

/// Site ids the caller may see. `None` means "no restriction" (owner/admin).
async fn visible_site_ids(
    state: &AppState,
    user: &CurrentUser,
) -> AppResult<Option<std::collections::HashSet<i64>>> {
    if db::teams::has_global_access(&user.0.role) {
        return Ok(None);
    }

    let ids = db::sites::list_for_user(&state.db, user.0.id, &user.0.role)
        .await?
        .into_iter()
        .map(|row| row.site.id)
        .collect();

    Ok(Some(ids))
}

async fn servers(
    State(state): State<AppState>,
    user: CurrentUser,
) -> AppResult<Json<Vec<ServerView>>> {
    // Servers are infrastructure: only owners and admins see them. Scoped users
    // get the servers that host sites they can see, with no counts they could
    // use to infer other tenants' footprint.
    let rows = db::servers::list(&state.db).await?;
    let visible = visible_site_ids(&state, &user).await?;

    let allowed_servers: Option<std::collections::HashSet<i64>> = match &visible {
        None => None,
        Some(ids) => {
            let mut servers = std::collections::HashSet::new();
            for id in ids {
                if let Some(site) = db::sites::get(&state.db, *id).await? {
                    servers.insert(site.site.server_id);
                }
            }
            Some(servers)
        }
    };

    Ok(Json(
        rows.into_iter()
            .filter(|row| {
                allowed_servers
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(&row.server.id))
            })
            .map(|row| ServerView {
                server: row.server,
                metrics: row.metrics,
                site_count: row.site_count,
            })
            .collect(),
    ))
}

async fn server(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Json<ServerView>> {
    if !db::teams::has_global_access(&user.0.role) {
        let sites = db::sites::list_for_user(&state.db, user.0.id, &user.0.role).await?;
        if !sites.iter().any(|row| row.site.server_id == id) {
            return Err(AppError::NotFound);
        }
    }

    let row = db::servers::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(ServerView {
        server: row.server,
        metrics: row.metrics,
        site_count: row.site_count,
    }))
}

async fn sites(State(state): State<AppState>, user: CurrentUser) -> AppResult<Json<Vec<SiteView>>> {
    let rows = db::sites::list_for_user(&state.db, user.0.id, &user.0.role).await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| SiteView {
                site: row.site,
                server_name: row.server_name,
            })
            .collect(),
    ))
}

async fn site(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Json<SiteView>> {
    let row = db::sites::get_for_user(&state.db, id, user.0.id, &user.0.role)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(SiteView {
        site: row.site,
        server_name: row.server_name,
    }))
}

async fn jobs(State(state): State<AppState>, user: CurrentUser) -> AppResult<Json<Vec<JobView>>> {
    let visible = visible_site_ids(&state, &user).await?;
    let rows = db::jobs::list(&state.db, 100).await?;

    Ok(Json(
        rows.into_iter()
            .filter(|row| match (&visible, row.job.site_id) {
                (None, _) => true,
                // Scoped users see jobs for their sites only; jobs with no site
                // (server-level work) stay hidden from them.
                (Some(ids), Some(site_id)) => ids.contains(&site_id),
                (Some(_), None) => false,
            })
            .map(|row| JobView {
                job: row.job,
                site_domain: row.site_domain,
                server_name: row.server_name,
                steps: Vec::new(),
            })
            .collect(),
    ))
}

async fn job(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Json<JobView>> {
    let row = db::jobs::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;

    if !db::teams::has_global_access(&user.0.role) {
        let Some(site_id) = row.job.site_id else {
            return Err(AppError::NotFound);
        };
        if db::sites::get_for_user(&state.db, site_id, user.0.id, &user.0.role)
            .await?
            .is_none()
        {
            return Err(AppError::NotFound);
        }
    }

    let steps = db::jobs::steps(&state.db, id).await?;

    Ok(Json(JobView {
        job: row.job,
        site_domain: row.site_domain,
        server_name: row.server_name,
        steps,
    }))
}
