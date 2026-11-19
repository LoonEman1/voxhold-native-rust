//! Background check of the signed `latest.json` on the update channel.
//!
//! This is the network half of "check on start and on a timer": the caller
//! decides when to run it, the crate enforces HTTPS, a response limit,
//! cancellation and signature verification before parsing.

use reqwest::StatusCode;
use std::time::Duration;

use crate::download::{build_client, CancelToken};
use crate::error::{redact_url, Result, UpdateError};
use crate::manifest::VerifiedManifest;
use crate::urls::validate_update_url;
use crate::verify::{verify_and_parse, UpdatePublicKey};

/// Maximum size of `latest.json`.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Maximum size of `latest.json.sig`.
pub const MAX_SIGNATURE_BYTES: u64 = 8 * 1024;

/// Input for [`check_signed_manifest`].
pub struct CheckRequest<'a> {
    pub manifest_url: &'a str,
    pub signature_url: &'a str,
    pub key: &'a UpdatePublicKey,
    pub cancel: CancelToken,
}

/// Fetch `latest.json` and its detached signature, verify, then parse.
///
/// The result is a [`VerifiedManifest`] and can be passed to
/// [`crate::stage::stage_update`].
pub async fn check_signed_manifest(request: CheckRequest<'_>) -> Result<VerifiedManifest> {
    let manifest_origin = validate_update_url(request.manifest_url)?.origin();
    let signature_origin = validate_update_url(request.signature_url)?.origin();
    if manifest_origin != signature_origin {
        return Err(UpdateError::InvalidArgument(
            "the manifest and signature URLs must share an origin".into(),
        ));
    }
    let manifest_bytes =
        fetch_limited(request.manifest_url, MAX_MANIFEST_BYTES, &request.cancel).await?;
    let signature_bytes =
        fetch_limited(request.signature_url, MAX_SIGNATURE_BYTES, &request.cancel).await?;
    let signature =
        String::from_utf8(signature_bytes).map_err(|_| UpdateError::BadSignatureEncoding)?;
    verify_and_parse(request.key, &manifest_bytes, &signature)
}

async fn fetch_limited(url: &str, limit: u64, cancel: &CancelToken) -> Result<Vec<u8>> {
    let parsed = validate_update_url(url)?;
    let client = build_client()?;
    let mut response = tokio::select! {
        _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
        response = tokio::time::timeout(Duration::from_secs(30), client.get(parsed).send()) => {
            match response {
                Err(_) => return Err(UpdateError::Network("manifest request timed out".into())),
                Ok(response) => response.map_err(network_error)?,
            }
        }
    };
    if response.status() != StatusCode::OK {
        return Err(UpdateError::HttpStatus {
            url: redact_url(url),
            status: response.status().as_u16(),
        });
    }
    if let Some(length) = response.content_length() {
        if length > limit {
            return Err(UpdateError::TooLarge {
                path: "manifest response".into(),
                limit,
                actual: length,
            });
        }
    }
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(UpdateError::Cancelled),
            chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk()) => {
                match chunk {
                    Err(_) => return Err(UpdateError::Network("manifest body timed out".into())),
                    Ok(chunk) => chunk.map_err(network_error)?,
                }
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        body.extend_from_slice(&chunk);
        if body.len() as u64 > limit {
            return Err(UpdateError::TooLarge {
                path: "manifest response".into(),
                limit,
                actual: body.len() as u64,
            });
        }
    }
    Ok(body)
}

fn network_error(error: reqwest::Error) -> UpdateError {
    UpdateError::Network(error.without_url().to_string())
}
