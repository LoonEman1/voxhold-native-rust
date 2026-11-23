//! Staging regressions: claims, cleanup safety and the offline proof the
//! helper repeats before it applies.

use super::*;
use crate::manifest::{FullPackage, SetupPackage, BETA_CHANNEL};
use crate::verify::UpdatePublicKey;

#[test]
fn staging_is_bound_to_the_installation() {
    let root = Path::new(r"C:\Users\test\AppData\Local\VoxHold\updates");
    let a = Path::new(r"C:\Games\Voxhold-A");
    let b = Path::new(r"C:\Games\Voxhold-B");
    assert_ne!(installation_id(a), installation_id(b));
    assert_ne!(
        staging_dir(root, a, &Version::new(0, 2, 0)),
        staging_dir(root, b, &Version::new(0, 2, 0))
    );
    assert!(staging_dir(root, a, &Version::new(0, 2, 0))
        .ends_with(Path::new(&installation_id(a)).join("0.2.0")));
}

fn write_claim(staging: &Path, body: impl AsRef<[u8]>) {
    std::fs::write(staging.join(ACTIVE_FILE), body).unwrap();
}

fn current_claim(age: u64) -> Vec<u8> {
    serde_json::to_vec(&ActiveMarker {
        pid: std::process::id(),
        created_at: now_seconds().saturating_sub(age),
    })
    .unwrap()
}

#[test]
fn cleanup_removes_old_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    std::fs::create_dir_all(root.join("0.1.0").join(APP_DIR)).unwrap();
    std::fs::create_dir_all(root.join("0.2.0").join(APP_DIR)).unwrap();
    std::fs::write(root.join("stray.part"), b"partial").unwrap();
    assert_eq!(cleanup_stale(&root, Some("0.2.0")).unwrap(), 2);
    assert!(!root.join("0.1.0").exists());
    assert!(root.join("0.2.0").exists());
    assert!(!root.join("stray.part").exists());
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(root.exists());
}

#[test]
fn cleanup_skips_a_directory_claimed_by_an_active_download() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let active = root.join("0.2.0");
    std::fs::create_dir_all(active.join(APP_DIR)).unwrap();
    write_claim(&active, current_claim(0));
    std::fs::write(active.join(PACKAGE_FILE), b"half").unwrap();
    std::fs::create_dir_all(root.join("0.1.0")).unwrap();

    assert!(staging_is_active(&active));
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(active.exists(), "the active download is preserved");
    assert!(active.join(PACKAGE_FILE).exists());
    assert!(!root.join("0.1.0").exists());

    // Once the owner releases the directory it becomes removable.
    std::fs::remove_file(active.join(ACTIVE_FILE)).unwrap();
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(!active.exists());
}

#[test]
fn a_claim_left_by_a_dead_process_does_not_block_a_retry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let stale = root.join("0.2.0");
    std::fs::create_dir_all(stale.join(APP_DIR)).unwrap();
    // `u32::MAX` is not a live process id.
    write_claim(&stale, u32::MAX.to_string());
    std::fs::write(stale.join(PACKAGE_FILE), b"half").unwrap();

    assert!(!staging_is_active(&stale));
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(!stale.exists(), "a crashed download is cleaned up");
}

#[test]
fn an_unreadable_or_empty_claim_is_never_cleaned() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let garbage = root.join("0.2.0");
    let empty = root.join("0.3.0");
    for path in [&garbage, &empty] {
        std::fs::create_dir_all(path.join(APP_DIR)).unwrap();
        std::fs::write(path.join(PACKAGE_FILE), b"half").unwrap();
    }
    // Invalid UTF-8 cannot be a marker this build understands.
    write_claim(&garbage, [0xff, 0xfe, 0x00]);
    write_claim(&empty, b"");

    assert!(staging_is_active(&garbage));
    assert!(staging_is_active(&empty));
    assert_eq!(
        cleanup_stale(&root, None).unwrap(),
        0,
        "a claim cleanup cannot understand is preserved"
    );
    assert!(garbage.exists());
    assert!(empty.exists());
}

