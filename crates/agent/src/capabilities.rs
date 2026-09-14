//! What the host's Nginx can actually do.
//!
//! Config we generate must match the binary that is installed, not the newest
//! Nginx we know about. Two directives in particular differ by version:
//!
//! * `http2 on;` exists from 1.25.1. Debian 12 ships 1.22.1 and Ubuntu 24.04
//!   ships 1.24.0, where the only form is `listen 443 ssl http2;`.
//! * `brotli on;` needs the third-party `ngx_brotli` module, which distro
//!   packages do not include.
//!
//! Emitting an unsupported directive makes `nginx -t` fail, which fails the
//! reload step, which fails the whole job. So we probe once at startup and
//! render accordingly.

use crate::exec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NginxCapabilities {
    /// Parsed `nginx version: nginx/X.Y.Z`.
    pub version: (u32, u32, u32),
    /// `http2 on;` is understood (>= 1.25.1). Otherwise use the `listen`
    /// parameter form.
    pub http2_directive: bool,
    /// Built with `--with-http_v3_module`.
    pub http3: bool,
    /// `ngx_brotli` compiled in (static or dynamic).
    pub brotli: bool,
    /// `ngx_cache_purge` compiled in: enables targeted FastCGI cache purging.
    pub cache_purge: bool,
    /// False when no usable `nginx` binary was found.
    pub detected: bool,
}

impl NginxCapabilities {
    /// Used when probing fails: the output works on every supported Nginx.
    /// Legacy `listen ... http2`, no brotli, no HTTP/3.
    pub const CONSERVATIVE: Self = Self {
        version: (1, 24, 0),
        http2_directive: false,
        http3: false,
        brotli: false,
        cache_purge: false,
        detected: false,
    };

    /// Runs `nginx -V`. Read-only, so it executes even in dry-run mode: the
    /// generated config must be correct for the host either way.
    pub async fn probe() -> Self {
        match exec::run(false, "nginx", &["-V"]).await {
            Ok(output) => {
                // nginx writes -V output to stderr; older builds use stdout.
                let combined = format!("{}\n{}", output.stderr, output.stdout);
                let caps = Self::parse(&combined);
                tracing::info!(
                    version = %format!("{}.{}.{}", caps.version.0, caps.version.1, caps.version.2),
                    http2_directive = caps.http2_directive,
                    http3 = caps.http3,
                    brotli = caps.brotli,
                    cache_purge = caps.cache_purge,
                    "detected nginx capabilities"
                );
                caps
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not run `nginx -V`; generating conservative config \
                     (no brotli, no HTTP/3, legacy http2 listen parameter)"
                );
                Self::CONSERVATIVE
            }
        }
    }

    /// Pure parser over the combined output of `nginx -V`.
    pub fn parse(output: &str) -> Self {
        let version = parse_version(output).unwrap_or(Self::CONSERVATIVE.version);

        Self {
            version,
            http2_directive: version >= (1, 25, 1),
            http3: output.contains("--with-http_v3_module"),
            brotli: output.contains("ngx_brotli") || output.contains("brotli"),
            cache_purge: output.contains("ngx_cache_purge")
                || output.contains("cache-purge")
                || output.contains("cache_purge"),
            detected: true,
        }
    }

    pub fn version_label(&self) -> String {
        format!("{}.{}.{}", self.version.0, self.version.1, self.version.2)
    }
}

/// Extracts `1.24.0` from `nginx version: nginx/1.24.0 (Ubuntu)`.
fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    let start = output.find("nginx/")? + "nginx/".len();
    let rest = &output[start..];
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();

    let mut parts = digits.split('.').map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
        parts.next().flatten().unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN_12: &str = "nginx version: nginx/1.22.1\nbuilt with OpenSSL 3.0.11\n\
        configure arguments: --with-http_ssl_module --with-http_v2_module";

    const UBUNTU_24_04: &str = "nginx version: nginx/1.24.0 (Ubuntu)\n\
        configure arguments: --with-http_ssl_module --with-http_v2_module \
        --with-http_realip_module";

    const MAINLINE_FULL: &str = "nginx version: nginx/1.29.1\nbuilt with OpenSSL 3.5.0\n\
        configure arguments: --with-http_ssl_module --with-http_v2_module \
        --with-http_v3_module --add-module=/build/ngx_brotli \
        --add-dynamic-module=/build/ngx_cache_purge";

    #[test]
    fn debian_12_has_no_http2_directive() {
        let caps = NginxCapabilities::parse(DEBIAN_12);
        assert_eq!(caps.version, (1, 22, 1));
        assert!(!caps.http2_directive);
        assert!(!caps.brotli);
        assert!(!caps.http3);
        assert!(caps.detected);
    }

    #[test]
    fn ubuntu_24_04_has_no_http2_directive() {
        let caps = NginxCapabilities::parse(UBUNTU_24_04);
        assert_eq!(caps.version, (1, 24, 0));
        assert!(!caps.http2_directive);
        assert!(!caps.brotli);
    }

    #[test]
    fn mainline_supports_everything() {
        let caps = NginxCapabilities::parse(MAINLINE_FULL);
        assert_eq!(caps.version, (1, 29, 1));
        assert!(caps.http2_directive);
        assert!(caps.http3);
        assert!(caps.brotli);
        assert!(caps.cache_purge);
    }

    #[test]
    fn http2_directive_boundary_is_1_25_1() {
        assert!(!NginxCapabilities::parse("nginx version: nginx/1.25.0").http2_directive);
        assert!(NginxCapabilities::parse("nginx version: nginx/1.25.1").http2_directive);
        assert!(NginxCapabilities::parse("nginx version: nginx/1.26.2").http2_directive);
    }

    #[test]
    fn unparsable_output_falls_back_to_the_conservative_version() {
        let caps = NginxCapabilities::parse("something else entirely");
        assert_eq!(caps.version, NginxCapabilities::CONSERVATIVE.version);
        assert!(!caps.http2_directive);
    }
}
