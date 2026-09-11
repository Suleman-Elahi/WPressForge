//! The agent keeps its own small record of the sites it manages, so operations
//! that arrive with only a `site_id` can find the domain, UID and container.
//! The panel remains the source of truth; this is a local index.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use wp_common::models::{CacheSettings, DatabaseMode, PhpVersion, ResourceLimits, SiteStatus};
use wp_common::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteRecord {
    pub site_id: i64,
    pub domain: String,
    pub uid: u32,
    pub php_version: PhpVersion,
    pub database_mode: DatabaseMode,
    pub limits: ResourceLimits,
    pub cache: CacheSettings,
    pub status: SiteStatus,
    pub container_id: Option<String>,
    pub db_name: String,
    pub domains: Vec<String>,
}

impl SiteRecord {
    pub fn container_name(&self) -> String {
        format!("wp-{}", self.domain.replace('.', "-"))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Snapshot {
    sites: BTreeMap<i64, SiteRecord>,
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    inner: Arc<RwLock<Snapshot>>,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let snapshot = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Snapshot::default(),
        };

        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(snapshot)),
        })
    }

    pub async fn get(&self, site_id: i64) -> Result<SiteRecord> {
        self.inner
            .read()
            .await
            .sites
            .get(&site_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("site {site_id} is not managed by this agent")))
    }

    pub async fn put(&self, record: SiteRecord) -> Result<()> {
        self.inner
            .write()
            .await
            .sites
            .insert(record.site_id, record);
        self.flush().await
    }

    pub async fn remove(&self, site_id: i64) -> Result<()> {
        self.inner.write().await.sites.remove(&site_id);
        self.flush().await
    }

    pub async fn all(&self) -> Vec<SiteRecord> {
        self.inner.read().await.sites.values().cloned().collect()
    }

    /// Lowest unused UID at or above `base`.
    pub async fn next_uid(&self, base: u32) -> u32 {
        let guard = self.inner.read().await;
        let mut uid = base;
        let used: Vec<u32> = guard.sites.values().map(|s| s.uid).collect();
        while used.contains(&uid) {
            uid += 1;
        }
        uid
    }

    async fn flush(&self) -> Result<()> {
        let bytes = {
            let guard = self.inner.read().await;
            serde_json::to_vec_pretty(&*guard).map_err(Error::internal)?
        };

        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(Error::internal)?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }
}
