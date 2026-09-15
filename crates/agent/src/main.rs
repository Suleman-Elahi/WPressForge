//! WP Panel node agent.
//!
//! Runs on every managed server and owns all privileged work: Docker, Nginx,
//! MariaDB, filesystem, TLS, Restic and WP-CLI. It accepts typed operations
//! from the panel over an authenticated HTTP API and never evaluates shell
//! strings supplied by the caller.

mod api;
mod capabilities;
mod config;
mod exec;
mod ops;
mod state;
mod store;
mod tls;

use crate::config::Config;
use crate::state::AgentState;
use crate::store::Store;
use anyhow::Context;
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

    // rustls has no process-wide default provider unless exactly one backend
    // feature is enabled anywhere in the dependency graph. Install it explicitly
    // so a transitive dependency enabling a second backend cannot turn TLS
    // startup into a panic.
    install_crypto_provider();

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
    let state = AgentState::new(config.clone(), store).await;

    // Load or generate TLS certificate.
    let tls = tls::load_or_generate(
        config.tls_cert.as_deref(),
        config.tls_key.as_deref(),
        config
            .state_file
            .parent()
            .unwrap_or(std::path::Path::new("/var/lib/wp-agent")),
        &hostname(),
        &local_ip(),
    )?;

    let rustls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem(tls.cert_pem.clone(), tls.key_pem.clone())
            .await
            .context("building rustls config from loaded certificate")?;

    tracing::info!(
        address = %config.bind,
        sites_root = %config.sites_root.display(),
        fingerprint = %tls.fingerprint,
        "wp-agent listening (TLS)"
    );

    let server = axum_server::bind_rustls(config.bind, rustls_config);
    let shutdown = shutdown_signal();

    tokio::select! {
        result = server.serve(api::router(state).into_make_service()) => {
            result.context("axum-server")?;
        }
        _ = shutdown => {
            tracing::info!("wp-agent shutting down");
        }
    }

    Ok(())
}

/// Installs the ring-based rustls provider. Idempotent: a second call is a no-op.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
    {
        tracing::debug!("a rustls crypto provider was already installed");
    }
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

/// Returns the system hostname.
fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".to_string())
}

/// Returns the first non-loopback IPv4 address, or 127.0.0.1.
fn local_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}
