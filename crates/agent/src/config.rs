use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Agent configuration. Runs as root (or with the required capabilities) from a
/// systemd unit; every value can come from the environment.
#[derive(Debug, Clone, Parser)]
#[command(name = "wp-agent", version, about = "WP Panel node agent")]
pub struct Config {
    /// Address the operations API binds to. Keep this on a private interface.
    #[arg(long, env = "WP_AGENT_BIND", default_value = "127.0.0.1:8443")]
    pub bind: SocketAddr,

    /// Shared secret the panel presents as a bearer token.
    #[arg(long, env = "WP_AGENT_TOKEN")]
    pub token: String,

    /// Root of the per-site directory tree.
    #[arg(long, env = "WP_AGENT_SITES_ROOT", default_value = "/var/www")]
    pub sites_root: PathBuf,

    /// Where the agent keeps its local index of managed sites.
    #[arg(long, env = "WP_AGENT_STATE", default_value = "/var/lib/wp-agent/state.json")]
    pub state_file: PathBuf,

    /// Nginx vhost directory.
    #[arg(long, env = "WP_AGENT_NGINX_DIR", default_value = "/etc/nginx/sites-enabled")]
    pub nginx_dir: PathBuf,

    /// First UID handed out to sites. Each site gets the next free one.
    #[arg(long, env = "WP_AGENT_UID_BASE", default_value_t = 10001)]
    pub uid_base: u32,

    /// Log every command that would run, but do not execute anything.
    /// Default on: a fresh install cannot damage a host by accident.
    #[arg(long, env = "WP_AGENT_DRY_RUN", default_value_t = true)]
    pub dry_run: bool,

    /// Restic repository, e.g. `s3:s3.amazonaws.com/wp-backups`.
    #[arg(long, env = "WP_AGENT_RESTIC_REPO")]
    pub restic_repo: Option<String>,

    /// Email used for Let's Encrypt registration.
    #[arg(long, env = "WP_AGENT_ACME_EMAIL")]
    pub acme_email: Option<String>,
}

impl Config {
    pub fn site_root(&self, domain: &str) -> PathBuf {
        self.sites_root.join(domain)
    }

    pub fn vhost_path(&self, domain: &str) -> PathBuf {
        self.nginx_dir.join(format!("{domain}.conf"))
    }
}
