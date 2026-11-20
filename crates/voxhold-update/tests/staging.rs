mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use semver::Version;
use support::{Route, TestServer};
use voxhold_update::download::CancelToken;
use voxhold_update::manifest::{
    DeltaEntry, FullPackage, Manifest, SetupPackage, VerifiedManifest, BETA_CHANNEL,
};
use voxhold_update::package::{
    pack_with_index, read_package_json_from_dir, sha256_hex, unpack, PackageIndex,
};
use voxhold_update::stage::{
    cleanup_after_install, read_ready, stage_update, StageMethod, StageOutcome, StageRequest,
};
use voxhold_update::verify::{generate_keypair, sign_bytes, verify_and_parse, UpdatePublicKey};

fn binary(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
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

fn make_package(root: &Path, version: &str, exe: &[u8], note: &str) -> Vec<u8> {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("Voxhold.exe"), exe).unwrap();
    std::fs::write(root.join("README.md"), note).unwrap();
    pack_with_index(root, version).unwrap().bytes
}

struct Packages {
    base: Vec<u8>,
    target: Vec<u8>,
    delta: Vec<u8>,
}

fn packages(temp: &Path) -> Packages {
    let base_dir = temp.join("base");
    let target_dir = temp.join("target");
    let exe = binary(256 * 1024, 0xDEAD_BEEF);
    let base = make_package(&base_dir, "0.1.0", &exe, "one");
    let mut updated = exe.clone();
    updated[4096..4160].fill(0x42);
    let target = make_package(&target_dir, "0.2.0", &updated, "two");
    let delta = voxhold_update::delta::create(&base, &target).unwrap();
    Packages {
        base,
        target,
        delta,
    }
}

fn build_manifest(
    server: &TestServer,
    full: &[u8],
    delta: Option<&[u8]>,
    delta_size: Option<u64>,
    delta_sha: Option<String>,
) -> Manifest {
    let full_sha = sha256_hex(full);
    let deltas = delta
        .map(|bytes| {
            let parsed = voxhold_update::delta::read_manifest(bytes).unwrap();
            DeltaEntry {
                from_version: parsed.from_version,
                from_package_sha256: parsed.base_package_sha256,
                url: server.url("/delta.zip"),
                size: delta_size.unwrap_or(bytes.len() as u64),
                sha256: delta_sha.unwrap_or_else(|| sha256_hex(bytes)),
            }
        })
        .into_iter()
        .collect();
    Manifest {
        schema: 1,
        channel: BETA_CHANNEL.into(),
        version: "0.2.0".into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "test notes".into(),
        authenticode: false,
        full: FullPackage {
            url: server.url("/full.zip"),
            size: full.len() as u64,
            sha256: full_sha.clone(),
            package_sha256: full_sha,
        },
        setup: SetupPackage {
            url: server.url("/Setup.exe"),
            size: 5,
            sha256: sha256_hex(b"setup"),
        },
        deltas,
    }
}

/// Sign a manifest so staging accepts it, exactly like the client does.
fn verified(manifest: &Manifest) -> VerifiedManifest {
    let (private, public) = generate_keypair().unwrap();
    let bytes = manifest.to_json();
    let signature = sign_bytes(&private, &bytes).unwrap();
    let key = UpdatePublicKey::from_base64(&public).unwrap();
    verify_and_parse(&key, &bytes, &signature).unwrap()
}

fn install_base(temp: &Path, base: &[u8]) -> (PathBuf, PackageIndex) {
    let install = temp.join("VoxHold");
    let index = unpack(base, &install).unwrap();
    (install, index)
}

fn server_for(packages: &Packages) -> TestServer {
    server_for_with_interrupt(packages, None)
}

fn server_for_with_interrupt(packages: &Packages, interrupt_after: Option<usize>) -> TestServer {
    let mut routes = HashMap::new();
    routes.insert(
        "/full.zip".to_string(),
        Route {
            body: packages.target.clone(),
            allow_range: true,
            interrupt_after,
            ..Route::default()
        },
    );
    routes.insert(
        "/delta.zip".to_string(),
        Route {
            body: packages.delta.clone(),
            allow_range: true,
            ..Route::default()
        },
    );
    TestServer::start(routes)
}

fn server_for_always_interrupt(packages: &Packages, interrupt_after: usize) -> TestServer {
    let mut routes = HashMap::new();
    routes.insert(
        "/full.zip".to_string(),
        Route {
            body: packages.target.clone(),
            allow_range: true,
            interrupt_after: Some(interrupt_after),
            interrupt_every: true,
            ..Route::default()
        },
    );
    TestServer::start(routes)
}

