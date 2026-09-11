//! First-run bootstrap: the admin account and (optionally) an example server
//! with a few sites so the UI can be evaluated before a node is attached.

use super::{servers, sites, users, Db};
use crate::auth;
use chrono::{Duration, Utc};
use wp_common::models::{
    BackupScope, CacheSettings, DatabaseMode, Environment, PhpUsage, PhpVersion, ResourceLimits,
    ServerMetrics, ServerStatus, ServiceHealth, SiteStatus, SslIssuer,
};

/// Creates the admin user if the `users` table is empty. Returns the generated
/// password when one had to be invented, so `main` can log it once.
pub async fn bootstrap_admin(
    db: &Db,
    email: &str,
    password: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if users::count(db).await? > 0 {
        return Ok(None);
    }

    let (password, generated) = match password {
        Some(p) => (p.to_string(), false),
        None => (auth::random_password(), true),
    };

    let hash = auth::hash_password(&password)?;
    users::create(db, email, "Administrator", &hash, "owner").await?;

    Ok(generated.then_some(password))
}

/// Inserts a demo server with three sites. Runs only when there are no servers.
pub async fn demo_data(db: &Db) -> anyhow::Result<()> {
    if servers::count(db).await? > 0 {
        return Ok(());
    }

    let server_id = servers::create(
        db,
        servers::NewServer {
            name: "server-01",
            agent_url: "https://127.0.0.1:8443",
            agent_token: "demo-token",
            hostname: "server-01.example.net",
            ip_address: "203.0.113.10",
            provider: Some("Hetzner"),
            region: Some("fsn1"),
        },
    )
    .await?;

    servers::record_heartbeat(
        db,
        server_id,
        ServerStatus::Online,
        Some("0.1.0"),
        Some(&ServerMetrics {
            cpu_percent: 32.0,
            memory_percent: 41.0,
            memory_total_mb: 16384,
            disk_percent: 58.0,
            disk_total_gb: 320,
            load_1m: 0.84,
            sites: 3,
            containers: 4,
            services: vec![
                ServiceHealth { name: "nginx".into(), healthy: true, detail: Some("1.27".into()) },
                ServiceHealth { name: "docker".into(), healthy: true, detail: Some("27.3".into()) },
                ServiceHealth { name: "mariadb".into(), healthy: true, detail: Some("11.4".into()) },
                ServiceHealth { name: "agent".into(), healthy: true, detail: Some("0.1.0".into()) },
            ],
            php_versions: vec![
                PhpUsage { version: PhpVersion::Php83, sites: 1 },
                PhpUsage { version: PhpVersion::Php84, sites: 2 },
            ],
        }),
    )
    .await?;

    let specs = [
        ("example.com", "Example Shop", PhpVersion::Php84, DatabaseMode::Dedicated, 2.0, 2048, SiteStatus::Online, true),
        ("blog.example.com", "Example Blog", PhpVersion::Php83, DatabaseMode::Shared, 1.0, 1024, SiteStatus::Online, true),
        ("staging.example.com", "Example Shop (staging)", PhpVersion::Php84, DatabaseMode::Shared, 1.0, 1024, SiteStatus::Stopped, false),
    ];

    for (domain, title, php, mode, cpu, mem, status, ssl) in specs {
        let environment = if domain.starts_with("staging.") {
            Environment::Staging
        } else {
            Environment::Production
        };

        let site_id = sites::create(
            db,
            sites::NewSite {
                server_id,
                domain,
                title: Some(title),
                php_version: php,
                database_mode: mode,
                environment,
                parent_site_id: None,
                limits: ResourceLimits {
                    cpu_cores: cpu,
                    memory_mb: mem,
                    php_workers: 8,
                },
                cache: CacheSettings::default(),
                request_ssl: ssl,
            },
        )
        .await?;

        sites::set_status(db, site_id, status).await?;
        sites::set_wp_version(db, site_id, "6.7.1").await?;
        if ssl {
            sites::set_ssl(
                db,
                site_id,
                true,
                SslIssuer::LetsEncrypt,
                Some(Utc::now() + Duration::days(74)),
            )
            .await?;
        }
        sites::record_backup(db, site_id, "a1b2c3d4", BackupScope::Full, 1_236_000_000).await?;
        sites::record_backup(db, site_id, "e5f6a7b8", BackupScope::Full, 1_198_000_000).await?;
    }

    Ok(())
}
