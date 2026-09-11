use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Runtime configuration. Every flag has an environment variable equivalent so
/// the panel can run from a systemd unit without arguments.
#[derive(Debug, Clone, Parser)]
#[command(name = "wp-panel", version, about = "WP Panel control plane")]
pub struct Config {
    /// Address the HTTP server binds to.
    #[arg(long, env = "WP_PANEL_BIND", default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    /// SQLite database file. Created if missing.
    #[arg(long, env = "WP_PANEL_DB", default_value = "data/panel.db")]
    pub database: PathBuf,

    /// Directory served at /static.
    #[arg(long, env = "WP_PANEL_STATIC", default_value = "static")]
    pub static_dir: PathBuf,

    /// Bootstrap admin account created on first run.
    #[arg(long, env = "WP_PANEL_ADMIN_EMAIL", default_value = "admin@localhost")]
    pub admin_email: String,

    /// Bootstrap admin password. A random one is generated and logged if unset.
    #[arg(long, env = "WP_PANEL_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    /// Insert an example server + sites on first run so the UI is explorable
    /// before any real node is attached.
    #[arg(long, env = "WP_PANEL_DEMO_DATA", default_value_t = true)]
    pub demo_data: bool,

    /// Number of concurrent job workers.
    #[arg(long, env = "WP_PANEL_WORKERS", default_value_t = 4)]
    pub workers: usize,

    /// Mark cookies as Secure. Turn off only for local plain-HTTP development.
    #[arg(long, env = "WP_PANEL_SECURE_COOKIES", default_value_t = false)]
    pub secure_cookies: bool,
}