#[test]
fn an_expired_claim_does_not_block_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let stale = root.join("0.2.0");
    std::fs::create_dir_all(stale.join(APP_DIR)).unwrap();
    // The PID is alive (it is this process) but the claim is older than the
    // limit: a reused process id must not keep an abandoned tree forever.
    write_claim(
        &stale,
        current_claim(ACTIVE_MAX_AGE.as_secs().saturating_add(60)),
    );

    assert!(!staging_is_active(&stale));
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(!stale.exists());
}

#[test]
fn cleanup_keeps_a_young_staging_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let young = root.join(format!("0.2.0{STAGING_TEMP_MARKER}4242"));
    std::fs::create_dir_all(young.join(APP_DIR)).unwrap();
    std::fs::create_dir_all(root.join("0.1.0")).unwrap();

    // The tree is being assembled: it has no marker yet, but the cleanup that
    // runs between `create_dir_all` and the claim must not delete it.
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(young.exists(), "a fresh staging tree is preserved");
    assert!(!root.join("0.1.0").exists());
}

#[test]
fn the_legacy_updates_layout_is_removed_but_installation_ids_stay() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("updates");
    let id = "0123456789abcdef";
    std::fs::create_dir_all(base.join("0.1.0").join(APP_DIR)).unwrap();
    std::fs::create_dir_all(base.join(id).join("0.2.0").join(APP_DIR)).unwrap();
    std::fs::create_dir_all(base.join("staging-notes")).unwrap();
    std::fs::write(base.join("helper-usage.log"), b"log").unwrap();
    let busy = base.join("0.3.0");
    std::fs::create_dir_all(&busy).unwrap();
    write_claim(&busy, current_claim(0));

    assert_eq!(cleanup_legacy_layout(&base).unwrap(), 2);
    assert!(!base.join("0.1.0").exists());
    assert!(!base.join("helper-usage.log").exists());
    assert!(base.join(id).exists(), "installation ids are never touched");
    assert!(base.join("staging-notes").exists());
    assert!(busy.exists(), "an active legacy download is preserved");
}

#[cfg(windows)]
struct LockedFile(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for LockedFile {
    fn drop(&mut self) {
        // SAFETY: the handle was created by `CreateFileW` and is closed once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn lock_without_sharing(path: &Path) -> LockedFile {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
    };

    const GENERIC_READ: u32 = 0x8000_0000;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE, "CreateFileW failed");
    LockedFile(handle)
}

/// The first start after an update still sees a relocated helper image
/// that waits for its handshake. Its version directory must be skipped
/// while the neighbouring leftovers are removed.
#[cfg(windows)]
#[test]
fn cleanup_stale_skips_a_locked_entry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("updates");
    let busy = root.join("0.1.0");
    std::fs::create_dir_all(&busy).unwrap();
    let helper = busy.join(crate::apply::HELPER_FILE);
    std::fs::write(&helper, b"helper image").unwrap();
    std::fs::create_dir_all(root.join("0.2.0").join(APP_DIR)).unwrap();
    std::fs::write(root.join("stray.part"), b"partial").unwrap();

    // Emulates a running helper image: the file is opened without sharing,
    // so `remove_dir_all` cannot delete its directory.
    let lock = lock_without_sharing(&helper);
    let removed = cleanup_stale(&root, None).unwrap();
    assert_eq!(removed, 2);
    assert!(busy.exists());
    assert!(helper.exists());
    assert!(!root.join("0.2.0").exists());
    assert!(!root.join("stray.part").exists());

    drop(lock);
    assert_eq!(cleanup_stale(&root, None).unwrap(), 1);
    assert!(!busy.exists());
}

#[test]
fn payload_cleanup_keeps_the_relocated_helper() {
    let temp = tempfile::tempdir().unwrap();
    let staging = temp.path().join("0.2.0");
    std::fs::create_dir_all(staging.join(APP_DIR)).unwrap();
    std::fs::write(staging.join(APP_DIR).join("Voxhold.exe"), b"exe").unwrap();
    std::fs::write(staging.join(PACKAGE_FILE), b"zip").unwrap();
    std::fs::write(staging.join(READY_FILE), b"{}").unwrap();
    std::fs::write(staging.join(crate::apply::HELPER_FILE), b"helper").unwrap();
    std::fs::write(staging.join(APPLY_LOG), b"log").unwrap();

    cleanup_after_install_payload(&staging).unwrap();
    assert!(!staging.join(APP_DIR).exists());
    assert!(!staging.join(PACKAGE_FILE).exists());
    assert!(!staging.join(READY_FILE).exists());
    assert!(staging.join(crate::apply::HELPER_FILE).exists());
    assert!(staging.join(APPLY_LOG).exists());
}

