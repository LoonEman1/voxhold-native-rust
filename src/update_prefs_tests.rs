//! Unit tests for [`UpdatePrefs`] and the auto-download decision.
//!
//! They live in their own file so `src/update.rs` stays below the repository
//! file-size limit.

use super::*;

#[test]
fn preferences_round_trip_atomically() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-update-prefs-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join(PREFS_FILE);
    let mut prefs = UpdatePrefs::default();
    prefs.set_declined(&Version::new(0, 2, 0));
    prefs.auto_download = false;
    prefs.install_on_next_start = false;
    save_prefs(Some(&path), &prefs).unwrap();
    let loaded = load_prefs(Some(&path));
    assert_eq!(loaded, prefs);
    assert_eq!(loaded.declined(), Some(Version::new(0, 2, 0)));
    assert!(!loaded.auto_download);
    assert!(!loaded.install_on_next_start);
    std::fs::write(&path, b"not json").unwrap();
    assert!(load_prefs(Some(&path)).declined().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn preferences_default_to_the_background_switches_on() {
    let prefs = UpdatePrefs::default();
    assert_eq!(prefs.declined_version, None);
    assert!(prefs.auto_download);
    assert!(prefs.install_on_next_start);
    // The `Default` impl must not be a derive: a missing field reads as
    // `true`, not as the `bool` default.
    let decoded: UpdatePrefs = serde_json::from_slice(b"{}").unwrap();
    assert!(decoded.auto_download);
    assert!(decoded.install_on_next_start);
}

#[test]
fn an_old_preferences_file_reads_missing_switches_as_on() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-update-prefs-old-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join(PREFS_FILE);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, br#"{"declined_version":"0.2.0"}"#).unwrap();
    let prefs = load_prefs(Some(&path));
    assert_eq!(prefs.declined(), Some(Version::new(0, 2, 0)));
    assert!(prefs.auto_download);
    assert!(prefs.install_on_next_start);

    // Saving a skipped version keeps both switches of the old file.
    let mut updated = prefs.clone();
    updated.set_declined(&Version::new(0, 3, 0));
    save_prefs(Some(&path), &updated).unwrap();
    let reloaded = load_prefs(Some(&path));
    assert_eq!(reloaded.declined(), Some(Version::new(0, 3, 0)));
    assert!(reloaded.auto_download);
    assert!(reloaded.install_on_next_start);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_preferences_file_keeps_the_background_switches_on() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-update-prefs-missing-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let prefs = load_prefs(Some(&dir.join("missing.json")));
    assert_eq!(prefs, UpdatePrefs::default());
    assert!(prefs.auto_download);
    assert!(prefs.install_on_next_start);
}

#[test]
fn explicit_false_switches_survive_a_reload() {
    let dir = std::env::temp_dir().join(format!(
        "voxhold-update-prefs-false-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let path = dir.join(PREFS_FILE);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &path,
        br#"{"declined_version":null,"auto_download":false,"install_on_next_start":false}"#,
    )
    .unwrap();
    let prefs = load_prefs(Some(&path));
    assert!(!prefs.auto_download);
    assert!(!prefs.install_on_next_start);
    assert_eq!(prefs.declined(), None);

    let mut updated = prefs.clone();
    updated.set_declined(&Version::new(0, 4, 0));
    save_prefs(Some(&path), &updated).unwrap();
    let reloaded = load_prefs(Some(&path));
    assert!(!reloaded.auto_download);
    assert!(!reloaded.install_on_next_start);
    assert_eq!(reloaded.declined(), Some(Version::new(0, 4, 0)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn auto_download_is_decided_once_per_version() {
    let prefs = UpdatePrefs::default();
    assert!(should_auto_download(&prefs, "0.2.0", None, None, None));
    // The user cancelled this version in this run.
    assert!(!should_auto_download(
        &prefs,
        "0.2.0",
        None,
        Some("0.2.0"),
        None
    ));
    assert!(should_auto_download(
        &prefs,
        "0.3.0",
        None,
        Some("0.2.0"),
        None
    ));
    // The automatic start already happened for this version.
    assert!(!should_auto_download(
        &prefs,
        "0.2.0",
        None,
        None,
        Some("0.2.0")
    ));
    // A skipped version is never downloaded automatically.
    let declined = Version::new(0, 2, 0);
    assert!(!should_auto_download(
        &prefs,
        "0.2.0",
        Some(&declined),
        None,
        None
    ));
    // The switch off wins over everything.
    let mut off = prefs.clone();
    off.auto_download = false;
    assert!(!should_auto_download(&off, "0.2.0", None, None, None));
}

#[test]
fn a_ready_note_follows_the_install_switch() {
    assert_eq!(ready_note(true), "Установится при следующем запуске");
    assert_eq!(ready_note(false), "Готово к установке");
}
