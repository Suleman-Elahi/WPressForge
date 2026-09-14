use super::{Chrome, FlashQuery, render};
use crate::auth::{CurrentSession, CurrentUser};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use wp_common::models::StepView;

#[derive(Template)]
#[template(path = "jobs/list.html")]
struct ListTemplate {
    chrome: Chrome,
    jobs: Vec<db::jobs::JobRow>,
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let jobs = db::jobs::list(&state.db, 100).await?;
    Ok(render(ListTemplate {
        chrome: Chrome::new(&state, &user, &session, "jobs", "Jobs", query.flash).await,
        jobs,
    }))
}

#[derive(Template)]
#[template(path = "jobs/detail.html")]
struct DetailTemplate {
    chrome: Chrome,
    job: db::jobs::JobRow,
    steps: Vec<StepView>,
    plan: &'static [&'static str],
}

pub async fn detail(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    Path(id): Path<i64>,
    Query(query): Query<FlashQuery>,
) -> AppResult<Response> {
    let job = db::jobs::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let steps = db::jobs::steps(&state.db, id).await?;

    Ok(render(DetailTemplate {
        chrome: Chrome::new(
            &state,
            &user,
            &session,
            "jobs",
            format!("Job #{id}"),
            query.flash,
        )
        .await,
        plan: crate::jobs::plan(job.job.kind),
        job,
        steps,
    }))
}

// ---------------------------------------------------------------------------
// Fragments
// ---------------------------------------------------------------------------

/// Polled by the sidebar badge and the dashboard banner (2s interval).
#[derive(Template)]
#[template(path = "jobs/active.html")]
struct ActiveFragment {
    jobs: Vec<db::jobs::JobRow>,
}

pub async fn active_fragment(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> AppResult<Response> {
    let jobs = db::jobs::list_active(&state.db, 5).await?;
    Ok(super::no_store(render(ActiveFragment { jobs })))
}

/// Polled by the job detail page; stops polling once the job is terminal.
#[derive(Template)]
#[template(path = "jobs/progress.html")]
struct ProgressFragment {
    job: db::jobs::JobRow,
    steps: Vec<StepView>,
    plan: &'static [&'static str],
}

pub async fn progress_fragment(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let job = db::jobs::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let steps = db::jobs::steps(&state.db, id).await?;
    Ok(super::no_store(render(ProgressFragment {
        plan: crate::jobs::plan(job.job.kind),
        job,
        steps,
    })))
}
