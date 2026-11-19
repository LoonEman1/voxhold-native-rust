//! URL policy shared by the manifest and the downloader.

use url::Url;

use crate::error::{redact_url, Result, UpdateError};

/// Validate an update URL.
///
/// Remote hosts must use HTTPS. Plain HTTP is accepted only for loopback
/// hosts so local acceptance tests can serve artifacts without weakening the
/// production channel. Credentials in the URL are always rejected.
pub fn validate_update_url(raw: &str) -> Result<Url> {
    let parsed = Url::parse(raw).map_err(|_| UpdateError::InsecureUrl {
        url: redact_url(raw),
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UpdateError::InsecureUrl {
            url: redact_url(raw),
        });
    }
    let host = parsed.host_str().unwrap_or_default();
    if host.is_empty() {
        return Err(UpdateError::InsecureUrl {
            url: redact_url(raw),
        });
    }
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if is_loopback_host(host) => Ok(parsed),
        _ => Err(UpdateError::InsecureUrl {
            url: redact_url(raw),
        }),
    }
}

/// Whether a host names the local machine.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host == "localhost" || host == "::1" {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Redacted form of a URL for diagnostics.
pub fn redacted(raw: &str) -> String {
    redact_url(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https_and_loopback_http() {
        assert!(
            validate_update_url("https://github.com/a/b/releases/download/beta/latest.json")
                .is_ok()
        );
        assert!(validate_update_url("http://127.0.0.1:8080/full.zip").is_ok());
        assert!(validate_update_url("http://[::1]:8080/full.zip").is_ok());
        assert!(validate_update_url("http://localhost/full.zip").is_ok());
    }

    #[test]
    fn rejects_plain_http_and_credentials() {
        assert!(validate_update_url("http://example.invalid/full.zip").is_err());
        assert!(validate_update_url("ftp://example.invalid/full.zip").is_err());
        assert!(validate_update_url("https://user:pass@example.invalid/full.zip").is_err());
        assert!(validate_update_url("https://").is_err());
    }
}
