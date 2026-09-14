use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha1 = Hmac<Sha1>;

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_encode(data: &[u8]) -> String {
    let mut res = String::new();
    let mut bits = 0;
    let mut val = 0u32;
    for &b in data {
        val = (val << 8) | (b as u32);
        bits += 8;
        while bits >= 5 {
            res.push(ALPHABET[((val >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        res.push(ALPHABET[((val << (5 - bits)) & 31) as usize] as char);
    }
    res
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits = 0;
    let mut val = 0u32;
    let mut out = Vec::new();
    for c in s.chars() {
        if c == '=' || c.is_whitespace() {
            continue;
        }
        let c = c.to_ascii_uppercase();
        let idx = ALPHABET.iter().position(|&b| b == c as u8)? as u32;
        val = (val << 5) | idx;
        bits += 5;
        if bits >= 8 {
            out.push((val >> (bits - 8)) as u8);
            bits -= 8;
            val &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn generate_secret() -> [u8; 20] {
    let mut secret = [0u8; 20];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut secret);
    secret
}

pub fn generate_code(secret: &[u8], time_step: u64) -> u32 {
    let mut mac = HmacSha1::new_from_slice(secret).expect("HMAC can take key of any size");
    mac.update(&time_step.to_be_bytes());
    let result = mac.finalize().into_bytes();
    let offset = (result[19] & 0xf) as usize;
    let code = ((result[offset] as u32 & 0x7f) << 24)
        | ((result[offset + 1] as u32) << 16)
        | ((result[offset + 2] as u32) << 8)
        | (result[offset + 3] as u32);
    code % 1_000_000
}

pub fn verify(secret: &[u8], code: &str) -> bool {
    let Ok(code) = code.parse::<u32>() else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let current_step = now / 30;

    for step in current_step.saturating_sub(1)..=current_step + 1 {
        if generate_code(secret, step) == code {
            return true;
        }
    }
    false
}

pub fn otpauth_uri(secret_b32: &str, email: &str) -> String {
    let encoded_email = urlencoding::encode(email);
    format!(
        "otpauth://totp/WPressForge:{}?secret={}&issuer=WPressForge",
        encoded_email, secret_b32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_roundtrip() {
        let raw = b"hello world 12345!";
        let encoded = base32_encode(raw);
        let decoded = base32_decode(&encoded).expect("valid base32");
        assert_eq!(raw.as_slice(), decoded.as_slice());
    }

    #[test]
    fn totp_rfc6238_vector() {
        // RFC 6238 test vector for SHA1, secret = "12345678901234567890" (ASCII)
        let secret = b"12345678901234567890";
        // Time = 59s -> step = 1 -> code 287082
        assert_eq!(generate_code(secret, 59 / 30), 287082);
        // Time = 1111111109s -> step = 37037036 -> code 081804
        assert_eq!(generate_code(secret, 1111111109 / 30), 81804);
    }

    #[test]
    fn totp_verify_current_step() {
        let secret = generate_secret();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let current_code = format!("{:06}", generate_code(&secret, now / 30));
        assert!(verify(&secret, &current_code));
        assert!(!verify(&secret, "0000000"));
        assert!(!verify(&secret, "999999"));
    }
}
