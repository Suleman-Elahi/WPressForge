//! AES-256-GCM encryption for destination credentials.
//!
//! The key is loaded from `WP_PANEL_SECRET_KEY` (base64, 32 bytes). If the
//! variable is missing at boot, a random key is generated, persisted to
//! `<db_dir>/secret.key` with mode 0600, and a warning is logged.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::Rng;
use std::path::Path;
use wp_common::{Error, Result};

pub struct SecretBox {
    key: [u8; 32],
}

impl SecretBox {
    /// Load or generate the encryption key.
    pub fn load_or_generate(db_dir: &Path) -> Result<Self> {
        if let Ok(key_b64) = std::env::var("WP_PANEL_SECRET_KEY") {
            let key_bytes = BASE64
                .decode(&key_b64)
                .map_err(|e| Error::Invalid(format!("invalid WP_PANEL_SECRET_KEY: {e}")))?;
            if key_bytes.len() != 32 {
                return Err(Error::Invalid(
                    "WP_PANEL_SECRET_KEY must be 32 bytes (base64)".into(),
                ));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            return Ok(Self { key });
        }

        // Generate a new key and persist it.
        let key_file = db_dir.join("secret.key");
        if key_file.exists() {
            let contents = std::fs::read_to_string(&key_file)
                .map_err(|e| Error::internal(format!("failed to read {}: {e}", key_file.display())))?;
            let key_bytes = BASE64
                .decode(contents.trim())
                .map_err(|e| Error::Invalid(format!("invalid secret.key: {e}")))?;
            if key_bytes.len() != 32 {
                return Err(Error::Invalid("secret.key must be 32 bytes".into()));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            return Ok(Self { key });
        }

        let mut key = [0u8; 32];
        rand::rng().fill(&mut key[..]);

        let key_b64 = BASE64.encode(key);
        std::fs::write(&key_file, &key_b64)
            .map_err(|e| Error::internal(format!("failed to write {}: {e}", key_file.display())))?;

        // Set mode 0600 (owner read/write only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600));
        }

        tracing::warn!(
            path = %key_file.display(),
            "generated new encryption key; set WP_PANEL_SECRET_KEY in production"
        );

        Ok(Self { key })
    }

    /// Encrypt plaintext. Returns base64(nonce || ciphertext).
    pub fn seal(&self, plaintext: &str) -> Result<String> {
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| Error::internal(e.to_string()))?;

        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill(&mut nonce_bytes[..]);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| Error::internal(e.to_string()))?;

        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);

        Ok(BASE64.encode(combined))
    }

    /// Decrypt base64(nonce || ciphertext) back to plaintext.
    pub fn open(&self, sealed: &str) -> Result<String> {
        let combined = BASE64
            .decode(sealed)
            .map_err(|e| Error::Invalid(format!("invalid sealed data: {e}")))?;

        if combined.len() < 12 {
            return Err(Error::Invalid("sealed data too short".into()));
        }

        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| Error::internal(e.to_string()))?;

        let nonce = Nonce::from_slice(&combined[..12]);
        let ciphertext = &combined[12..];

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| Error::Invalid(format!("decryption failed: {e}")))?;

        String::from_utf8(plaintext).map_err(|e| Error::Invalid(format!("invalid UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir() -> std::path::PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wp-panel-test-{}", ts))
    }

    #[test]
    fn seal_and_open_roundtrip() {
        let dir = test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let sb = SecretBox::load_or_generate(&dir).unwrap();
        let plaintext = "my-secret-access-key-12345";
        let sealed = sb.seal(plaintext).unwrap();
        let opened = sb.open(&sealed).unwrap();
        assert_eq!(opened, plaintext);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_sealed_values_for_same_plaintext() {
        let dir = test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let sb = SecretBox::load_or_generate(&dir).unwrap();
        let sealed1 = sb.seal("same").unwrap();
        let sealed2 = sb.seal("same").unwrap();
        // Different nonces produce different ciphertexts.
        assert_ne!(sealed1, sealed2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_key_fails_to_open() {
        let dir1 = test_dir();
        let dir2 = test_dir();
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        let sb1 = SecretBox::load_or_generate(&dir1).unwrap();
        let sb2 = SecretBox::load_or_generate(&dir2).unwrap();
        let sealed = sb1.seal("secret").unwrap();
        assert!(sb2.open(&sealed).is_err());
        let _ = std::fs::remove_dir_all(&dir1);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
