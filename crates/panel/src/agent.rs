//! Thin HTTP client for the node agent. The panel only ever sends typed
//! [`Operation`]s: no shell strings cross this boundary.
//!
//! Each server's certificate fingerprint is used to build a pinned TLS client
//! that will only accept that specific agent's certificate.  Clients are cached
//! so the handshake cost is paid only once per fingerprint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wp_common::Error;
use wp_common::protocol::{Operation, OperationEnvelope, OperationResult};

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

        if conn.fingerprint.is_none() && url.starts_with("https:") {
            return Err(Error::Unreachable("HTTPS requires a fingerprint".into()));
        }

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
        let envelope = OperationEnvelope::new(operation);
        let url = format!("{}/v1/operations", conn.url.trim_end_matches('/'));

        if conn.fingerprint.is_none() && url.starts_with("https:") {
            return Err(Error::Unreachable("HTTPS requires a fingerprint".into()));
        }

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
            Err(result
                .error
                .unwrap_or_else(|| Error::internal("agent query failed")))
        }
    }
}

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::Digest;

#[derive(Debug)]
struct FingerprintVerifier(String);

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let hash = sha2::Sha256::digest(end_entity.as_ref());
        let mut expected = self.0.trim().to_lowercase();
        if expected.starts_with("sha256:") {
            expected = expected[7..].to_string();
        }
        expected.retain(|c| c != ':');

        let mut expected_bytes = [0u8; 32];
        if expected.len() == 64 {
            for i in 0..32 {
                if let Ok(b) = u8::from_str_radix(&expected[i * 2..i * 2 + 2], 16) {
                    expected_bytes[i] = b;
                } else {
                    return Err(rustls::Error::General("invalid fingerprint hex".into()));
                }
            }
        } else {
            return Err(rustls::Error::General("invalid fingerprint length".into()));
        }

        let hash_bytes: [u8; 32] = hash.into();
        let mut diff = 0;
        for (a, b) in expected_bytes.iter().zip(hash_bytes.iter()) {
            diff |= a ^ b;
        }

        if diff == 0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a `reqwest::Client` that only accepts the server whose leaf
/// certificate matches `expected_fingerprint` (SHA-256, colon-separated hex).
fn build_pinned_client(expected_fingerprint: &str) -> reqwest::Result<reqwest::Client> {
    let verifier = std::sync::Arc::new(FingerprintVerifier(expected_fingerprint.to_string()));
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("wp-panel/", env!("CARGO_PKG_VERSION")))
        .use_preconfigured_tls(tls_config)
        .build()
}
