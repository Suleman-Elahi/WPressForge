//! Thin HTTP client for the node agent. The panel only ever sends typed
//! [`Operation`]s: no shell strings cross this boundary.

use std::time::Duration;
use wp_common::protocol::{Operation, OperationEnvelope, OperationResult};
use wp_common::Error;

#[derive(Clone)]
pub struct AgentClient {
    http: reqwest::Client,
}

impl AgentClient {
    pub fn new() -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("wp-panel/", env!("CARGO_PKG_VERSION")))
            // Agents present a self-signed certificate pinned per server; until
            // pinning lands the panel talks to them over a private network.
            .danger_accept_invalid_certs(true)
            .build()?;
        Ok(Self { http })
    }

    /// Sends an operation and returns the agent's structured result.
    pub async fn send(
        &self,
        agent_url: &str,
        token: &str,
        operation: Operation,
        job_id: Option<i64>,
    ) -> Result<OperationResult, Error> {
        let mut envelope = OperationEnvelope::new(operation);
        if let Some(job_id) = job_id {
            envelope = envelope.with_job(job_id);
        }

        let url = format!("{}/v1/operations", agent_url.trim_end_matches('/'));
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
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
    pub async fn ping(&self, agent_url: &str, token: &str) -> Result<OperationResult, Error> {
        self.send(agent_url, token, Operation::Ping, None).await
    }

    pub async fn metrics(&self, agent_url: &str, token: &str) -> Result<OperationResult, Error> {
        self.send(agent_url, token, Operation::GetServerMetrics, None)
            .await
    }
}