fn request<'a>(
    manifest: &'a VerifiedManifest,
    install: &'a Path,
    installed: Option<&'a PackageIndex>,
    root: &'a Path,
) -> StageRequest<'a> {
    static CURRENT: Version = Version::new(0, 1, 0);
    StageRequest {
        manifest,
        channel: BETA_CHANNEL,
        current_version: &CURRENT,
        install_dir: install,
        installed,
        updates_root: root,
        cancel: CancelToken::new(),
        progress: None,
    }
}

#[tokio::test]
async fn stages_delta_then_cleans_up() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);
    let server = server_for(&packages);
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        None,
        None,
    ));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, Some(&index), &root)).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::Delta);
    let marker = read_ready(&staged.staging_dir).unwrap();
    assert_eq!(marker.version, "0.2.0");
    assert_eq!(marker.method, StageMethod::Delta);
    assert_eq!(
        read_package_json_from_dir(&staged.app_dir).unwrap().version,
        "0.2.0"
    );
    let requests = server.requests();
    assert!(requests.iter().any(|request| request.path == "/delta.zip"));
    assert!(!requests.iter().any(|request| request.path == "/full.zip"));

    cleanup_after_install(&staged.staging_dir).unwrap();
    assert!(!staged.staging_dir.exists());
}

#[tokio::test]
async fn falls_back_to_full_without_base() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, _) = install_base(temp.path(), &packages.base);
    let server = server_for(&packages);
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        None,
        None,
    ));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, None, &root)).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::Full);
    let requests = server.requests();
    assert!(requests.iter().any(|request| request.path == "/full.zip"));
    assert!(!requests.iter().any(|request| request.path == "/delta.zip"));
}

#[tokio::test]
async fn falls_back_when_delta_hash_is_wrong() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);
    let server = server_for(&packages);
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        None,
        Some("0".repeat(64)),
    ));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, Some(&index), &root)).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::FullFallback);
    let marker = read_ready(&staged.staging_dir).unwrap();
    assert!(marker.fallback_reason.is_some());
    let requests = server.requests();
    assert!(requests.iter().any(|request| request.path == "/delta.zip"));
    assert!(requests.iter().any(|request| request.path == "/full.zip"));
}

#[tokio::test]
async fn ignores_delta_that_is_not_smaller() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);
    let server = server_for(&packages);
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        Some(packages.target.len() as u64),
        None,
    ));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, Some(&index), &root)).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::Full);
    let requests = server.requests();
    assert!(!requests.iter().any(|request| request.path == "/delta.zip"));
}

#[tokio::test]
async fn failed_download_removes_staging() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, _) = install_base(temp.path(), &packages.base);
    let server = TestServer::start(HashMap::new());
    let manifest = verified(&build_manifest(&server, &packages.target, None, None, None));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, None, &root)).await;
    match outcome {
        StageOutcome::Failed(error) => {
            assert!(matches!(
                error,
                voxhold_update::UpdateError::HttpStatus { .. }
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(!root.join("0.2.0").exists());
}

#[tokio::test]
async fn full_download_resumes_after_interruption() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, _) = install_base(temp.path(), &packages.base);
    let server = server_for_with_interrupt(&packages, Some(32 * 1024));
    let manifest = verified(&build_manifest(&server, &packages.target, None, None, None));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, None, &root)).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::Full);
    let requests = server.requests();
    assert!(requests.len() >= 2, "{requests:?}");
    assert_eq!(requests[0].status, 200);
    assert!(
        requests.iter().skip(1).any(|request| request.status == 206),
        "{requests:?}"
    );
    assert!(requests.iter().skip(1).any(|request| request
        .range
        .as_deref()
        .is_some_and(|value| value.starts_with("bytes="))));
}

#[tokio::test]
async fn retries_a_network_error_once() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, _) = install_base(temp.path(), &packages.base);
    let server = server_for_always_interrupt(&packages, 0);
    let manifest = verified(&build_manifest(&server, &packages.target, None, None, None));
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, None, &root)).await;
    match outcome {
        StageOutcome::Failed(error) => {
            assert!(matches!(error, voxhold_update::UpdateError::Network(_)));
        }
        other => panic!("unexpected {other:?}"),
    }
    let attempts = server
        .requests()
        .iter()
        .filter(|request| request.path == "/full.zip")
        .count();
    assert_eq!(attempts, 2, "one retry means two attempts in total");
    assert!(!root.join("0.2.0").exists());
}

#[tokio::test]
async fn repacks_installed_package_for_delta() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);
    let repacked = voxhold_update::package::repack_installed(&install, &index).unwrap();
    assert_eq!(repacked.sha256, sha256_hex(&packages.base));
    assert_eq!(read_package_json_from_dir(&install).unwrap(), index);
}
