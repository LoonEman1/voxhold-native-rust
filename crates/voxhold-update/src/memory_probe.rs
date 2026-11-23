//! Ignored peak-memory probes for the staging paths.
//!
//! `GetProcessMemoryInfo` reports the peak for the whole process lifetime, so
//! every case must run in its own process:
//!
//! ```text
//! # real portable ZIP, full download + hash + streaming unpack
//! $env:VOXHOLD_MEMORY_CASE = "full"
//! cargo test -p voxhold-update --locked memory_probe -- --ignored --nocapture
//!
//! # previous in-memory path (`std::fs::read` + `unpack`), for comparison
//! $env:VOXHOLD_MEMORY_CASE = "legacy"
//! cargo test -p voxhold-update --locked memory_probe -- --ignored --nocapture
//!
//! # synthetic delta: prepare once, then measure the delta run separately
//! $env:VOXHOLD_MEMORY_PREPARE_DIR = "C:\vx-delta"
//! cargo test -p voxhold-update --locked memory_probe -- --ignored --nocapture
//! $env:VOXHOLD_MEMORY_CASE = "delta"
//! $env:VOXHOLD_MEMORY_DELTA_DIR = "C:\vx-delta"
//! cargo test -p voxhold-update --locked memory_probe -- --ignored --nocapture
//! ```
//!
//! `VOXHOLD_MEMORY_ZIP` overrides the portable ZIP path; without it the probe
//! uses `%USERPROFILE%\Downloads\VoxHold-portable.zip` (`$HOME` outside
//! Windows) and fails with a clear message when that file is missing. The ZIP
//! itself is never committed or copied into the repository; it is served from
//! a loopback HTTP listener so the real download code runs.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Instant;

use semver::Version;

use crate::delta;
use crate::download::CancelToken;
use crate::manifest::{
    DeltaEntry, FullPackage, Manifest, SetupPackage, VerifiedManifest, BETA_CHANNEL,
};
use crate::package;
use crate::stage::{stage_update, StageOutcome, StageRequest};

#[test]
#[ignore = "manual peak-memory probe; run one case per process"]
fn memory_probe() {
    if let Some(dir) = std::env::var_os("VOXHOLD_MEMORY_PREPARE_DIR") {
        let summary = prepare_synthetic(Path::new(&dir));
        println!("memory-probe: prepared {summary}");
        return;
    }

    let case = std::env::var("VOXHOLD_MEMORY_CASE").unwrap_or_else(|_| "full".into());
    let zip = match case.as_str() {
        "delta" => None,
        _ => Some(memory_zip()),
    };
    let archive_bytes = zip
        .as_ref()
        .map(|path| std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))
        .unwrap_or(0);
    let archive = zip
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".into());
    let started = Instant::now();
    let (result, synthetic_generation) = match case.as_str() {
        "legacy" => (
            probe_legacy(zip.as_ref().expect("legacy needs a ZIP")),
            false,
        ),
        "delta" => probe_delta(),
        _ => (probe_full(zip.as_ref().expect("full needs a ZIP")), false),
    };
    let (working_set, pagefile) = peak_memory();
    println!(
        "memory-probe: case={case} archive={archive} archive_bytes={archive_bytes} \
         peak_working_set_bytes={working_set} peak_working_set_mib={:.1} \
         peak_pagefile_bytes={pagefile} peak_pagefile_mib={:.1} \
         synthetic_generation={synthetic_generation} elapsed_ms={} result={result}",
        working_set as f64 / (1024.0 * 1024.0),
        pagefile as f64 / (1024.0 * 1024.0),
        started.elapsed().as_millis(),
    );
    assert!(result.starts_with("ok"), "memory probe failed: {result}");
}

/// Portable ZIP for the probe: `VOXHOLD_MEMORY_ZIP`, else the user's
/// `Downloads` folder. There is no machine-specific path in the repository.
fn memory_zip() -> PathBuf {
    if let Some(path) = std::env::var_os("VOXHOLD_MEMORY_ZIP") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            panic!(
                "memory-probe: VOXHOLD_MEMORY_ZIP points at {}, which is not a file",
                path.display()
            );
        }
        return path;
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "memory-probe: set VOXHOLD_MEMORY_ZIP to a portable ZIP; \
                 USERPROFILE and HOME are not set"
            )
        });
    let path = home.join("Downloads").join("VoxHold-portable.zip");
    if !path.is_file() {
        panic!(
            "memory-probe: {} is missing; set VOXHOLD_MEMORY_ZIP to a portable ZIP",
            path.display()
        );
    }
    path
}

