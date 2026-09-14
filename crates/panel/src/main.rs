//! WP Panel control plane.
//!
//! Serves the UI and JSON API, owns the SQLite state, and runs the job workers
//! that drive node agents.

#![allow(clippy::collapsible_if, clippy::clone_on_copy, dead_code)]

use clap::Parser;
use rand::RngCore;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use wp_panel::agent::AgentClient;
use wp_panel::config::Config;
use wp_panel::state::AppState;
use wp_panel::{alerts_loop, csrf, db, jobs, router, scheduler, secrets};

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
