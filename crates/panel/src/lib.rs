//! WP Panel control plane library.

pub mod agent;
pub mod alerts;
pub mod api;
pub mod auth;
pub mod config;
pub mod credentials;
pub mod csrf;
pub mod db;
pub mod email;
pub mod error;
pub mod jobs;
pub mod scheduler;
pub mod secrets;
pub mod state;
pub mod totp;
pub mod web;

use crate::config::Config;
use crate::state::AppState;
use axum::http::{HeaderValue, StatusCode, header};
use axum::routing::{get, post};
use axum::{Router, middleware};
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState, config: &Config) -> Router {
    let static_service = ServeDir::new(&config.static_dir)
        .precompressed_br()
        .precompressed_gzip()
        .append_index_html_on_directories(false);

    let protected = web::routes().layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_session,
    ));

    let api = api::routes().layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_api_auth,
    ));

    Router::new()
        .merge(protected)
        .nest("/api/v1", api)
        .route(
            "/login",
            get(web::pages::login_form).post(web::pages::login_submit),
        )
        .route("/logout", post(web::pages::logout))
        .route(
            "/register",
            get(web::users::register_form).post(web::users::register_submit),
        )
        .route("/healthz", get(healthz))
        .nest_service("/static", static_service)
        .fallback(not_found)
        .layer(CompressionLayer::new().br(true).gzip(true))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found() -> (StatusCode, axum::response::Html<String>) {
    (
        StatusCode::NOT_FOUND,
        web::render_error(StatusCode::NOT_FOUND, "That page does not exist."),
    )
}

pub async fn alerts_loop(state: AppState) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        ticker.tick().await;
        alerts::evaluate(&state).await;
    }
}
