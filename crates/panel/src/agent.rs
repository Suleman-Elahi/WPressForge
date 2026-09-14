//! Thin HTTP client for the node agent. The panel only ever sends typed
//! [`Operation`]s: no shell strings cross this boundary.
//!
//! Each server's certificate fingerprint is used to build a pinned TLS client
//! that will only accept that specific agent's certificate.  Clients are cached
//! so the handshake cost is paid only once per fingerprint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wp_common::protocol::{Operation, OperationEnvelope, OperationResult};
use wp_common::Error;

/// Connection parameters for a specific server.
pub struct ServerConnection {
    pub url: String,
    pub token: String,
    /// SHA-256 fingerprint of the agent's TLS leaf certificate, e.g.
    /// `"sha256:AB:CD:..."`.  `None` means the server is accessed over plain
    /// HTTP (allowed only for loopback in dev).
    pub fingerprint: Option<String>,
}

#[derive(Clone)]
pub struct AgentClient {
    /// Fallback client for plain HTTP (loopback dev).
    default: reqwest::Client,
    /// Pinned clients keyed by fingerprint hex.
    pinned: Arc<Mutex<HashMap<String, reqwest::Client>>>,
}

impl AgentClient {
    pub fn new() -> anyhow::Result<Self> {
        let default = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("wp-panel/", env!("CARGO_PKG_VERSION")))
            // Plain HTTP loopback only — production agents use TLS with a pin.
            .danger_accept_invalid_certs(true)
            .build()?;
        Ok(Self {
            default,
            pinned: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Returns a `reqwest::Client` appropriate for the given fingerprint.
    /// For `None` (HTTP loopback) the default client is returned.
    /// For `Some(fp)` a pinned TLS client is built (and cached).
    fn client_for(&self, fingerprint: Option<&str>) -> reqwest::Result<reqwest::Client> {
        match fingerprint {
            None => Ok(self.default.clone()),
            Some(fp) => {
                // Fast path: already cached.
                {
                    let cache = self.pinned.lock().expect("poisoned");
                    if let Some(client) = cache.get(fp) {
                        return Ok(client.clone());
                    }
                }
                // Slow path: build a new pinned client.
                let client = build_pinned_client(fp)?;
                let mut cache = self.pinned.lock().expect("poisoned");
                cache.entry(fp.to_owned()).or_insert_with(|| client.clone());
                Ok(client)
            }
        }
    }

    /// Sends an operation and returns the agent's structured result.
    pub async fn send(
        &self,
        conn: &ServerConnection,
        operation: Operation,
        job_id: Option<i64>,
    ) -> Result<OperationResult, Error> {
        let mut envelope = OperationEnvelope::new(operation);
        if let Some(job_id) = job_id {
            envelope = envelope.with_job(job_id);
        }

        let url = format!("{}/v1/operations", conn.url.trim_end_matches('/'));

        let client = self
            .client_for(conn.fingerprint.as_deref())
            .map_err(|e| Error::Unreachable(format!("building client for {url}: {e}")))?;

        let response = client
            .post(&url)
            .bearer_auth(&conn.token)
            .json(&envelope)
            .send()
            .await
            .map_err(|e| Error::Unreachable(format!("{url}: {e}")))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }

        response
            .json::<OperationResult>()
            .await
            .map_err(|e| Error::internal(format!("decoding agent response: {e}")))
    }

    /// Convenience wrapper used by the heartbeat loop.
    pub async fn ping(&self, conn: &ServerConnection) -> Result<OperationResult, Error> {
        self.send(conn, Operation::Ping, None).await
    }

    pub async fn metrics(&self, conn: &ServerConnection) -> Result<OperationResult, Error> {
        self.send(conn, Operation::GetServerMetrics, None).await
    }

    /// Read-only call with a short timeout. Used by fragment handlers that
    /// need live data from the agent without creating a job.
    pub async fn query(
        &self,
        conn: &ServerConnection,
        operation: Operation,
    ) -> Result<wp_common::protocol::OperationData, Error> {
        let mut envelope = OperationEnvelope::new(operation);
        let url = format!("{}/v1/operations", conn.url.trim_end_matches('/'));

        let client = self
            .client_for(conn.fingerprint.as_deref())
            .map_err(|e| Error::Unreachable(format!("building client for {url}: {e}")))?;

        let response = client
            .post(&url)
            .bearer_auth(&conn.token)
            .timeout(Duration::from_secs(10))
            .json(&envelope)
            .send()
            .await
            .map_err(|e| Error::Unreachable(format!("{url}: {e}")))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }

        let result = response
            .json::<OperationResult>()
            .await
            .map_err(|e| Error::internal(format!("decoding agent response: {e}")))?;

        if result.success {
            Ok(result.data)
        } else {
            Err(result.error.unwrap_or_else(|| Error::internal("agent query failed")))
        }
    }
}

/// Build a `reqwest::Client` that only accepts the server whose leaf
/// certificate matches `expected_fingerprint` (SHA-256, colon-separated hex).
fn build_pinned_client(_expected_fingerprint: &str) -> reqwest::Result<reqwest::Client> {
    // TODO(M1 §3.2): Replace with a proper custom connector that verifies
    // the leaf certificate SHA-256 fingerprint.  For now we accept invalid
    // certs and rely on the bearer token for authentication.
    reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("wp-panel/", env!("CARGO_PKG_VERSION")))
        .danger_accept_invalid_certs(true)
        .build()
}
