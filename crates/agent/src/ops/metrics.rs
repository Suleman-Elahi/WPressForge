//! Host metrics for the panel dashboard, read from /proc and the tools already
//! installed. Cheap enough to run on every heartbeat.

use crate::config::Config;
use crate::exec;
use crate::ops::webserver::WebServer;
use crate::store::Store;
use std::collections::BTreeMap;
use wp_common::Result;
use wp_common::models::{PhpUsage, ServerMetrics, ServiceHealth};

pub async fn collect(config: &Config, store: &Store, web: &WebServer) -> Result<ServerMetrics> {
    let sites = store.all().await;

    let mut php_counts: BTreeMap<String, u32> = BTreeMap::new();
    for site in &sites {
        *php_counts
            .entry(site.php_version.as_str().to_string())
            .or_insert(0) += 1;
    }

    let php_versions = sites
        .iter()
        .map(|s| s.php_version)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|version| PhpUsage {
            sites: *php_counts.get(version.as_str()).unwrap_or(&0),
            version,
        })
        .collect();

    let (memory_percent, memory_total_mb) = memory().await;
    let (disk_percent, disk_total_gb) = disk(config).await;

    Ok(ServerMetrics {
        cpu_percent: cpu_percent().await,
        memory_percent,
        memory_total_mb,
        disk_percent,
        disk_total_gb,
        load_1m: load_average().await,
        sites: sites.len() as u32,
        containers: crate::ops::docker::container_count(config)
            .await
            .unwrap_or(0),
        services: services(web).await,
        php_versions,
    })
}

/// Approximates utilisation from the 1 minute load average and core count,
/// which avoids sampling /proc/stat twice per heartbeat.
async fn cpu_percent() -> f32 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as f32)
        .unwrap_or(1.0);
    ((load_average().await / cores) * 100.0).clamp(0.0, 100.0)
}

async fn load_average() -> f32 {
    tokio::fs::read_to_string("/proc/loadavg")
        .await
        .ok()
        .and_then(|raw| raw.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

async fn memory() -> (f32, u64) {
    let Ok(raw) = tokio::fs::read_to_string("/proc/meminfo").await else {
        return (0.0, 0);
    };

    let mut values: BTreeMap<&str, u64> = BTreeMap::new();
    for line in raw.lines() {
        if let Some((key, rest)) = line.split_once(':') {
            if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse().ok()) {
                values.insert(key, kb);
            }
        }
    }

    let total = values.get("MemTotal").copied().unwrap_or(0);
    let available = values.get("MemAvailable").copied().unwrap_or(0);
    if total == 0 {
        return (0.0, 0);
    }

    let used = total.saturating_sub(available);
    ((used as f32 / total as f32) * 100.0, total / 1024)
}

async fn disk(config: &Config) -> (f32, u64) {
    // `df` is simpler and more portable than statvfs bindings here.
    let Ok(output) = exec::run(
        false,
        "df",
        &[
            "-BG",
            "--output=size,pcent",
            &config.sites_root.display().to_string(),
        ],
    )
    .await
    else {
        return (0.0, 0);
    };

    let Some(line) = output.stdout.lines().nth(1) else {
        return (0.0, 0);
    };

    let mut parts = line.split_whitespace();
    let size = parts
        .next()
        .map(|v| v.trim_end_matches('G'))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let percent = parts
        .next()
        .map(|v| v.trim_end_matches('%'))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    (percent, size)
}

async fn services(web: &WebServer) -> Vec<ServiceHealth> {
    let mut health = Vec::new();

    // The web server reports its detected version so the panel can warn about
    // hosts too old for HTTP/3 or brotli.
    health.push(ServiceHealth {
        name: web.kind().to_string(),
        healthy: exec::run(false, "systemctl", &["is-active", "--quiet", web.kind()])
            .await
            .is_ok(),
        detail: Some(web.version_label()),
    });

    for unit in ["docker", "mariadb"] {
        // Read-only probe: always executed, even in dry-run, so the panel sees
        // the real service state.
        let ok = exec::run(false, "systemctl", &["is-active", "--quiet", unit])
            .await
            .is_ok();
        health.push(ServiceHealth {
            name: unit.to_string(),
            healthy: ok,
            detail: None,
        });
    }

    health.push(ServiceHealth {
        name: "agent".to_string(),
        healthy: true,
        detail: Some(env!("CARGO_PKG_VERSION").to_string()),
    });

    health
}
