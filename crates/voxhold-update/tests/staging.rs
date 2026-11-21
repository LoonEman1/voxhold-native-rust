mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use voxhold_update::error::UpdateError;

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
    cleanup_after_install, read_ready, stage_update, StageMethod, StageOutcome, StagePhase,
    StageRequest,
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

/// Serve a delta body that does not match the valid delta used for the
/// manifest metadata (same length, declared digest over the served bytes).
/// The full package is throttled so the caller sees intermediate progress.
fn server_for_corrupt_delta(packages: &Packages, delta: Vec<u8>) -> TestServer {
    let mut routes = HashMap::new();
    routes.insert(
        "/full.zip".to_string(),
        Route {
            body: packages.target.clone(),
            allow_range: true,
            throttle: Some(Duration::from_millis(1)),
            ..Route::default()
        },
    );
    routes.insert(
        "/delta.zip".to_string(),
        Route {
            body: delta,
            allow_range: true,
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
        phase: None,
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
async fn rejects_a_package_with_a_different_version_than_the_signed_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, _) = install_base(temp.path(), &packages.base);
    let server = server_for(&packages);
    let mut manifest = build_manifest(&server, &packages.target, None, None, None);
    manifest.version = "0.3.0".into();
    let manifest = verified(&manifest);
    let root = temp.path().join("updates");

    let outcome = stage_update(request(&manifest, &install, None, &root)).await;
    assert!(matches!(
        outcome,
        StageOutcome::Failed(UpdateError::InvalidPackage(_))
    ));
    assert!(!root.join("0.3.0").exists());
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
async fn a_failed_delta_reports_downloading_and_progress_for_the_full_package() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);

    // The served body has the declared digest but is not a valid delta, so the
    // download finishes (and reports `Preparing`) before the fallback starts.
    let mut delta_body = packages.delta.clone();
    let last = delta_body.len() - 1;
    delta_body[last] ^= 0xFF;
    let server = server_for_corrupt_delta(&packages, delta_body.clone());
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        Some(delta_body.len() as u64),
        Some(sha256_hex(&delta_body)),
    ));
    let root = temp.path().join("updates");

    let phases = Arc::new(Mutex::new(Vec::new()));
    let samples = Arc::new(Mutex::new(Vec::new()));
    let mut request = request(&manifest, &install, Some(&index), &root);
    let phase_log = phases.clone();
    request.phase = Some(Arc::new(move |phase| {
        phase_log.lock().expect("phase log").push(phase);
    }));
    let sample_log = samples.clone();
    request.progress = Some(Arc::new(move |downloaded, total| {
        sample_log
            .lock()
            .expect("progress log")
            .push((downloaded, total));
    }));

    let outcome = stage_update(request).await;
    let staged = match outcome {
        StageOutcome::Ready(staged) => staged,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(staged.method, StageMethod::FullFallback);

    // The delta reports its body, then the fallback announces a fresh full
    // download with a new total instead of leaving the UI in `Preparing`.
    let phases = phases.lock().expect("phase log").clone();
    assert_eq!(
        phases,
        vec![
            StagePhase::Downloading {
                total: delta_body.len() as u64
            },
            StagePhase::Preparing,
            StagePhase::Downloading {
                total: packages.target.len() as u64
            },
            StagePhase::Preparing,
        ]
    );

    let full_total = packages.target.len() as u64;
    let samples = samples.lock().expect("progress log").clone();
    let full: Vec<(u64, Option<u64>)> = samples
        .iter()
        .copied()
        .filter(|(_, total)| *total == Some(full_total))
        .collect();
    assert!(
        full.iter()
            .any(|(downloaded, _)| *downloaded > 0 && *downloaded < full_total),
        "the throttled full download reports intermediate bytes: {full:?}"
    );
    assert!(
        full.iter().all(|(downloaded, _)| *downloaded <= full_total),
        "no full sample exceeds its total: {full:?}"
    );
    assert!(
        full.iter().any(|(downloaded, _)| *downloaded == full_total),
        "the full download reaches its total: {full:?}"
    );
    assert!(
        samples
            .iter()
            .any(|(_, total)| *total == Some(packages.delta.len() as u64)),
        "the delta keeps its own total: {samples:?}"
    );
}

#[tokio::test]
async fn cancelling_during_the_full_fallback_stops_before_the_body_is_requested() {
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

    let full_total = packages.target.len() as u64;
    let token = CancelToken::new();
    let cancellation = token.clone();
    let mut request = request(&manifest, &install, Some(&index), &root);
    request.cancel = token;
    request.phase = Some(Arc::new(move |phase| {
        if let StagePhase::Downloading { total } = phase {
            if total == full_total {
                cancellation.cancel();
            }
        }
    }));

    let outcome = stage_update(request).await;
    assert!(
        matches!(outcome, StageOutcome::Failed(UpdateError::Cancelled)),
        "the fallback must be cancellable, got {outcome:?}"
    );
    assert!(!root.join("0.2.0").exists());
    let requests = server.requests();
    assert!(requests.iter().any(|request| request.path == "/delta.zip"));
    assert!(
        !requests.iter().any(|request| request.path == "/full.zip"),
        "a cancelled fallback never starts the body: {requests:?}"
    );
}

#[tokio::test]
async fn cancelling_while_preparing_skips_the_fallback_and_removes_staging() {
    let temp = tempfile::tempdir().unwrap();
    let packages = packages(temp.path());
    let (install, index) = install_base(temp.path(), &packages.base);
    let mut delta_body = packages.delta.clone();
    let last = delta_body.len() - 1;
    delta_body[last] ^= 0xFF;
    let server = server_for_corrupt_delta(&packages, delta_body.clone());
    let manifest = verified(&build_manifest(
        &server,
        &packages.target,
        Some(&packages.delta),
        Some(delta_body.len() as u64),
        Some(sha256_hex(&delta_body)),
    ));
    let root = temp.path().join("updates");

    let token = CancelToken::new();
    let cancellation = token.clone();
    let mut request = request(&manifest, &install, Some(&index), &root);
    request.cancel = token;
    // `Preparing` is exactly the window the panel now offers a cancel in.
    request.phase = Some(Arc::new(move |phase| {
        if phase == StagePhase::Preparing {
            cancellation.cancel();
        }
    }));

    let outcome = stage_update(request).await;
    assert!(
        matches!(outcome, StageOutcome::Failed(UpdateError::Cancelled)),
        "a cancel in Preparing must stop the update, got {outcome:?}"
    );
    assert!(!root.join("0.2.0").exists());
    let requests = server.requests();
    assert!(requests.iter().any(|request| request.path == "/delta.zip"));
    assert!(
        !requests.iter().any(|request| request.path == "/full.zip"),
        "a cancel in Preparing never starts the fallback download: {requests:?}"
    );
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
