//! JSON API. Same data as the UI, for CLI tooling and automation.
//! Mounted under `/api/v1` behind [`crate::auth::require_api_auth`].

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

async fn servers(State(state): State<AppState>) -> AppResult<Json<Vec<ServerView>>> {
    let rows = db::servers::list(&state.db).await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| ServerView {
                server: row.server,
                metrics: row.metrics,
                site_count: row.site_count,
            })
            .collect(),
    ))
}

async fn server(State(state): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<ServerView>> {
    let row = db::servers::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(ServerView {
        server: row.server,
        metrics: row.metrics,
        site_count: row.site_count,
    }))
}

async fn sites(State(state): State<AppState>) -> AppResult<Json<Vec<SiteView>>> {
    let rows = db::sites::list(&state.db).await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| SiteView {
                site: row.site,
                server_name: row.server_name,
            })
            .collect(),
    ))
}

async fn site(State(state): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<SiteView>> {
    let row = db::sites::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(SiteView {
        site: row.site,
        server_name: row.server_name,
    }))
}

async fn jobs(State(state): State<AppState>) -> AppResult<Json<Vec<JobView>>> {
    let rows = db::jobs::list(&state.db, 100).await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| JobView {
                job: row.job,
                site_domain: row.site_domain,
                server_name: row.server_name,
                steps: Vec::new(),
            })
            .collect(),
    ))
}

async fn job(State(state): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<JobView>> {
    let row = db::jobs::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let steps = db::jobs::steps(&state.db, id).await?;
    Ok(Json(JobView {
        job: row.job,
        site_domain: row.site_domain,
        server_name: row.server_name,
        steps,
    }))
}
