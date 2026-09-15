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

        // Decode the presented token.
        let presented_bytes =
            match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(presented) {
                Ok(b) => b,
                Err(_) => return false,
            };

        if presented_bytes.len() != 32 {
            return false;
        }

        // `verify_slice` recomputes the MAC over the payload we feed it and
        // compares it with the presented tag in constant time. Feeding the
        // payload (not the tag) is the whole point: an earlier version passed
        // these the other way round, so no token ever verified.
        [now, previous].iter().any(|bucket| {
            let payload = format!("{session_token}:{bucket}");
            let mut mac = HmacSha256::new_from_slice(&self.0).expect("HMAC accepts any key length");
            mac.update(payload.as_bytes());
            mac.verify_slice(&presented_bytes).is_ok()
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CsrfKey {
        CsrfKey([7u8; 32])
    }

    #[test]
    fn a_freshly_issued_token_verifies() {
        let key = key();
        let token = key.token("session-abc");
        assert!(
            key.verify("session-abc", &token),
            "token issued for a session must verify for that session"
        );
    }

    #[test]
    fn a_token_is_bound_to_its_session() {
        let key = key();
        let token = key.token("session-abc");
        assert!(!key.verify("session-xyz", &token));
    }

    #[test]
    fn a_token_is_bound_to_the_key() {
        let token = key().token("session-abc");
        let other = CsrfKey([9u8; 32]);
        assert!(!other.verify("session-abc", &token));
    }

    #[test]
    fn rejects_empty_tampered_and_short_tokens() {
        let key = key();
        let token = key.token("session-abc");

        assert!(!key.verify("session-abc", ""));
        assert!(!key.verify("session-abc", "not-base64!!"));
        assert!(!key.verify("session-abc", &token[..token.len() - 2]));

        let mut tampered = token.clone().into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        assert!(!key.verify("session-abc", &String::from_utf8(tampered).unwrap()));
    }

    #[test]
    fn yesterdays_token_still_verifies_but_older_does_not() {
        let key = key();
        let now = day_bucket(chrono::Utc::now());

        assert!(key.verify("s", &key.make_token("s", now - 1)));
        assert!(!key.verify("s", &key.make_token("s", now - 2)));
    }

    #[test]
    fn check_token_requires_both_session_and_token() {
        let key = key();
        let token = key.token("s");

        assert!(check_token(&key, Some("s"), Some(&token)));
        assert!(!check_token(&key, None, Some(&token)));
        assert!(!check_token(&key, Some("s"), None));
    }
}
