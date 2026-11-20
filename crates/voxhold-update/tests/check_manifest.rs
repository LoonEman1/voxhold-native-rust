mod support;

use std::collections::HashMap;

use support::{Route, TestServer};
use voxhold_update::check::{check_signed_manifest, CheckRequest};
use voxhold_update::download::CancelToken;
use voxhold_update::error::UpdateError;
use voxhold_update::manifest::{DeltaEntry, FullPackage, Manifest, SetupPackage, BETA_CHANNEL};
use voxhold_update::verify::{generate_keypair, sign_bytes, UpdatePublicKey};

fn manifest_json() -> Vec<u8> {
    let manifest = Manifest {
        schema: 1,
        channel: BETA_CHANNEL.into(),
        version: "0.2.0".into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "check test".into(),
        authenticode: false,
        full: FullPackage {
            url: "https://example.invalid/full.zip".into(),
            size: 100,
            sha256: "a".repeat(64),
            package_sha256: "a".repeat(64),
        },
        setup: SetupPackage {
            url: "https://example.invalid/Setup.exe".into(),
            size: 10,
            sha256: "b".repeat(64),
        },
        deltas: vec![DeltaEntry {
            from_version: "0.1.0".into(),
            from_package_sha256: "c".repeat(64),
            url: "https://example.invalid/delta.zip".into(),
            size: 5,
            sha256: "d".repeat(64),
        }],
    };
    manifest.to_pretty_json()
}

#[tokio::test]
async fn verifies_a_signed_manifest() {
    let (private, public) = generate_keypair().unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    let bytes = manifest_json();
    let signature = sign_bytes(&private, &bytes).unwrap();
    let mut routes = HashMap::new();
    routes.insert(
        "/latest.json".to_string(),
        Route {
            body: bytes,
            ..Route::default()
        },
    );
    routes.insert(
        "/latest.json.sig".to_string(),
        Route {
            body: format!("{signature}\n").into_bytes(),
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let manifest_url = server.url("/latest.json");
    let signature_url = server.url("/latest.json.sig");
    let manifest = check_signed_manifest(CheckRequest {
        manifest_url: &manifest_url,
        signature_url: &signature_url,
        key: &key,
        cancel: CancelToken::new(),
    })
    .await
    .unwrap();
    assert_eq!(manifest.version, "0.2.0");
    assert_eq!(manifest.deltas.len(), 1);
}

#[tokio::test]
async fn rejects_a_tampered_manifest_before_parsing() {
    let (private, public) = generate_keypair().unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    let signed = manifest_json();
    let signature = sign_bytes(&private, &signed).unwrap();
    let mut tampered = signed.clone();
    let position = tampered
        .windows(5)
        .position(|window| window == b"0.2.0")
        .unwrap();
    tampered[position + 4] = b'9';
    let mut routes = HashMap::new();
    routes.insert(
        "/latest.json".to_string(),
        Route {
            body: tampered,
            ..Route::default()
        },
    );
    routes.insert(
        "/latest.json.sig".to_string(),
        Route {
            body: signature.into_bytes(),
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let manifest_url = server.url("/latest.json");
    let signature_url = server.url("/latest.json.sig");
    let result = check_signed_manifest(CheckRequest {
        manifest_url: &manifest_url,
        signature_url: &signature_url,
        key: &key,
        cancel: CancelToken::new(),
    })
    .await;
    assert!(
        matches!(result, Err(UpdateError::BadSignature)),
        "a tampered manifest must fail the signature check, got {result:?}"
    );
}

#[tokio::test]
async fn reports_missing_manifest() {
    let (_, public) = generate_keypair().unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    let server = TestServer::start(HashMap::new());
    let manifest_url = server.url("/latest.json");
    let signature_url = server.url("/latest.json.sig");
    let result = check_signed_manifest(CheckRequest {
        manifest_url: &manifest_url,
        signature_url: &signature_url,
        key: &key,
        cancel: CancelToken::new(),
    })
    .await;
    assert!(matches!(
        result,
        Err(UpdateError::HttpStatus { status: 404, .. })
    ));
}

#[tokio::test]
async fn rejects_signature_from_another_origin() {
    let (_, public) = generate_keypair().unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    let server = TestServer::start(HashMap::new());
    let manifest_url = server.url("/latest.json");
    let result = check_signed_manifest(CheckRequest {
        manifest_url: &manifest_url,
        signature_url: "http://127.0.0.1:1/latest.json.sig",
        key: &key,
        cancel: CancelToken::new(),
    })
    .await;
    assert!(matches!(result, Err(UpdateError::InvalidArgument(_))));
}

#[tokio::test]
async fn rejects_remote_plain_http() {
    let (_, public) = generate_keypair().unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    let result = check_signed_manifest(CheckRequest {
        manifest_url: "http://example.invalid/latest.json",
        signature_url: "http://example.invalid/latest.json.sig",
        key: &key,
        cancel: CancelToken::new(),
    })
    .await;
    assert!(matches!(result, Err(UpdateError::InsecureUrl { .. })));
}