#[cfg(windows)]
#[test]
fn updates_root_requires_local_appdata() {
    assert!(matches!(
        updates_root_from_base(None),
        Err(UpdateError::InvalidArgument(_))
    ));
    assert!(matches!(
        updates_root_from_base(Some(std::ffi::OsStr::new(""))),
        Err(UpdateError::InvalidArgument(_))
    ));
    let root =
        updates_root_from_base(Some(std::ffi::OsStr::new(r"C:\Users\test\AppData\Local"))).unwrap();
    assert!(root.ends_with(Path::new("VoxHold").join("updates")));
}

/// A signed staging tree whose marker points at `install`.
fn signed_staging(temp: &Path, install: &Path) -> (PathBuf, UpdatePublicKey) {
    let source = temp.join("package-source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("Voxhold.exe"), b"app").unwrap();
    let packed = crate::package::pack_with_index(&source, "9.9.9").unwrap();
    let staging = temp.join("updates").join("9.9.9");
    let app = staging.join(APP_DIR);
    crate::package::unpack(&packed.bytes, &app).unwrap();
    std::fs::write(staging.join(PACKAGE_FILE), &packed.bytes).unwrap();

    let manifest = crate::manifest::Manifest {
        schema: 1,
        channel: BETA_CHANNEL.into(),
        version: "9.9.9".into(),
        min_windows: "10.0.19041".into(),
        published_at: "2026-11-19T12:00:00Z".into(),
        notes: "Заметки".into(),
        authenticode: false,
        full: FullPackage {
            url: "https://example.invalid/full.zip".into(),
            size: packed.size(),
            sha256: packed.sha256.clone(),
            package_sha256: packed.sha256.clone(),
        },
        setup: SetupPackage {
            url: "https://example.invalid/Setup.exe".into(),
            size: 10,
            sha256: "b".repeat(64),
        },
        deltas: Vec::new(),
    };
    let bytes = manifest.to_json();
    let (private, public) = crate::verify::generate_keypair().unwrap();
    let signature = crate::verify::sign_bytes(&private, &bytes).unwrap();
    std::fs::write(staging.join(MANIFEST_FILE), &bytes).unwrap();
    std::fs::write(staging.join(SIGNATURE_FILE), signature.as_bytes()).unwrap();
    let marker = ReadyMarker {
        version: "9.9.9".into(),
        package_sha256: packed.sha256,
        install_dir: install.to_string_lossy().into_owned(),
        created_at: 1,
        method: StageMethod::Full,
        fallback_reason: None,
        manifest_sha256: package::sha256_hex(&bytes),
        manifest_signature: signature,
    };
    std::fs::write(
        staging.join(READY_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    (staging, UpdatePublicKey::from_base64(&public).unwrap())
}

#[test]
fn the_helper_repeats_the_staged_proof_before_applying() {
    let temp = tempfile::tempdir().unwrap();
    let install = temp.path().join("VoxHold");
    std::fs::create_dir_all(&install).unwrap();
    let (staging, key) = signed_staging(temp.path(), &install);

    let verified = verify_staged_proof_with(&staging, &install, &key).unwrap();
    assert_eq!(verified.version, Version::new(9, 9, 9));
    assert!(verified.verified);
    // The helper uses the embedded release key, which did not sign this
    // throw-away manifest.
    assert!(verify_staged_proof(&staging, &install).is_err());

    // `package.json` is not listed in its own file table. A replacement after
    // the app's check must cancel the apply, not be copied into the install.
    std::fs::write(
        staging.join(APP_DIR).join(package::PACKAGE_JSON),
        br#"{"version":"9.9.9","files":[]}"#,
    )
    .unwrap();
    assert!(matches!(
        verify_staged_proof_with(&staging, &install, &key),
        Err(UpdateError::InvalidPackage(_))
    ));
}
