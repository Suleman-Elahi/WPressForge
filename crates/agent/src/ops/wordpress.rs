//! WordPress operations, all through WP-CLI inside the site container.
//!
//! WP-CLI subcommands are whitelisted: the panel cannot ask the agent to run
//! arbitrary commands, only WordPress management verbs.

use crate::config::Config;
use crate::exec::CommandOutput;
use crate::ops::database::Credentials;
use crate::ops::docker;
use crate::store::SiteRecord;
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

async fn wp(config: &Config, container: &str, args: &[&str]) -> Result<CommandOutput> {
    let mut argv = vec!["wp".to_string(), "--path=/var/www/html".to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    docker::exec_in(config, container, &argv).await
}
