//! WP Panel node agent.
//!
//! Runs on every managed server and owns all privileged work: Docker, Nginx,
//! MariaDB, filesystem, TLS, Restic and WP-CLI. It accepts typed operations
//! from the panel over an authenticated HTTP API and never evaluates shell
//! strings supplied by the caller.

mod api;
mod config;
mod exec;
mod ops;
mod state;
mod store;

use crate::config::Config;
use crate::state::AgentState;
use crate::store::Store;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("WP_AGENT_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        // Colour codes only when a human is watching: log files stay greppable.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .compact()
        .init();

    if config.token.len() < 24 {
        anyhow::bail!("WP_AGENT_TOKEN must be at least 24 characters");
    }

    if config.dry_run {
        tracing::warn!(
            "dry-run mode: commands are logged but not executed. \
             Set WP_AGENT_DRY_RUN=false once the host is ready."
        );
    }

    let store = Store::open(&config.state_file)
        .await
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", config.state_file.display()))?;
    let state = AgentState::new(config.clone(), store);

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(
        address = %config.bind,
        sites_root = %config.sites_root.display(),
        "wp-agent listening"
    );

    axum::serve(listener, api::router(state))
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

    tracing::info!("wp-agent shutting down");
}
