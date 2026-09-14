//! Backup destination management routes.

use crate::auth::{CurrentSession, CurrentUser};
use crate::db;
use crate::error::AppError;
use crate::state::AppState;
use axum::extract::{Form, Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use askama::Template;
use serde::Deserialize;

use super::{redirect_with_flash, render, Chrome, FlashQuery};

#[derive(Template)]
#[template(path = "settings/destinations.html")]
struct DestinationsPage {
    chrome: Chrome,
    destinations: Vec<db::destinations::Destination>,
}

#[derive(Template)]
#[template(path = "settings/destination_new.html")]
struct DestinationNewPage {
    chrome: Chrome,
}

#[derive(Deserialize)]
pub struct DestinationForm {
    pub name: String,
    pub provider: String,
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub access_key_id: String,
    pub secret: String,
    pub restic_password: String,
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
    axum::extract::Query(query): axum::extract::Query<FlashQuery>,
) -> Result<Response, AppError> {
    let destinations = db::destinations::list(&state.db).await?;
    let chrome = Chrome::new(&state, &user, &session, "settings", "Backup Destinations", query.flash).await;
    Ok(render(DestinationsPage { chrome, destinations }))
}

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
    session: CurrentSession,
) -> Result<Response, AppError> {
    let chrome = Chrome::new(&state, &user, &session, "settings", "New Destination", None).await;
    Ok(render(DestinationNewPage { chrome }))
}

pub async fn create(
    State(state): State<AppState>,
    _user: CurrentUser,
    _session: CurrentSession,
    Form(form): Form<DestinationForm>,
) -> Result<Response, AppError> {
    let db_form = db::destinations::DestinationForm {
        name: form.name,
        provider: form.provider,
        bucket: form.bucket,
        region: form.region,
        endpoint: form.endpoint,
        access_key_id: form.access_key_id,
        secret: form.secret,
        restic_password: form.restic_password,
    };
    db::destinations::create(&state.db, db_form, &state.secrets).await?;
    Ok(redirect_with_flash("/settings/destinations", "Destination created"))
}

pub async fn delete(
    State(state): State<AppState>,
    _user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    db::destinations::delete(&state.db, id).await?;
    Ok(redirect_with_flash("/settings/destinations", "Destination deleted"))
}

pub async fn test(
    State(state): State<AppState>,
    _user: CurrentUser,
    _session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let dest = db::destinations::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let creds = db::destinations::decrypt_credentials(&dest, &state.secrets)?;

    // Build a ResticTarget and try to init the repo.
    let target = wp_common::protocol::ResticTarget {
        repo: format!(
            "s3:{}/{}",
            dest.endpoint.as_deref().unwrap_or("s3.amazonaws.com"),
            dest.bucket
        ),
        password: creds.restic_password,
        env: vec![
            ("AWS_ACCESS_KEY_ID".into(), creds.access_key_id),
            ("AWS_SECRET_ACCESS_KEY".into(), creds.secret),
        ],
    };

    // Use the first online server for the test.
    let servers = db::servers::list(&state.db).await?;
    let server = servers.iter().find(|s| s.server.status == wp_common::models::ServerStatus::Online).ok_or(AppError::NotFound)?;

    match state.agent.query(&server.connection(), wp_common::protocol::Operation::InitBackupRepo { target }).await {
        Ok(_) => Ok(redirect_with_flash("/settings/destinations", "Repository connection successful")),
        Err(e) => Ok(redirect_with_flash("/settings/destinations", &format!("Test failed: {e}"))),
    }
}
