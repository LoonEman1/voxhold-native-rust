mod support;

use std::collections::HashMap;

use support::{Route, TestServer};
use voxhold_update::download::{download, part_path, CancelToken, DownloadRequest};
use voxhold_update::error::UpdateError;
use voxhold_update::package::sha256_hex;

fn payload(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

#[tokio::test]
async fn resumes_partial_download_with_range() {
    let body = payload(200 * 1024);
    let digest = sha256_hex(&body);
    let mut routes = HashMap::new();
    routes.insert(
        "/pkg.zip".to_string(),
        Route {
            body: body.clone(),
            interrupt_after: Some(64 * 1024),
            allow_range: true,
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("pkg.zip");
    let url = server.url("/pkg.zip");

    let first = download(DownloadRequest {
        url: &url,
        dest: &dest,
        expected_size: body.len() as u64,
        expected_sha256: &digest,
        max_bytes: body.len() as u64 + 1024,
        cancel: CancelToken::new(),
        progress: None,
    })
    .await;
    assert!(matches!(first, Err(UpdateError::Network(_))), "{first:?}");
    let part = part_path(&dest);
    let partial = std::fs::metadata(&part).unwrap().len();
    assert!(
        partial > 0 && partial < body.len() as u64,
        "partial {partial}"
    );

    let second = download(DownloadRequest {
        url: &url,
        dest: &dest,
        expected_size: body.len() as u64,
        expected_sha256: &digest,
        max_bytes: body.len() as u64 + 1024,
        cancel: CancelToken::new(),
        progress: None,
    })
    .await
    .unwrap();
    assert_eq!(second.bytes, body.len() as u64);
    assert_eq!(second.resumed_from, partial);
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert!(!part.exists());

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].range.as_deref(),
        Some(format!("bytes={partial}-").as_str())
    );
    assert_eq!(requests[1].status, 206);
}

#[tokio::test]
async fn hash_mismatch_removes_artifact() {
    let body = payload(32 * 1024);
    let mut routes = HashMap::new();
    routes.insert(
        "/pkg.zip".to_string(),
        Route {
            body: body.clone(),
            allow_range: true,
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("pkg.zip");
    let url = server.url("/pkg.zip");
    let result = download(DownloadRequest {
        url: &url,
        dest: &dest,
        expected_size: body.len() as u64,
        expected_sha256: &"0".repeat(64),
        max_bytes: body.len() as u64 + 1024,
        cancel: CancelToken::new(),
        progress: None,
    })
    .await;
    assert!(matches!(result, Err(UpdateError::HashMismatch { .. })));
    assert!(!dest.exists());
    assert!(!part_path(&dest).exists());
}

#[tokio::test]
async fn cancellation_removes_partial_file() {
    let body = payload(4096);
    let mut routes = HashMap::new();
    routes.insert(
        "/pkg.zip".to_string(),
        Route {
            body: body.clone(),
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("pkg.zip");
    let part = part_path(&dest);
    std::fs::write(&part, b"partial").unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let url = server.url("/pkg.zip");
    let result = download(DownloadRequest {
        url: &url,
        dest: &dest,
        expected_size: body.len() as u64,
        expected_sha256: &sha256_hex(&body),
        max_bytes: body.len() as u64 + 1024,
        cancel,
        progress: None,
    })
    .await;
    assert!(matches!(result, Err(UpdateError::Cancelled)));
    assert!(!part.exists());
}

#[tokio::test]
async fn reports_http_status_without_query() {
    let mut routes = HashMap::new();
    routes.insert(
        "/missing.zip".to_string(),
        Route {
            status: 404,
            ..Route::default()
        },
    );
    let server = TestServer::start(routes);
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("pkg.zip");
    let url = format!("{}?token=secret", server.url("/missing.zip"));
    let result = download(DownloadRequest {
        url: &url,
        dest: &dest,
        expected_size: 1024,
        expected_sha256: &"a".repeat(64),
        max_bytes: 2048,
        cancel: CancelToken::new(),
        progress: None,
    })
    .await;
    match result {
        Err(UpdateError::HttpStatus { url, status }) => {
            assert_eq!(status, 404);
            assert!(!url.contains("secret"));
            assert!(!url.contains('?'));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(!part_path(&dest).exists());
}

#[tokio::test]
async fn rejects_plain_http_remote_host() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("pkg.zip");
    let result = download(DownloadRequest {
        url: "http://example.invalid/pkg.zip",
        dest: &dest,
        expected_size: 10,
        expected_sha256: &"a".repeat(64),
        max_bytes: 100,
        cancel: CancelToken::new(),
        progress: None,
    })
    .await;
    assert!(matches!(result, Err(UpdateError::InsecureUrl { .. })));
}
