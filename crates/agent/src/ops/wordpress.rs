//! WordPress operations, all through WP-CLI inside the site container.
//!
//! WP-CLI subcommands are whitelisted: the panel cannot ask the agent to run
//! arbitrary commands, only WordPress management verbs.

use crate::config::Config;
use crate::exec::{self, CommandOutput};
use crate::ops::database::Credentials;
use crate::ops::docker;
use crate::store::SiteRecord;
use wp_common::models::{CronEventInfo, CronMode, PluginInfo, ThemeInfo, WpItemAction, WpUserInfo};
use wp_common::protocol::InstallWordpress;
use wp_common::{Error, Result};

const ALLOWED_SUBCOMMANDS: [&str; 14] = [
    "core", "plugin", "theme", "option", "user", "cache", "db", "search-replace", "cron",
    "rewrite", "transient", "site", "post", "media",
];

pub async fn install(
    config: &Config,
    site: &SiteRecord,
    credentials: &Credentials,
    request: &InstallWordpress,
) -> Result<String> {
    let container = site.container_name();

    wp(config, &container, &["core", "download", &format!("--locale={}", request.locale)]).await?;

    wp(
        config,
        &container,
        &[
            "config",
            "create",
            &format!("--dbname={}", credentials.name),
            &format!("--dbuser={}", credentials.user),
            &format!("--dbpass={}", credentials.password),
            &format!("--dbhost={}", credentials.host),
            "--dbcharset=utf8mb4",
            "--skip-check",
        ],
    )
    .await?;

    wp(
        config,
        &container,
        &[
            "core",
            "install",
            &format!("--url=https://{}", site.domain),
            &format!("--title={}", request.site_title),
            &format!("--admin_user={}", request.admin_user),
            &format!("--admin_email={}", request.admin_email),
            &format!("--admin_password={}", request.admin_password),
            "--skip-email",
        ],
    )
    .await?;

    // Sensible defaults for a fresh install.
    wp(config, &container, &["rewrite", "structure", "/%postname%/"]).await?;
    wp(config, &container, &["option", "update", "blog_public", "1"]).await?;

    version(config, site).await
}

pub async fn update_core(config: &Config, site: &SiteRecord) -> Result<String> {
    let container = site.container_name();
    wp(config, &container, &["core", "update"]).await?;
    wp(config, &container, &["core", "update-db"]).await?;
    version(config, site).await
}

pub async fn version(config: &Config, site: &SiteRecord) -> Result<String> {
    let output = wp(config, &site.container_name(), &["core", "version"]).await?;
    Ok(if output.skipped {
        "unknown".to_string()
    } else {
        output.trimmed_stdout().to_string()
    })
}

pub async fn flush_cache(config: &Config, site: &SiteRecord) -> Result<()> {
    wp(config, &site.container_name(), &["cache", "flush"])
        .await
        .map(|_| ())
}

/// Replaces the site URL everywhere. Used by clone and staging, which land with
/// their own milestone; kept here because the vhost and DB pieces already exist.
#[allow(dead_code)]
pub async fn search_replace(
    config: &Config,
    site: &SiteRecord,
    from: &str,
    to: &str,
) -> Result<()> {
    wp(
        config,
        &site.container_name(),
        &["search-replace", from, to, "--all-tables", "--precise"],
    )
    .await
    .map(|_| ())
}

/// Entry point for panel-supplied WP-CLI arguments.
pub async fn passthrough(
    config: &Config,
    site: &SiteRecord,
    args: &[String],
) -> Result<CommandOutput> {
    let subcommand = args
        .first()
        .ok_or_else(|| Error::invalid("no wp-cli subcommand supplied"))?;

    if !ALLOWED_SUBCOMMANDS.contains(&subcommand.as_str()) {
        return Err(Error::Invalid(format!(
            "`wp {subcommand}` is not allowed by this agent"
        )));
    }
    if args.iter().any(|arg| arg.contains(';') || arg.contains('`') || arg.contains('$')) {
        return Err(Error::invalid("wp-cli arguments contain shell metacharacters"));
    }

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    wp(config, &site.container_name(), &borrowed).await
}

// ---------------------------------------------------------------------------
// M2: WordPress management operations
// ---------------------------------------------------------------------------

/// Validate a plugin/theme slug: alphanumeric, dots, hyphens, underscores only.
fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > 64 {
        return Err(Error::invalid("slug must be 1-64 characters"));
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    {
        return Err(Error::invalid("slug contains invalid characters"));
    }
    if slug.starts_with('.') || slug.starts_with('-') {
        return Err(Error::invalid("slug must start with a letter or digit"));
    }
    Ok(())
}