#[cfg(windows)]
fn peak_memory() -> (usize, usize) {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the counter structure is sized by `cb` and the pseudo handle is
    // always valid for the current process.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(ok, 0, "GetProcessMemoryInfo failed");
    (counters.PeakWorkingSetSize, counters.PeakPagefileUsage)
}

fn probe_legacy(zip: &Path) -> String {
    let bytes = match std::fs::read(zip) {
        Ok(bytes) => bytes,
        Err(error) => return format!("error reading {}: {error}", zip.display()),
    };
    let temp = match tempfile::tempdir() {
        Ok(temp) => temp,
        Err(error) => return format!("error tempdir: {error}"),
    };
    match package::unpack(&bytes, temp.path()) {
        Ok(index) => format!("ok legacy-unpack {}", index.version),
        Err(error) => format!("error legacy-unpack: {error}"),
    }
}

fn probe_full(zip: &Path) -> String {
    let index = match package::read_package_json_from_file(zip) {
        Ok(index) => index,
        Err(error) => return format!("error reading package.json: {error}"),
    };
    let size = std::fs::metadata(zip).map(|meta| meta.len()).unwrap_or(0);
    let sha = match package::sha256_file(zip) {
        Ok(sha) => sha,
        Err(error) => return format!("error hashing archive: {error}"),
    };
    let base = serve_file(zip);
    let manifest = manifest(
        &index.version,
        format!("{base}/full.zip"),
        size,
        &sha,
        Vec::new(),
    );
    run_stage(manifest, &Version::new(0, 0, 0), None, None)
}

fn probe_delta() -> (String, bool) {
    let prepared = std::env::var_os("VOXHOLD_MEMORY_DELTA_DIR").map(PathBuf::from);
    let (dir, generated, _temp) = match prepared {
        Some(dir) => (dir, false, None),
        None => {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(error) => return (format!("error tempdir: {error}"), false),
            };
            let summary = prepare_synthetic(temp.path());
            println!("memory-probe: generated synthetic packages: {summary}");
            (temp.path().to_path_buf(), true, Some(temp))
        }
    };
    let base_zip = dir.join("base.zip");
    let target_zip = dir.join("target.zip");
    let delta_zip = dir.join("delta.zip");
    let install = dir.join("install");
    let base_index = match package::read_package_json_from_dir(&install) {
        Ok(index) => index,
        Err(error) => return (format!("error reading base index: {error}"), generated),
    };
    let target_index = match package::read_package_json_from_file(&target_zip) {
        Ok(index) => index,
        Err(error) => return (format!("error reading target index: {error}"), generated),
    };
    let base_sha = match package::sha256_file(&base_zip) {
        Ok(sha) => sha,
        Err(error) => return (format!("error hashing base: {error}"), generated),
    };
    let target_sha = match package::sha256_file(&target_zip) {
        Ok(sha) => sha,
        Err(error) => return (format!("error hashing target: {error}"), generated),
    };
    let delta_sha = match package::sha256_file(&delta_zip) {
        Ok(sha) => sha,
        Err(error) => return (format!("error hashing delta: {error}"), generated),
    };
    let target_size = std::fs::metadata(&target_zip)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let delta_size = std::fs::metadata(&delta_zip)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let full_base = serve_file(&target_zip);
    let delta_base = serve_file(&delta_zip);
    let entry = DeltaEntry {
        from_version: base_index.version.clone(),
        from_package_sha256: base_sha,
        url: format!("{delta_base}/delta.zip"),
        size: delta_size,
        sha256: delta_sha,
    };
    let manifest = manifest(
        &target_index.version,
        format!("{full_base}/target.zip"),
        target_size,
        &target_sha,
        vec![entry],
    );
    let current = Version::parse(&base_index.version).unwrap_or_else(|_| Version::new(0, 0, 0));
    (
        run_stage(manifest, &current, Some(&install), Some(&base_index)),
        generated,
    )
}

fn manifest(
    version: &str,
    url: String,
    size: u64,
    sha256: &str,
    deltas: Vec<DeltaEntry>,
) -> Manifest {
    Manifest {
        schema: 1,
        channel: BETA_CHANNEL.into(),
        version: version.into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "memory probe".into(),
        authenticode: false,
        full: FullPackage {
            url,
            size,
            sha256: sha256.into(),
            package_sha256: sha256.into(),
        },
        setup: SetupPackage {
            url: "https://example.invalid/Setup.exe".into(),
            size: 1,
            sha256: "b".repeat(64),
        },
        deltas,
    }
}

