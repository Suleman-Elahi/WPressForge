//! Nginx vhost generation. Nginx stays on the host and proxies to the site's
//! PHP-FPM socket; the cache is WordPress-aware.
//!
//! Everything rendered here is gated on [`NginxCapabilities`], probed once at
//! startup. Generating a directive the installed binary does not understand
//! makes `nginx -t` fail, which fails the reload, which fails the job.

use crate::capabilities::NginxCapabilities;
use crate::config::Config;
use crate::exec;
use crate::store::SiteRecord;
use std::sync::Arc;
use wp_common::Result;

/// Nginx as a web server backend. Holds the host's capabilities so callers do
/// not have to thread them through every call.
#[derive(Clone)]
pub struct NginxServer {
    pub config: Arc<Config>,
    pub caps: NginxCapabilities,
}

impl NginxServer {
    pub fn new(config: Arc<Config>, caps: NginxCapabilities) -> Self {
        Self { config, caps }
    }

    /// Writes the vhost (and the site's cache zone include when caching is on).
    pub async fn write_site(&self, site: &SiteRecord, ssl: bool) -> Result<()> {
        let path = self.config.vhost_path(&site.domain);
        let contents = render_vhost(&self.config, &self.caps, site, ssl);

        if site.cache.fastcgi_cache {
            self.write_cache_zone(site).await?;
        }

        if self.config.dry_run {
            tracing::info!(
                path = %path.display(),
                bytes = contents.len(),
                nginx = %self.caps.version_label(),
                "dry-run: would write vhost"
            );
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(wp_common::Error::internal)?;
        }
        tokio::fs::write(&path, contents)
            .await
            .map_err(wp_common::Error::internal)?;
        Ok(())
    }

    pub async fn remove_site(&self, domain: &str) -> Result<()> {
        let vhost = self.config.vhost_path(domain);
        let zone = self.cache_zone_path(domain);

        if self.config.dry_run {
            tracing::info!(path = %vhost.display(), "dry-run: would remove vhost");
            return Ok(());
        }

        let _ = tokio::fs::remove_file(vhost).await;
        let _ = tokio::fs::remove_file(zone).await;
        Ok(())
    }

    /// `nginx -t` before every reload: a bad config never reaches production.
    pub async fn test(&self) -> Result<()> {
        exec::run(self.config.dry_run, "nginx", &["-t"]).await.map(|_| ())
    }

    pub async fn reload(&self) -> Result<()> {
        self.test().await?;
        exec::run(self.config.dry_run, "systemctl", &["reload", "nginx"])
            .await
            .map(|_| ())
    }

    /// Purges the FastCGI cache directory for one site.
    pub async fn purge(&self, domain: &str) -> Result<()> {
        let path = self.config.cache_dir(domain).display().to_string();
        exec::run(
            self.config.dry_run,
            "find",
            &[&path, "-type", "f", "-delete"],
        )
        .await
        .map(|_| ())
    }

    async fn write_cache_zone(&self, site: &SiteRecord) -> Result<()> {
        let path = self.cache_zone_path(&site.domain);

        if self.config.dry_run {
            tracing::info!(path = %path.display(), "dry-run: would write cache zone");
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(wp_common::Error::internal)?;
        }
        tokio::fs::write(&path, render_cache_zone(&self.config, site))
            .await
            .map_err(wp_common::Error::internal)?;
        Ok(())
    }

    fn cache_zone_path(&self, domain: &str) -> std::path::PathBuf {
        self.config
            .nginx_dir
            .parent()
            .unwrap_or(&self.config.nginx_dir)
            .join("conf.d")
            .join(format!("wp-cache-{domain}.conf"))
    }
}

/// `fastcgi_cache_path` must live in the http context, so each site gets a small
/// include next to its vhost declaring its own cache zone.
pub fn render_cache_zone(config: &Config, site: &SiteRecord) -> String {
    let zone = site.domain.replace('.', "_");
    let dir = config.cache_dir(&site.domain).display().to_string();
    format!(
        "# Managed by wp-panel.\n\
         fastcgi_cache_path {dir} levels=1:2 keys_zone={zone}:32m \
         inactive=1h max_size=512m use_temp_path=off;\n\
         fastcgi_cache_key \"$scheme$request_method$host$request_uri\";\n"
    )
}