pub async fn list_plugins(config: &Config, site: &SiteRecord) -> Result<Vec<PluginInfo>> {
    let out = wp(
        config,
        &site.container_name(),
        &[
            "plugin",
            "list",
            "--format=json",
            "--fields=name,status,version,update_version,auto_update",
        ],
    )
    .await?;
    if out.skipped {
        return Ok(demo_plugins());
    }
    serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)
}

pub async fn list_themes(config: &Config, site: &SiteRecord) -> Result<Vec<ThemeInfo>> {
    let out = wp(
        config,
        &site.container_name(),
        &[
            "theme",
            "list",
            "--format=json",
            "--fields=name,status,version,update_version",
        ],
    )
    .await?;
    if out.skipped {
        return Ok(demo_themes());
    }
    serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)
}

pub async fn list_wp_users(config: &Config, site: &SiteRecord) -> Result<Vec<WpUserInfo>> {
    let out = wp(
        config,
        &site.container_name(),
        &[
            "user",
            "list",
            "--format=json",
            "--fields=ID,user_login,user_email,roles",
        ],
    )
    .await?;
    if out.skipped {
        return Ok(vec![WpUserInfo {
            id: 1,
            login: "admin".into(),
            email: "admin@example.com".into(),
            role: "administrator".into(),
        }]);
    }
    let raw: Vec<serde_json::Value> =
        serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)?;
    Ok(raw
        .into_iter()
        .map(|v| WpUserInfo {
            id: v.get("ID").and_then(|x| x.as_i64()).unwrap_or(0),
            login: v
                .get("user_login")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            email: v
                .get("user_email")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            role: v
                .get("roles")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

pub async fn list_cron_events(config: &Config, site: &SiteRecord) -> Result<Vec<CronEventInfo>> {
    let out = wp(
        config,
        &site.container_name(),
        &[
            "cron",
            "event",
            "list",
            "--format=json",
            "--fields=hook,next_run_relative,schedule",
        ],
    )
    .await?;
    if out.skipped {
        return Ok(vec![]);
    }
    serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)
}

pub async fn core_check_update(
    config: &Config,
    site: &SiteRecord,
) -> Result<(String, Option<String>)> {
    let current = version(config, site).await?;
    let out = wp(
        config,
        &site.container_name(),
        &["core", "check-update", "--format=json"],
    )
    .await?;
    if out.skipped {
        return Ok((current, None));
    }
    // `wp core check-update` returns a JSON array; empty = up to date.
    let updates: Vec<serde_json::Value> =
        serde_json::from_str(out.trimmed_stdout()).map_err(Error::internal)?;
    let latest = updates.first().and_then(|v| v.get("update")).and_then(|u| {
        u.get("version")
            .or_else(|| u.get("new_version"))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    });
    Ok((current, latest))
}

pub async fn plugin_action(
    config: &Config,
    site: &SiteRecord,
    slug: &str,
    action: WpItemAction,
) -> Result<()> {
    validate_slug(slug)?;
    let verb = action.as_str();
    wp(config, &site.container_name(), &["plugin", verb, slug])
        .await
        .map(|_| ())
}

pub async fn theme_action(
    config: &Config,
    site: &SiteRecord,
    slug: &str,
    action: WpItemAction,
) -> Result<()> {
    validate_slug(slug)?;
    let verb = action.as_str();
    wp(config, &site.container_name(), &["theme", verb, slug])
        .await
        .map(|_| ())
}

pub async fn update_all_plugins(config: &Config, site: &SiteRecord) -> Result<()> {
    wp(config, &site.container_name(), &["plugin", "update", "--all"])
        .await
        .map(|_| ())
}

pub async fn reset_wp_password(
    config: &Config,
    site: &SiteRecord,
    user_login: &str,
) -> Result<String> {
    // Generate a random password, never log it.
    let mut bytes = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    let password = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &bytes,
    );
    let password = password.chars().take(20).collect::<String>();
    wp(
        config,
        &site.container_name(),
        &[
            "user",
            "update",
            user_login,
            &format!("--user_pass={password}"),
        ],
    )
    .await?;
    Ok(password)
}

pub async fn run_cron_event(config: &Config, site: &SiteRecord, hook: &str) -> Result<()> {
    // Reject anything that isn't a simple hook name.
    if hook.chars().any(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-') {
        return Err(Error::invalid("invalid cron hook name"));
    }
    wp(
        config,
        &site.container_name(),
        &["cron", "event", "run", hook],
    )
    .await
    .map(|_| ())
}

pub async fn set_wp_cron(config: &Config, site: &SiteRecord, mode: CronMode) -> Result<()> {
    match mode {
        CronMode::WpCron => {
            wp(
                config,
                &site.container_name(),
                &["config", "set", "DISABLE_WP_CRON", "false", "--raw"],
            )
            .await?;
        }
        CronMode::SystemCron => {
            wp(
                config,
                &site.container_name(),
                &["config", "set", "DISABLE_WP_CRON", "true", "--raw"],
            )
            .await?;
            // TODO: write /etc/cron.d/wp-<domain> for system cron
        }
    }
    Ok(())
}

pub async fn purge_urls(config: &Config, site: &SiteRecord, urls: &[String]) -> Result<()> {
    // Each URL is purged via the ngx_cache_purge location.
    // When ngx_cache_purge is absent, we fall back to a full cache flush.
    let cache_zone = site.domain.replace('.', "_");
    let purge_base = format!("/wp-panel-purge");

    for url in urls {
        // Build the purge URL: /wp-panel-purge + original path
        let path = url
            .strip_prefix(&format!("http://{}", site.domain))
            .or_else(|| url.strip_prefix(&format!("https://{}", site.domain)))
            .unwrap_or(url);

        let purge_url = format!("http://127.0.0.1{}{}", purge_base, path);

        // Attempt the purge via HTTP GET to the Nginx purge location.
        // This requires ngx_cache_purge module. If it fails (module not present),
        // fall back to wp_cache_flush().
        match exec::run(
            config.dry_run,
            "curl",
            &["-s", "-o", "/dev/null", "-w", "%{http_code}", &purge_url],
        )
        .await
        {
            Ok(_) => {}
            Err(_) => {
                // Fallback: flush entire object cache
                let _ = wp(
                    config,
                    &site.container_name(),
                    &["eval", "wp_cache_flush()"],
                )
                .await;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Demo data (dry-run)
// ---------------------------------------------------------------------------

fn demo_plugins() -> Vec<PluginInfo> {
    vec![
        PluginInfo {
            name: "Akismet Anti-Spam".into(),
            slug: "akismet".into(),
            status: "active".into(),
            version: "5.3.5".into(),
            update_version: None,
            auto_update: false,
        },
        PluginInfo {
            name: "Classic Editor".into(),
            slug: "classic-editor".into(),
            status: "inactive".into(),
            version: "1.6.6".into(),
            update_version: Some("1.6.7".into()),
            auto_update: false,
        },
        PluginInfo {
            name: "WooCommerce".into(),
            slug: "woocommerce".into(),
            status: "active".into(),
            version: "9.4.1".into(),
            update_version: None,
            auto_update: false,
        },
    ]
}

fn demo_themes() -> Vec<ThemeInfo> {
    vec![
        ThemeInfo {
            name: "Twenty Twenty-Five".into(),
            slug: "twentytwentyfive".into(),
            status: "active".into(),
            version: "1.0".into(),
            update_version: None,
        },
        ThemeInfo {
            name: "Twenty Twenty-Four".into(),
            slug: "twentytwentyfour".into(),
            status: "inactive".into(),
            version: "1.3".into(),
            update_version: Some("1.4".into()),
        },
    ]
}

pub async fn wp(config: &Config, container: &str, args: &[&str]) -> Result<CommandOutput> {
    let mut argv = vec!["wp".to_string(), "--path=/var/www/html".to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    docker::exec_in(config, container, &argv).await
}

/// Set database credentials in wp-config.php.
pub async fn set_db_config(
    config: &Config,
    site: &SiteRecord,
    credentials: &Credentials,
) -> Result<()> {
    wp(
        config,
        &site.container_name(),
        &[
            "config",
            "set",
            "DB_NAME",
            &credentials.name,
            "--raw",
            "--allow-root",
        ],
    )
    .await?;
    wp(
        config,
        &site.container_name(),
        &[
            "config",
            "set",
            "DB_USER",
            &credentials.user,
            "--raw",
            "--allow-root",
        ],
    )
    .await?;
    wp(
        config,
        &site.container_name(),
        &[
            "config",
            "set",
            "DB_PASSWORD",
            &credentials.password,
            "--raw",
            "--allow-root",
        ],
    )
    .await?;
    Ok(())
}
