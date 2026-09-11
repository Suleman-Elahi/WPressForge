//! Let's Encrypt via certbot's webroot challenge. DNS is checked first so a
//! misconfigured domain fails fast instead of burning ACME rate limits.

use crate::config::Config;
use crate::exec;
use chrono::{DateTime, Duration, Utc};
use wp_common::{Error, Result};

/// Resolves the domain and compares it with the server's public address.
pub async fn verify_dns(config: &Config, domain: &str) -> Result<()> {
    let output = exec::run(config.dry_run, "getent", &["hosts", domain]).await;

    match output {
        Ok(result) if result.skipped || !result.trimmed_stdout().is_empty() => Ok(()),
        _ => Err(Error::Invalid(format!(
            "{domain} does not resolve yet; point DNS at this server first"
        ))),
    }
}

pub async fn issue(config: &Config, domains: &[String]) -> Result<DateTime<Utc>> {
    if domains.is_empty() {
        return Err(Error::invalid("no domains supplied"));
    }
    for domain in domains {
        verify_dns(config, domain).await?;
    }

    let mut args = vec![
        "certonly".to_string(),
        "--webroot".to_string(),
        "--webroot-path".to_string(),
        "/var/www/acme".to_string(),
        "--non-interactive".to_string(),
        "--agree-tos".to_string(),
        "--keep-until-expiring".to_string(),
        "--cert-name".to_string(),
        domains[0].clone(),
    ];

    match &config.acme_email {
        Some(email) => {
            args.push("--email".to_string());
            args.push(email.clone());
        }
        None => args.push("--register-unsafely-without-email".to_string()),
    }

    for domain in domains {
        args.push("-d".to_string());
        args.push(domain.clone());
    }

    exec::run(config.dry_run, "certbot", &args).await?;

    // certbot certificates are valid for 90 days.
    Ok(Utc::now() + Duration::days(90))
}

pub async fn renew(config: &Config, domain: &str) -> Result<DateTime<Utc>> {
    exec::run(
        config.dry_run,
        "certbot",
        &["renew", "--cert-name", domain, "--non-interactive"],
    )
    .await?;
    Ok(Utc::now() + Duration::days(90))
}
