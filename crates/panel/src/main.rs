//! WP Panel control plane.
//!
//! Serves the UI and JSON API, owns the SQLite state, and runs the job workers
//! that drive node agents.

mod agent;
mod alerts;
mod api;
mod auth;
mod config;
mod csrf;
mod db;
mod error;
mod jobs;
mod scheduler;
mod secrets;
mod state;
mod web;

use crate::agent::AgentClient;
use crate::config::Config;
use crate::state::AppState;
use axum::http::{header, HeaderValue, StatusCode};
use axum::routing::{get, post};
use axum::{middleware, Router};
use clap::Parser;
use rand::RngCore;
use std::time::Duration;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("WP_PANEL_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=info")),
        )
        .with_target(false)
        // Colour codes only when a human is watching: log files stay greppable.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .compact()
        .init();

    let pool = db::connect(&config.database).await?;

    if let Some(password) =
        db::seed::bootstrap_admin(&pool, &config.admin_email, config.admin_password.as_deref())
            .await?
    {
        tracing::warn!(
            email = %config.admin_email,
            password = %password,
            "created the initial admin account - store this password now, it is not shown again"
        );
    }

    if config.demo_data {
        db::seed::demo_data(&pool).await?;
    }

    let orphans = db::jobs::requeue_orphans(&pool).await?;
    if orphans > 0 {
        tracing::warn!(orphans, "marked interrupted jobs as failed");
    }

    // Generate per-boot CSRF key from random bytes.
    let csrf_key = {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        csrf::CsrfKey(bytes)
    };

    // Load or generate the encryption key for destination secrets.
    let db_dir = config
        .database
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let secrets = secrets::SecretBox::load_or_generate(db_dir)?;

    let state = AppState::new(pool, config.clone(), AgentClient::new()?, csrf_key, secrets);
    jobs::spawn(state.clone(), config.workers);
    scheduler::spawn_scheduler(state.clone());
    tokio::spawn(alerts_loop(state.clone()));

    let app = router(state.clone(), &config);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(address = %config.bind, "wp-panel listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn router(state: AppState, config: &Config) -> Router {
    // Static assets are content-addressed by the `?v=` query the templates
    // append, so they can be cached hard.
    let static_service = ServeDir::new(&config.static_dir)
        .precompressed_br()
        .precompressed_gzip()
        .append_index_html_on_directories(false);

    let protected = web::routes()
        .layer(middleware::from_fn_with_state(
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
        .route("/login", get(web::pages::login_form).post(web::pages::login_submit))
        .route("/logout", post(web::pages::logout))
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

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutting down");
    // Give in-flight requests a moment to finish.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

async fn alerts_loop(state: AppState) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        alerts::evaluate(&state).await;
    }
}
