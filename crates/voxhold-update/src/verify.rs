//! Ed25519 verification of `latest.json`.
//!
//! The signature is checked before the JSON is parsed, so a tampered or
//! unauthenticated manifest never reaches the schema parser.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

use crate::error::{Result, UpdateError};
use crate::manifest::Manifest;

/// Base64 text compiled into the client.
///
/// A release build sets `VOXHOLD_UPDATE_PUBKEY` at build time; otherwise the
/// checked-in `update-pubkey.txt` is used. Both forms contain a 32-byte
/// Ed25519 public key in standard base64.
const ENV_PUBLIC_KEY: Option<&str> = option_env!("VOXHOLD_UPDATE_PUBKEY");
const FILE_PUBLIC_KEY: &str = include_str!("../update-pubkey.txt");

/// A validated Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePublicKey {
    bytes: [u8; 32],
}

impl UpdatePublicKey {
    /// Parse standard base64 that decodes to exactly 32 bytes.
    pub fn from_base64(text: &str) -> Result<Self> {
        let decoded = STANDARD
            .decode(text.trim())
            .map_err(|_| UpdateError::BadPublicKey)?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|_| UpdateError::BadPublicKey)?;
        Ok(Self { bytes })
    }

    /// Standard base64 form.
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.bytes)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The key baked into this build.
pub fn embedded_public_key() -> Result<UpdatePublicKey> {
    let text = match ENV_PUBLIC_KEY {
        Some(value) if !value.trim().is_empty() => value,
        _ => FILE_PUBLIC_KEY,
    };
    UpdatePublicKey::from_base64(text)
}

/// Verify a detached signature over raw manifest bytes.
pub fn verify_bytes(key: &UpdatePublicKey, message: &[u8], signature_base64: &str) -> Result<()> {
    let signature = STANDARD
        .decode(signature_base64.trim())
        .map_err(|_| UpdateError::BadSignatureEncoding)?;
    UnparsedPublicKey::new(&ED25519, key.as_bytes())
        .verify(message, &signature)
        .map_err(|_| UpdateError::BadSignature)
}

/// Verify the signature first, then parse and validate the manifest.
pub fn verify_and_parse(
    key: &UpdatePublicKey,
    manifest_bytes: &[u8],
    signature_base64: &str,
) -> Result<Manifest> {
    verify_bytes(key, manifest_bytes, signature_base64)?;
    Manifest::parse(manifest_bytes)
}

/// Generate a new Ed25519 key pair.
///
/// Returns `(pkcs8_base64, public_key_base64)`. The private document is meant
/// for `VOXHOLD_UPDATE_SIGNING_KEY` and must never be committed.
pub fn generate_keypair() -> Result<(String, String)> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| UpdateError::BadPublicKey)?;
    let pair =
        Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| UpdateError::BadPublicKey)?;
    Ok((
        STANDARD.encode(document.as_ref()),
        STANDARD.encode(pair.public_key().as_ref()),
    ))
}

/// Derive the public key from a base64 PKCS#8 document.
pub fn public_key_from_pkcs8(pkcs8_base64: &str) -> Result<UpdatePublicKey> {
    let document = STANDARD
        .decode(pkcs8_base64.trim())
        .map_err(|_| UpdateError::BadPublicKey)?;
    let pair = Ed25519KeyPair::from_pkcs8(&document).map_err(|_| UpdateError::BadPublicKey)?;
    let bytes: [u8; 32] = pair
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| UpdateError::BadPublicKey)?;
    Ok(UpdatePublicKey { bytes })
}

