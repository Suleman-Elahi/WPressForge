//! Stateless double-submit CSRF protection with an HMAC-bound token.
//!
//! `token(session)` = base64url(HMAC-SHA256(secret, session_token || ":" || day_bucket))
//!
//! The middleware accepts the current and previous day bucket so a form open
//! across midnight still submits.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 32 random bytes generated at boot, kept in [`AppState`](crate::state::AppState).
pub struct CsrfKey(pub [u8; 32]);

impl CsrfKey {
    /// Generate a CSRF token for the given session token, scoped to the
    /// current UTC day bucket.
    pub fn token(&self, session_token: &str) -> String {
        self.make_token(session_token, day_bucket(chrono::Utc::now()))
    }

    /// Verify a presented token against the current and previous day buckets.
    /// Uses constant-time comparison.
    pub fn verify(&self, session_token: &str, presented: &str) -> bool {
        if presented.is_empty() {
            return false;
        }
        let now = day_bucket(chrono::Utc::now());
        let previous = now - 1;

        // Compute expected tokens for both buckets.
        let expected_current = self.hmac_bytes(session_token, now);
        let expected_previous = self.hmac_bytes(session_token, previous);

        // Decode the presented token.
        let presented_bytes =
            match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(presented) {
                Ok(b) => b,
                Err(_) => return false,
            };

        if presented_bytes.len() != 32 {
            return false;
        }

        // Constant-time compare against both expected values.
        let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(&presented_bytes);
        let ok1 = mac.verify_slice(&expected_current).is_ok();

        let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(&presented_bytes);
        let ok2 = mac.verify_slice(&expected_previous).is_ok();

        ok1 || ok2
    }

    fn hmac_bytes(&self, session_token: &str, bucket: i64) -> Vec<u8> {
        let payload = format!("{session_token}:{bucket}");
        let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    fn make_token(&self, session_token: &str, bucket: i64) -> String {
        let bytes = self.hmac_bytes(session_token, bucket);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

/// Returns the number of days since Unix epoch — the "day bucket".
fn day_bucket(when: chrono::DateTime<chrono::Utc>) -> i64 {
    when.timestamp() / 86400
}

/// Checks a CSRF token from the `x-csrf-token` header or a form field.
/// Returns `true` if the token is valid for the given session.
pub fn check_token(
    csrf_key: &CsrfKey,
    session_token: Option<&str>,
    presented: Option<&str>,
) -> bool {
    match (session_token, presented) {
        (Some(session), Some(token)) => csrf_key.verify(session, token),
        _ => false,
    }
}