fn run_stage(
    manifest: Manifest,
    current: &Version,
    install_override: Option<&Path>,
    installed: Option<&package::PackageIndex>,
) -> String {
    let work = match tempfile::tempdir() {
        Ok(work) => work,
        Err(error) => return format!("error tempdir: {error}"),
    };
    let install_dir = match install_override {
        Some(path) => path.to_path_buf(),
        None => work.path().join("install"),
    };
    if let Err(error) = std::fs::create_dir_all(&install_dir) {
        return format!("error install dir: {error}");
    }
    let root = work.path().join("updates");
    let verified = VerifiedManifest::new(manifest);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return format!("error runtime: {error}"),
    };
    let outcome = runtime.block_on(stage_update(StageRequest {
        manifest: &verified,
        channel: BETA_CHANNEL,
        current_version: current,
        install_dir: &install_dir,
        installed,
        updates_root: &root,
        cancel: CancelToken::new(),
        progress: None,
        phase: None,
    }));
    match outcome {
        StageOutcome::Ready(staged) => {
            let fallback = crate::stage::read_ready(&staged.staging_dir)
                .ok()
                .and_then(|marker| marker.fallback_reason);
            format!(
                "ok {:?} version={} sha={} fallback={fallback:?}",
                staged.method, staged.version, staged.package_sha256
            )
        }
        StageOutcome::UpToDate => "ok up-to-date".into(),
        StageOutcome::Failed(error) => format!("error stage: {error}"),
    }
}

fn prepare_synthetic(dir: &Path) -> String {
    let base_dir = dir.join("base-src");
    let target_dir = dir.join("target-src");
    let install = dir.join("install");
    if let Err(error) = std::fs::create_dir_all(&base_dir) {
        return format!("error base dir: {error}");
    }
    let executable = synthetic_bytes(8 * 1024 * 1024, 0x1234_5678);
    if let Err(error) = std::fs::write(base_dir.join("Voxhold.exe"), &executable) {
        return format!("error base exe: {error}");
    }
    if let Err(error) = std::fs::write(base_dir.join("README.md"), b"one") {
        return format!("error base readme: {error}");
    }
    let base = match package::pack_with_index(&base_dir, "0.1.0") {
        Ok(packed) => packed,
        Err(error) => return format!("error packing base: {error}"),
    };
    if let Err(error) = std::fs::create_dir_all(&target_dir) {
        return format!("error target dir: {error}");
    }
    let mut updated = executable;
    let start = 2 * 1024 * 1024;
    updated[start..start + 4096].fill(0xEE);
    if let Err(error) = std::fs::write(target_dir.join("Voxhold.exe"), &updated) {
        return format!("error target exe: {error}");
    }
    if let Err(error) = std::fs::write(target_dir.join("README.md"), b"two") {
        return format!("error target readme: {error}");
    }
    let target = match package::pack_with_index(&target_dir, "0.2.0") {
        Ok(packed) => packed,
        Err(error) => return format!("error packing target: {error}"),
    };
    let delta = match delta::create(&base.bytes, &target.bytes) {
        Ok(bytes) => bytes,
        Err(error) => return format!("error creating delta: {error}"),
    };
    if std::fs::create_dir_all(&install).is_err() {
        return "error install dir".into();
    }
    if let Err(error) = package::unpack(&base.bytes, &install) {
        return format!("error unpacking base: {error}");
    }
    for (name, bytes) in [
        ("base.zip", base.bytes.as_slice()),
        ("target.zip", target.bytes.as_slice()),
        ("delta.zip", delta.as_slice()),
    ] {
        if let Err(error) = std::fs::write(dir.join(name), bytes) {
            return format!("error writing {name}: {error}");
        }
    }
    format!(
        "dir={} base={} target={} delta={} install={}",
        dir.display(),
        base.bytes.len(),
        target.bytes.len(),
        delta.len(),
        install.display()
    )
}

fn synthetic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Serve one file over `127.0.0.1` with a fixed `Content-Length`.
fn serve_file(path: &Path) -> String {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("loopback listener failed: {error}"),
    };
    let port = match listener.local_addr() {
        Ok(address) => address.port(),
        Err(error) => panic!("loopback address failed: {error}"),
    };
    let path = path.to_path_buf();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = respond(&path, stream);
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn respond(path: &Path, mut stream: TcpStream) -> std::io::Result<()> {
    let mut request = [0u8; 4096];
    let _ = stream.read(&mut request)?;
    let size = std::fs::metadata(path)?.len();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nContent-Type: application/zip\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes())?;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        stream.write_all(&buffer[..read])?;
    }
    Ok(())
}
