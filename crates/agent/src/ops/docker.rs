//! PHP-FPM containers. One per site, resource limited, running as the site UID
//! with only that site's files mounted.

use crate::config::Config;
use crate::exec;
use crate::store::SiteRecord;
use wp_common::models::PhpVersion;
use wp_common::Result;

/// Starts the PHP-FPM container for a site and returns its container id.
pub async fn start_php(config: &Config, site: &SiteRecord, version: PhpVersion) -> Result<String> {
    let root = config.site_root(&site.domain);
    let name = container_name(&site.domain, version);

    let args = vec![
        "run".to_string(),
        "--detach".to_string(),
        "--restart".to_string(),
        "unless-stopped".to_string(),
        "--name".to_string(),
        name.clone(),
        "--user".to_string(),
        format!("{}:{}", site.uid, site.uid),
        "--cpus".to_string(),
        format!("{:.2}", site.limits.cpu_cores),
        "--memory".to_string(),
        format!("{}m", site.limits.memory_mb),
        "--memory-swap".to_string(),
        format!("{}m", site.limits.memory_mb),
        "--pids-limit".to_string(),
        "512".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--read-only".to_string(),
        "--tmpfs".to_string(),
        "/tmp:rw,noexec,nosuid,size=128m".to_string(),
        "--volume".to_string(),
        format!("{}:/var/www/html", root.join("public_html").display()),
        "--volume".to_string(),
        format!("{}:/var/log/php", root.join("logs").display()),
        "--volume".to_string(),
        format!("{}:/run/php", root.join("tmp").display()),
        "--env".to_string(),
        format!("PHP_FPM_MAX_CHILDREN={}", site.limits.php_workers),
        "--env".to_string(),
        format!("PHP_FPM_UID={}", site.uid),
        "--label".to_string(),
        format!("wp-panel.site={}", site.domain),
        "--label".to_string(),
        format!("wp-panel.php={}", version.as_str()),
        version.image(),
    ];

    let output = exec::run(config.dry_run, "docker", &args).await?;
    Ok(if output.skipped {
        name
    } else {
        output.trimmed_stdout().to_string()
    })
}

pub async fn stop(config: &Config, container: &str) -> Result<()> {
    exec::run(config.dry_run, "docker", &["stop", "--time", "20", container])
        .await
        .map(|_| ())
}

pub async fn remove(config: &Config, container: &str) -> Result<()> {
    exec::run(config.dry_run, "docker", &["rm", "--force", container])
        .await
        .map(|_| ())
}

pub async fn restart(config: &Config, container: &str) -> Result<()> {
    exec::run(config.dry_run, "docker", &["restart", container])
        .await
        .map(|_| ())
}

/// `true` when the container is running and its FPM socket answers.
pub async fn healthy(config: &Config, container: &str) -> Result<bool> {
    let output = exec::run(
        config.dry_run,
        "docker",
        &[
            "inspect",
            "--format",
            "{{.State.Running}}",
            container,
        ],
    )
    .await?;

    Ok(output.skipped || output.trimmed_stdout() == "true")
}

pub async fn pull(config: &Config, version: PhpVersion) -> Result<()> {
    exec::run(config.dry_run, "docker", &["pull", &version.image()])
        .await
        .map(|_| ())
}

/// Runs a command inside the site container as the site user.
pub async fn exec_in(config: &Config, container: &str, argv: &[String]) -> Result<exec::CommandOutput> {
    let mut args = vec![
        "exec".to_string(),
        "--workdir".to_string(),
        "/var/www/html".to_string(),
        container.to_string(),
    ];
    args.extend(argv.iter().cloned());
    exec::run(config.dry_run, "docker", &args).await
}

pub async fn container_count(config: &Config) -> Result<u32> {
    let output = exec::run(
        config.dry_run,
        "docker",
        &["ps", "--quiet", "--filter", "label=wp-panel.site"],
    )
    .await?;

    Ok(output.trimmed_stdout().lines().filter(|l| !l.is_empty()).count() as u32)
}

/// Container names are versioned so a PHP switch can run both side by side.
pub fn container_name(domain: &str, version: PhpVersion) -> String {
    format!("wp-{}-php{}", domain.replace('.', "-"), version.as_str().replace('.', ""))
}