/// Sign manifest bytes with a base64 PKCS#8 document, returning base64.
pub fn sign_bytes(pkcs8_base64: &str, message: &[u8]) -> Result<String> {
    let document = STANDARD
        .decode(pkcs8_base64.trim())
        .map_err(|_| UpdateError::BadPublicKey)?;
    let pair = Ed25519KeyPair::from_pkcs8(&document).map_err(|_| UpdateError::BadPublicKey)?;
    Ok(STANDARD.encode(pair.sign(message).as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::BETA_CHANNEL;

    fn manifest_bytes() -> Vec<u8> {
        let manifest = Manifest {
            schema: 1,
            channel: BETA_CHANNEL.into(),
            version: "0.2.0".into(),
            min_windows: "10.0.19041".into(),
            published_at: "2026-11-19T12:00:00Z".into(),
            notes: "test".into(),
            authenticode: false,
            full: crate::manifest::FullPackage {
                url: "https://example.invalid/full.zip".into(),
                size: 10,
                sha256: "a".repeat(64),
                package_sha256: "a".repeat(64),
            },
            setup: crate::manifest::SetupPackage {
                url: "https://example.invalid/Setup.exe".into(),
                size: 10,
                sha256: "b".repeat(64),
            },
            deltas: Vec::new(),
        };
        manifest.to_json()
    }

    #[test]
    fn accepts_valid_signature() {
        let (private, public) = generate_keypair().unwrap();
        let key = UpdatePublicKey::from_base64(&public).unwrap();
        let bytes = manifest_bytes();
        let signature = sign_bytes(&private, &bytes).unwrap();
        let parsed = verify_and_parse(&key, &bytes, &signature).unwrap();
        assert_eq!(parsed.version, "0.2.0");
        assert_eq!(public_key_from_pkcs8(&private).unwrap(), key);
    }

    #[test]
    fn rejects_tampered_manifest() {
        let (private, public) = generate_keypair().unwrap();
        let key = UpdatePublicKey::from_base64(&public).unwrap();
        let bytes = manifest_bytes();
        let signature = sign_bytes(&private, &bytes).unwrap();
        let mut tampered = bytes.clone();
        let position = tampered.iter().position(|b| *b == b'0').unwrap();
        tampered[position] = b'1';
        assert!(matches!(
            verify_and_parse(&key, &tampered, &signature),
            Err(UpdateError::BadSignature)
        ));
    }

    #[test]
    fn rejects_tampered_signature() {
        let (private, public) = generate_keypair().unwrap();
        let key = UpdatePublicKey::from_base64(&public).unwrap();
        let bytes = manifest_bytes();
        let signature = sign_bytes(&private, &bytes).unwrap();
        let mut raw = STANDARD.decode(&signature).unwrap();
        raw[0] ^= 0xff;
        let broken = STANDARD.encode(raw);
        assert!(matches!(
            verify_and_parse(&key, &bytes, &broken),
            Err(UpdateError::BadSignature)
        ));
        assert!(matches!(
            verify_and_parse(&key, &bytes, "not base64!"),
            Err(UpdateError::BadSignatureEncoding)
        ));
    }

    #[test]
    fn verifies_before_parsing() {
        let (private, public) = generate_keypair().unwrap();
        let key = UpdatePublicKey::from_base64(&public).unwrap();
        let bytes = b"{ not json".to_vec();
        let signature = sign_bytes(&private, &bytes).unwrap();
        assert!(matches!(
            verify_and_parse(&key, &bytes, &signature),
            Err(UpdateError::InvalidManifest(_))
        ));
        let wrong = sign_bytes(&private, b"other").unwrap();
        assert!(matches!(
            verify_and_parse(&key, &bytes, &wrong),
            Err(UpdateError::BadSignature)
        ));
    }

    #[test]
    fn rejects_bad_public_keys() {
        assert!(matches!(
            UpdatePublicKey::from_base64("not base64!"),
            Err(UpdateError::BadPublicKey)
        ));
        assert!(matches!(
            UpdatePublicKey::from_base64(&STANDARD.encode([0u8; 16])),
            Err(UpdateError::BadPublicKey)
        ));
    }

    #[test]
    fn embedded_key_is_a_valid_ed25519_public_key() {
        let key = embedded_public_key().expect("checked-in update public key");
        assert_eq!(key.to_base64().len(), 44);
        assert_ne!(
            key.to_base64(),
            "PLACEHOLDER_REPLACE_WITH_REAL_ED25519_PUBLIC_KEY_BASE64="
        );
    }
}