/// Renders the vhost for a site. Pure function: unit tested against the Nginx
/// versions we support, and diffable before anything is written.
pub fn render_vhost(
    config: &Config,
    caps: &NginxCapabilities,
    site: &SiteRecord,
    ssl: bool,
) -> String {
    let root = config.site_root(&site.domain);
    let public = root.join("public_html").display().to_string();
    let logs = root.join("logs").display().to_string();
    let socket = root.join("tmp/php-fpm.sock").display().to_string();
    let server_names = if site.domains.is_empty() {
        site.domain.clone()
    } else {
        site.domains.join(" ")
    };
    let cache_zone = site.domain.replace('.', "_");

    let cache_block = if site.cache.fastcgi_cache {
        format!(
            r#"
    # ---- FastCGI cache (WordPress aware) --------------------------------
    set $skip_cache 0;
    if ($request_method = POST) {{ set $skip_cache 1; }}
    if ($query_string != "") {{ set $skip_cache 1; }}
    if ($request_uri ~* "/wp-admin/|/wp-json/|/xmlrpc.php|wp-.*\.php|/feed/|sitemap(_index)?\.xml") {{ set $skip_cache 1; }}
    if ($http_cookie ~* "comment_author|wordpress_[a-f0-9]+|wp-postpass|wordpress_logged_in|woocommerce_items_in_cart|woocommerce_cart_hash") {{ set $skip_cache 1; }}

    fastcgi_cache {cache_zone};
    fastcgi_cache_valid 200 301 302 {ttl}s;
    fastcgi_cache_bypass $skip_cache;
    fastcgi_no_cache $skip_cache;
    fastcgi_cache_use_stale error timeout updating http_500 http_503;
    fastcgi_cache_background_update on;
    fastcgi_cache_lock on;
    add_header X-Cache $upstream_cache_status;
"#,
            ttl = site.cache.ttl_seconds
        )
    } else {
        "    set $skip_cache 1;\n".to_string()
    };

    let tls_block = if ssl {
        // `http2 on;` only exists from 1.25.1. On older binaries the only
        // accepted form is the `listen` parameter, which newer versions still
        // accept (with a deprecation warning).
        let (listen_lines, http2_line) = if caps.http2_directive {
            (
                "    listen 443 ssl;\n    listen [::]:443 ssl;\n".to_string(),
                "    http2 on;\n".to_string(),
            )
        } else {
            (
                "    listen 443 ssl http2;\n    listen [::]:443 ssl http2;\n".to_string(),
                String::new(),
            )
        };

        // HTTP/3 needs --with-http_v3_module. `reuseport` is deliberately
        // omitted: it may appear only once per address:port, and every site
        // gets its own server block.
        let quic_lines = if caps.http3 {
            format!(
                "    listen 443 quic;\n    listen [::]:443 quic;\n    \
                 http3 on;\n    add_header Alt-Svc 'h3=\":443\"; ma=86400' always;\n"
            )
        } else {
            String::new()
        };

        format!(
            "{listen_lines}{http2_line}{quic_lines}    \
             ssl_certificate     /etc/letsencrypt/live/{domain}/fullchain.pem;\n    \
             ssl_certificate_key /etc/letsencrypt/live/{domain}/privkey.pem;\n    \
             ssl_protocols TLSv1.2 TLSv1.3;\n    \
             ssl_prefer_server_ciphers off;\n    \
             ssl_session_cache shared:SSL:10m;\n    \
             ssl_stapling on;\n    \
             add_header Strict-Transport-Security \"max-age=31536000\" always;\n",
            domain = site.domain
        )
    } else {
        "    listen 80;\n    listen [::]:80;\n".to_string()
    };

    let redirect_block = if ssl {
        format!(
            r#"server {{
    listen 80;
    listen [::]:80;
    server_name {server_names};
    location /.well-known/acme-challenge/ {{ root /var/www/acme; }}
    location / {{ return 301 https://$host$request_uri; }}
}}

"#
        )
    } else {
        String::new()
    };

    // Brotli is a third-party module (ngx_brotli); distro packages omit it.
    let compression_block = if caps.brotli {
        "    brotli on;\n    brotli_comp_level 5;\n    brotli_static on;\n    \
         brotli_types text/plain text/css application/javascript application/json image/svg+xml;\n"
    } else {
        "    gzip_static on;\n"
    };

    // Cache purge block: when ngx_cache_purge is available, allow the mu-plugin
    // (or panel) to purge individual URLs via a localhost-only GET request.
    let purge_block = if site.cache.fastcgi_cache && caps.cache_purge {
        format!(
            r#"
    # ---- Targeted cache purge (ngx_cache_purge) ---------------------------
    location ~ /wp-panel-purge(/.*) {{
        allow 127.0.0.1;
        allow ::1;
        deny all;
        fastcgi_cache_purge {cache_zone} "$scheme$request_method$host$1";
    }}
"#
        )
    } else {
        String::new()
    };

    // Header lists only the optional features that are switched on, so a
    // conservative config contains no mention of directives it must not use.
    let mut enabled: Vec<&str> = Vec::new();
    if caps.http2_directive {
        enabled.push("http2-directive");
    }
    if caps.http3 {
        enabled.push("http3");
    }
    if caps.brotli {
        enabled.push("brotli");
    }
    if caps.cache_purge && site.cache.fastcgi_cache {
        enabled.push("cache_purge");
    }
    let feature_line = if enabled.is_empty() {
        String::new()
    } else {
        format!("# optional features: {}\n", enabled.join(", "))
    };

    format!(
        r#"# Managed by wp-panel. Manual edits are overwritten.
# Rendered for nginx {nginx_version} as detected on this host.
{feature_line}{redirect_block}server {{
{tls_block}    server_name {server_names};
    root {public};
    index index.php index.html;

    access_log {logs}/nginx-access.log;
    error_log  {logs}/nginx-error.log warn;

    client_max_body_size 128m;
    server_tokens off;

    add_header X-Content-Type-Options nosniff always;
    add_header X-Frame-Options SAMEORIGIN always;
    add_header Referrer-Policy strict-origin-when-cross-origin always;

    gzip on;
    gzip_vary on;
    gzip_types text/plain text/css application/javascript application/json image/svg+xml;
{compression_block}
    # Static assets are served straight off disk.
    location ~* \.(?:css|js|jpg|jpeg|png|gif|webp|avif|svg|woff2?|ttf|ico|mp4)$ {{
        expires 30d;
        add_header Cache-Control "public, immutable";
        access_log off;
        try_files $uri =404;
    }}

    location = /favicon.ico {{ access_log off; log_not_found off; }}
    location = /robots.txt  {{ access_log off; log_not_found off; }}

    # Hardening: no PHP execution in uploads, no dotfiles.
    location ~* /(?:uploads|files)/.*\.php$ {{ deny all; }}
    location ~ /\.(?!well-known) {{ deny all; }}

    # Rate limit the login form.
    location = /wp-login.php {{
        limit_req zone=wp_login burst=5 nodelay;
        include fastcgi_params;
        fastcgi_pass unix:{socket};
        fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
    }}
{cache_block}{purge_block}
    location / {{
        try_files $uri $uri/ /index.php?$args;
    }}

    location ~ \.php$ {{
        try_files $uri =404;
        include fastcgi_params;
        fastcgi_pass unix:{socket};
        fastcgi_index index.php;
        fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
        fastcgi_read_timeout 120s;
        fastcgi_buffers 16 16k;
        fastcgi_buffer_size 32k;
    }}
}}
"#,
        nginx_version = caps.version_label(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wp_common::models::{CacheSettings, DatabaseMode, PhpVersion, ResourceLimits, SiteStatus};

    pub fn config() -> Config {
        Config {
            bind: "127.0.0.1:8443".parse().unwrap(),
            token: "t".repeat(24),
            sites_root: "/var/www".into(),
            cache_root: "/var/cache/nginx".into(),
            state_file: "/var/lib/wp-agent/state.json".into(),
            nginx_dir: "/etc/nginx/sites-enabled".into(),
            uid_base: 10001,
            dry_run: true,
            restic_repo: None,
            acme_email: None,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
        }
    }

    pub fn site() -> SiteRecord {
        SiteRecord {
            site_id: 1,
            domain: "example.com".into(),
            uid: 10001,
            php_version: PhpVersion::Php84,
            database_mode: DatabaseMode::Shared,
            limits: ResourceLimits::default(),
            cache: CacheSettings::default(),
            status: SiteStatus::Online,
            container_id: None,
            db_name: "wp_example_com".into(),
            domains: vec!["example.com".into(), "www.example.com".into()],
        }
    }

    /// Debian 12 / Ubuntu 24.04: no `http2 on;`, no brotli.
    #[test]
    fn legacy_nginx_uses_listen_parameter_and_no_brotli() {
        let caps = NginxCapabilities::parse("nginx version: nginx/1.24.0 (Ubuntu)");
        let vhost = render_vhost(&config(), &caps, &site(), true);

        assert!(vhost.contains("listen 443 ssl http2;"));
        assert!(!vhost.contains("http2 on;"));
        assert!(!vhost.contains("brotli"));
        assert!(vhost.contains("gzip_static on;"));
        assert!(!vhost.contains("quic"));
    }

    /// Mainline with the extra modules: modern directives appear.
    #[test]
    fn modern_nginx_uses_http2_directive_and_brotli() {
        let caps = NginxCapabilities::parse(
            "nginx version: nginx/1.29.1\nconfigure arguments: --with-http_v3_module \
             --add-module=/build/ngx_brotli",
        );
        let vhost = render_vhost(&config(), &caps, &site(), true);

        assert!(vhost.contains("http2 on;"));
        assert!(!vhost.contains("listen 443 ssl http2;"));
        assert!(vhost.contains("brotli on;"));
        assert!(vhost.contains("listen 443 quic;"));
        assert!(vhost.contains("http3 on;"));
        assert!(vhost.contains("Alt-Svc"));
    }

    /// `reuseport` must never be emitted: one per address:port, many vhosts.
    #[test]
    fn quic_listener_never_claims_reuseport() {
        let caps = NginxCapabilities::parse(
            "nginx version: nginx/1.29.1\nconfigure arguments: --with-http_v3_module",
        );
        let vhost = render_vhost(&config(), &caps, &site(), true);
        assert!(!vhost.contains("reuseport"));
    }

    #[test]
    fn plain_http_vhost_has_no_tls_or_redirect() {
        let caps = NginxCapabilities::CONSERVATIVE;
        let vhost = render_vhost(&config(), &caps, &site(), false);

        assert!(vhost.contains("listen 80;"));
        assert!(!vhost.contains("ssl_certificate"));
        assert!(!vhost.contains("return 301 https://"));
    }

    #[test]
    fn tls_vhost_redirects_http_and_keeps_acme_reachable() {
        let vhost = render_vhost(&config(), &NginxCapabilities::CONSERVATIVE, &site(), true);

        assert!(vhost.contains("return 301 https://$host$request_uri;"));
        assert!(vhost.contains("/.well-known/acme-challenge/"));
        assert!(vhost.contains("server_name example.com www.example.com;"));
    }

    #[test]
    fn caching_can_be_disabled_per_site() {
        let mut record = site();
        record.cache.fastcgi_cache = false;
        let vhost = render_vhost(&config(), &NginxCapabilities::CONSERVATIVE, &record, true);

        assert!(vhost.contains("set $skip_cache 1;"));
        assert!(!vhost.contains("fastcgi_cache example_com;"));
    }

    #[test]
    fn cache_zone_matches_the_zone_the_vhost_references() {
        let record = site();
        let vhost = render_vhost(&config(), &NginxCapabilities::CONSERVATIVE, &record, true);
        let zone = render_cache_zone(&config(), &record);

        assert!(vhost.contains("fastcgi_cache example_com;"));
        assert!(zone.contains("keys_zone=example_com:32m"));
    }

    /// Config we generate must never contain a directive that only exists in a
    /// newer Nginx than the host reports.
    #[test]
    fn conservative_output_avoids_every_optional_directive() {
        let vhost = render_vhost(&config(), &NginxCapabilities::CONSERVATIVE, &site(), true);

        for forbidden in ["http2 on;", "http3 on;", "brotli", "quic", "reuseport", "fastcgi_cache_purge"] {
            assert!(
                !vhost.contains(forbidden),
                "conservative vhost must not contain `{forbidden}`"
            );
        }
    }

    /// When cache_purge is available and caching enabled, the purge location appears.
    #[test]
    fn purge_location_rendered_when_cache_purge_available() {
        let caps = NginxCapabilities {
            cache_purge: true,
            ..NginxCapabilities::CONSERVATIVE
        };
        let vhost = render_vhost(&config(), &caps, &site(), true);

        assert!(vhost.contains("fastcgi_cache_purge example_com"));
        assert!(vhost.contains("/wp-panel-purge"));
        assert!(vhost.contains("allow 127.0.0.1;"));
        assert!(vhost.contains("deny all;"));
    }

    /// When cache_purge is absent, no purge directive appears.
    #[test]
    fn no_purge_location_without_cache_purge_module() {
        let caps = NginxCapabilities {
            cache_purge: false,
            ..NginxCapabilities::CONSERVATIVE
        };
        let vhost = render_vhost(&config(), &caps, &site(), true);

        assert!(!vhost.contains("fastcgi_cache_purge"));
        assert!(!vhost.contains("/wp-panel-purge"));
    }

    /// When cache_purge is available but caching disabled, no purge location.
    #[test]
    fn no_purge_location_when_caching_disabled() {
        let mut record = site();
        record.cache.fastcgi_cache = false;
        let caps = NginxCapabilities {
            cache_purge: true,
            ..NginxCapabilities::CONSERVATIVE
        };
        let vhost = render_vhost(&config(), &caps, &record, true);

        assert!(!vhost.contains("fastcgi_cache_purge"));
    }
}

/// Validates generated config against the real Nginx binary on this machine.
///
/// Ignored by default because it needs `nginx` and `openssl` installed. Run it
/// on any host you intend to support:
///
/// ```text
/// cargo test -p wp-agent -- --ignored nginx_accepts
/// ```
///
/// This is the test that would have caught `http2 on;` on Ubuntu 24.04 and
/// `brotli on;` without `ngx_brotli`.
#[cfg(test)]
mod host_validation {
    use super::tests_support::*;
    use super::*;
    use std::process::Command;

    #[test]
    #[ignore = "requires nginx and openssl on the host"]
    fn nginx_accepts_generated_config() {
        if Command::new("nginx").arg("-v").output().is_err() {
            eprintln!("nginx not installed; skipping");
            return;
        }

        let detected = probe_blocking();
        eprintln!(
            "testing against nginx {} (http2_directive={}, http3={}, brotli={})",
            detected.version_label(),
            detected.http2_directive,
            detected.http3,
            detected.brotli
        );

        // Both dialects must be accepted: what we render for this host, and the
        // conservative output we render when the probe fails or the host is old.
        let variants = [("detected", detected), ("conservative", NginxCapabilities::CONSERVATIVE)];

        for (label, caps) in variants {
        for ssl in [false, true] {
            let prefix = prepare_prefix(label, ssl).expect("prepare test prefix");
            let site = super::tests::site();
            let config = config_rooted_at(&prefix);

            let vhost = render_vhost(&config, &caps, &site, ssl)
                // The certificate lives in /etc/letsencrypt on a real host; use
                // the throwaway self-signed pair here.
                .replace(
                    "/etc/letsencrypt/live/example.com/fullchain.pem",
                    &prefix.join("test.crt").display().to_string(),
                )
                .replace(
                    "/etc/letsencrypt/live/example.com/privkey.pem",
                    &prefix.join("test.key").display().to_string(),
                );

            // `nginx -t` really binds the listeners, so move them to
            // unprivileged ports. The directive forms under test are unchanged.
            let vhost = vhost
                .replace("listen [::]:443", "listen [::]:18443")
                .replace("listen 443", "listen 18443")
                .replace("listen [::]:80;", "listen [::]:18080;")
                .replace("listen 80;", "listen 18080;");

            std::fs::write(prefix.join("site.conf"), &vhost).unwrap();
            std::fs::write(
                prefix.join("cache.conf"),
                render_cache_zone(&config, &site),
            )
            .unwrap();
            std::fs::write(prefix.join("nginx.conf"), main_config()).unwrap();

            let output = Command::new("nginx")
                .args([
                    "-t",
                    "-p",
                    &prefix.display().to_string(),
                    "-c",
                    &prefix.join("nginx.conf").display().to_string(),
                ])
                .output()
                .expect("run nginx -t");

            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "nginx rejected the {label} {} config:\n{stderr}\n--- config ---\n{vhost}",
                if ssl { "https" } else { "http" }
            );
            eprintln!("  ok: {label} {} config", if ssl { "https" } else { "http" });
        }
        }
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Synchronous capability probe for tests (the async one needs a runtime).
    pub fn probe_blocking() -> NginxCapabilities {
        let output = std::process::Command::new("nginx")
            .arg("-V")
            .output()
            .expect("nginx -V");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        NginxCapabilities::parse(&combined)
    }

    /// Temp prefix with the log directories, fastcgi_params and a self-signed
    /// certificate that `nginx -t` can open.
    pub fn prepare_prefix(label: &str, ssl: bool) -> std::io::Result<PathBuf> {
        let prefix = std::env::temp_dir().join(format!(
            "wp-panel-nginx-check-{label}-{}",
            if ssl { "https" } else { "http" }
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(prefix.join("example.com/public_html"))?;
        std::fs::create_dir_all(prefix.join("example.com/logs"))?;
        std::fs::create_dir_all(prefix.join("example.com/tmp"))?;
        std::fs::create_dir_all(prefix.join("cache"))?;

        for candidate in ["/etc/nginx/fastcgi_params", "/usr/local/nginx/conf/fastcgi_params"] {
            if Path::new(candidate).exists() {
                std::fs::copy(candidate, prefix.join("fastcgi_params"))?;
                break;
            }
        }

        if ssl {
            let status = std::process::Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                    "-subj", "/CN=example.com",
                    "-keyout", &prefix.join("test.key").display().to_string(),
                    "-out", &prefix.join("test.crt").display().to_string(),
                ])
                .output()?;
            assert!(status.status.success(), "openssl failed to make a test cert");
        }

        Ok(prefix)
    }

    pub fn config_rooted_at(prefix: &Path) -> Config {
        Config {
            bind: "127.0.0.1:8443".parse().unwrap(),
            token: "t".repeat(24),
            sites_root: prefix.to_path_buf(),
            cache_root: prefix.join("cache"),
            state_file: prefix.join("state.json"),
            nginx_dir: prefix.to_path_buf(),
            uid_base: 10001,
            dry_run: true,
            restic_repo: None,
            acme_email: None,
            tls_cert: None,
            tls_key: None,
            tls_self_signed: false,
        }
    }

    /// Minimal http context: the pieces `deploy/nginx/wp-panel-global.conf`
    /// provides on a real host, plus writable temp paths so the test can run
    /// unprivileged.
    pub fn main_config() -> String {
        "events { worker_connections 128; }\n\
         pid nginx.pid;\n\
         error_log error.log;\n\
         http {\n\
           access_log access.log;\n\
           client_body_temp_path client_body_temp;\n\
           proxy_temp_path proxy_temp;\n\
           fastcgi_temp_path fastcgi_temp;\n\
           uwsgi_temp_path uwsgi_temp;\n\
           scgi_temp_path scgi_temp;\n\
           limit_req_zone $binary_remote_addr zone=wp_login:10m rate=6r/m;\n\
           include cache.conf;\n\
           include site.conf;\n\
         }\n"
            .to_string()
    }
}
