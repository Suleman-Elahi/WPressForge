//! Nginx vhost generation. Nginx stays on the host and proxies to the site's
//! PHP-FPM socket; the cache is WordPress-aware.

use crate::config::Config;
use crate::exec;
use crate::store::SiteRecord;
use wp_common::Result;

/// Renders the vhost for a site. Kept as a plain function so it can be unit
/// tested and diffed before anything is written.
pub fn render_vhost(config: &Config, site: &SiteRecord, ssl: bool) -> String {
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
        format!(
            r#"    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    ssl_certificate     /etc/letsencrypt/live/{domain}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/{domain}/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers off;
    ssl_session_cache shared:SSL:10m;
    ssl_stapling on;
    add_header Strict-Transport-Security "max-age=31536000" always;
"#,
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

    format!(
        r#"# Managed by wp-panel. Manual edits are overwritten.
{redirect_block}server {{
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
{brotli}
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
{cache_block}
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
        brotli = if site.cache.brotli {
            "    brotli on;\n    brotli_types text/plain text/css application/javascript application/json image/svg+xml;\n"
        } else {
            ""
        }
    )
}

/// `fastcgi_cache_path` must live in the http context, so each site gets a small
/// include next to its vhost declaring its own cache zone.
pub fn render_cache_zone(site: &SiteRecord) -> String {
    let zone = site.domain.replace('.', "_");
    format!(
        "# Managed by wp-panel.\n\
         fastcgi_cache_path /var/cache/nginx/{zone} levels=1:2 keys_zone={zone}:32m \
         inactive=1h max_size=512m use_temp_path=off;\n\
         fastcgi_cache_key \"$scheme$request_method$host$request_uri\";\n"
    )
}

pub async fn write_vhost(config: &Config, site: &SiteRecord, ssl: bool) -> Result<()> {
    let path = config.vhost_path(&site.domain);
    let contents = render_vhost(config, site, ssl);

    if site.cache.fastcgi_cache {
        let zone_path = config
            .nginx_dir
            .parent()
            .unwrap_or(&config.nginx_dir)
            .join("conf.d")
            .join(format!("wp-cache-{}.conf", site.domain));

        if config.dry_run {
            tracing::info!(path = %zone_path.display(), "dry-run: would write cache zone");
        } else {
            if let Some(parent) = zone_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(wp_common::Error::internal)?;
            }
            tokio::fs::write(&zone_path, render_cache_zone(site))
                .await
                .map_err(wp_common::Error::internal)?;
        }
    }

    if config.dry_run {
        tracing::info!(path = %path.display(), bytes = contents.len(), "dry-run: would write vhost");
    } else {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(wp_common::Error::internal)?;
        }
        tokio::fs::write(&path, contents)
            .await
            .map_err(wp_common::Error::internal)?;
    }

    Ok(())
}

pub async fn remove_vhost(config: &Config, domain: &str) -> Result<()> {
    let path = config.vhost_path(domain);
    if config.dry_run {
        tracing::info!(path = %path.display(), "dry-run: would remove vhost");
        return Ok(());
    }
    let _ = tokio::fs::remove_file(path).await;
    Ok(())
}

/// `nginx -t` before every reload: a bad config never reaches production.
pub async fn test(config: &Config) -> Result<()> {
    exec::run(config.dry_run, "nginx", &["-t"]).await.map(|_| ())
}

pub async fn reload(config: &Config) -> Result<()> {
    test(config).await?;
    exec::run(config.dry_run, "systemctl", &["reload", "nginx"])
        .await
        .map(|_| ())
}

/// Purges the FastCGI cache directory for one site.
pub async fn purge_cache(config: &Config, domain: &str) -> Result<()> {
    let path = format!("/var/cache/nginx/{}", domain.replace('.', "_"));
    exec::run(config.dry_run, "find", &[&path, "-type", "f", "-delete"])
        .await
        .map(|_| ())
}
