//! Web server abstraction.
//!
//! Nginx is the only backend today and the only one the panel exposes. The seam
//! exists because an OpenLiteSpeed backend is a plausible future option, and
//! because it keeps `site.rs` free of config-file details: site operations ask
//! for "write this site's config and reload", not for a specific directive
//! dialect.
//!
//! This is an enum rather than a trait object on purpose: `async fn` in traits
//! is not dyn-compatible, and enum dispatch needs no extra dependency and no
//! boxing. Adding a backend means adding a variant and matching on it; the
//! compiler then lists every place that needs attention.

use crate::capabilities::NginxCapabilities;
use crate::config::Config;
use crate::ops::nginx::NginxServer;
use crate::store::SiteRecord;
use std::sync::Arc;
use wp_common::Result;

#[derive(Clone)]
pub enum WebServer {
    Nginx(NginxServer),
    // Future: OpenLiteSpeed(OlsServer) — see docs/IMPLEMENTATION-PLAN.md §M8.
}

impl WebServer {
    /// Probes the host and builds the configured backend.
    pub async fn detect(config: Arc<Config>) -> Self {
        let caps = NginxCapabilities::probe().await;
        Self::Nginx(NginxServer::new(config, caps))
    }

    /// Backend name, reported to the panel in service health.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Nginx(_) => "nginx",
        }
    }

    /// Version of the backend as installed, for the server dashboard.
    pub fn version_label(&self) -> String {
        match self {
            Self::Nginx(server) => server.caps.version_label(),
        }
    }

    /// Writes (or rewrites) the site's configuration. Idempotent.
    pub async fn write_site(&self, site: &SiteRecord, ssl: bool) -> Result<()> {
        match self {
            Self::Nginx(server) => server.write_site(site, ssl).await,
        }
    }

    pub async fn remove_site(&self, domain: &str) -> Result<()> {
        match self {
            Self::Nginx(server) => server.remove_site(domain).await,
        }
    }

    /// Validates, then applies. Never reloads an invalid configuration.
    pub async fn reload(&self) -> Result<()> {
        match self {
            Self::Nginx(server) => server.reload().await,
        }
    }

    /// Drops the site's full-page cache.
    pub async fn purge(&self, domain: &str) -> Result<()> {
        match self {
            Self::Nginx(server) => server.purge(domain).await,
        }
    }
}
