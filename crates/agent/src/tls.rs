//! TLS certificate loading and self-signed generation for the agent.
//!
//! On first start a self-signed certificate is generated and persisted next to
//! the state file so the fingerprint is stable across restarts.  The fingerprint
//! is logged at `WARN` so the installer (or the operator) can capture it and
//! paste it into the panel's "attach server" form.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// Result of loading or generating a TLS certificate.
pub struct TlsBundle {
    /// PEM-encoded certificate chain.
    pub cert_pem: Vec<u8>,
    /// PEM-encoded private key.
    pub key_pem: Vec<u8>,
    /// Human-readable SHA-256 fingerprint of the leaf certificate,
    /// e.g. `"sha256:AB:CD:..."`.
    pub fingerprint: String,
}

/// Load an existing certificate or generate a self-signed one.
///
/// * If `config.tls_cert` and `config.tls_key` are both provided, those files
///   are loaded and the fingerprint is computed from the certificate.
/// * Otherwise a self-signed certificate is generated (or re-used if one
///   already exists at the default path).
pub fn load_or_generate(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    state_dir: &Path,
    hostname: &str,
    ip: &str,
) -> Result<TlsBundle> {
    if let (Some(cert), Some(key)) = (cert_path, key_path) {
        let cert_pem = fs::read(cert)
            .with_context(|| format!("reading TLS certificate from {}", cert.display()))?;
        let key_pem = fs::read(key)
            .with_context(|| format!("reading TLS key from {}", key.display()))?;
        let fingerprint = fingerprint_from_pem(&cert_pem)?;
        tracing::info!(fingerprint = %fingerprint, "loaded TLS certificate from disk");
        return Ok(TlsBundle {
            cert_pem,
            key_pem,
            fingerprint,
        });
    }

    // Self-signed path: persist next to state file.
    let cert_file = state_dir.join("agent.crt");
    let key_file = state_dir.join("agent.key");

    if cert_file.exists() && key_file.exists() && !cert_path.is_some() {
        let cert_pem = fs::read(&cert_file)
            .with_context(|| format!("reading {}", cert_file.display()))?;
        let key_pem = fs::read(&key_file)
            .with_context(|| format!("reading {}", key_file.display()))?;
        let fingerprint = fingerprint_from_pem(&cert_pem)?;
        tracing::info!(fingerprint = %fingerprint, "reusing existing self-signed certificate");
        return Ok(TlsBundle {
            cert_pem,
            key_pem,
            fingerprint,
        });
    }

    // Generate a new self-signed certificate.
    tracing::info!("generating self-signed TLS certificate");
    let (cert_pem, key_pem) = generate_self_signed(hostname, ip)?;
    let fingerprint = fingerprint_from_pem(&cert_pem)?;

    // Ensure the state directory exists.
    fs::create_dir_all(state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;

    fs::write(&cert_file, &cert_pem)
        .with_context(|| format!("writing {}", cert_file.display()))?;
    fs::write(&key_file, &key_pem)
        .with_context(|| format!("writing {}", key_file.display()))?;

    // Restrict key permissions (owner read/write only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
    }

    tracing::warn!(
        fingerprint = %fingerprint,
        cert = %cert_file.display(),
        key = %key_file.display(),
        "self-signed TLS certificate generated — paste the fingerprint into the panel"
    );

    Ok(TlsBundle {
        cert_pem,
        key_pem,
        fingerprint,
    })
}

/// Generate a self-signed certificate for the given hostname and IP.
fn generate_self_signed(hostname: &str, ip: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params = rcgen::CertificateParams::new(vec![
        hostname.to_string(),
        ip.to_string(),
    ])
    .context("creating certificate params")?;

    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, hostname);

    let key_pair = rcgen::KeyPair::generate().context("generating key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("self-signing certificate")?;

    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();

    Ok((cert_pem, key_pem))
}

/// Compute the SHA-256 fingerprint of the first certificate in a PEM bundle.
fn fingerprint_from_pem(pem_bytes: &[u8]) -> Result<String> {
    let mut reader = std::io::BufReader::new(pem_bytes);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .context("parsing PEM certificate")?;

    let cert = certs
        .first()
        .context("no certificate found in PEM data")?;

    let digest = Sha256::digest(cert.as_ref());
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    Ok(format!("sha256:{}", hex.join(":")))
}
