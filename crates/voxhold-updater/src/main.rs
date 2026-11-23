//! Helper process that applies a staged update after the client exits.
//!
//! Arguments:
//!
//! ```text
//! --pid <process id> --target <install dir> --staged <staging dir>
//! --exe <executable name> --handshake <file> [--ready <file>]
//! [--refusal <file>] [--version <version>]
//! [--monitor <left,top,right,bottom>] [--detached]
//! ```
//!
//! `--target`, `--staged` and `--handshake` must be absolute paths; `--target`
//! and `--staged` must already exist and are canonicalised before the process
//! changes its working directory. `--exe` is a plain file name inside the
//! staged `app` directory. `--monitor` is the work area of the monitor that
//! showed the client; the helper centres its own window in it. A rectangle that
//! no longer intersects a live monitor falls back to the primary work area,
//! exactly like an absent argument.
//!
//! `--staged` is the version directory (`...\updates\<version>`); the helper
//! uses `<staged>\app` as the prepared payload and writes `<staged>\apply.log`.
//! The log contains local paths and error codes only, never URLs or secrets.
//!
//! Windows cannot rename or delete a directory that contains a running image.
//! A helper started from the installation or from the staged `app` directory
//! therefore copies itself to `<staged>\helper.exe` and restarts there with
//! `--detached`; the parent exits immediately. Release builds use the Windows
//! subsystem so the helper never flashes a console window; failures are
//! reported through `apply.log` and the exit code.
//!
//! The helper owns the visible installation window (see `window.rs`). It is
//! created and shown before `--ready` is written, and the window thread enters
//! its message loop immediately after sending that readiness signal, so the
//! client only exits once the helper can already receive (or queue) stage
//! messages. After a confirmed handshake the window closes and the helper
//! exits.
//!
//! After a successful apply the helper removes the staged payload and then
//! runs a best-effort `cleanup_stale` over the updates root, keeping its own
//! version directory because the running image cannot be deleted. The
//! application removes the remainder on the next start or after its
//! post-handshake cleanup timer.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod monitor;
mod stages;
mod window;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use voxhold_update::apply::{
    apply_staged_observed, helper_relocation_destination, is_relocated_helper, relocate_helper,
    ApplyObserver, ApplyStage,
};
use voxhold_update::recovery::{recover_install, RecoveryMode, RecoveryOutcome};
use voxhold_update::stage::{
    cleanup_after_install, cleanup_after_install_payload, cleanup_stale, updates_root,
    verify_staged_proof, APPLY_LOG, APP_DIR,
};

const EXIT_OK: u8 = 0;
const EXIT_USAGE: u8 = 2;
const EXIT_WAIT: u8 = 3;
const EXIT_APPLY: u8 = 4;
const WAIT_LIMIT: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct Arguments {
    pid: u32,
    target: PathBuf,
    staged: PathBuf,
    exe: String,
    handshake: PathBuf,
    detached: bool,
    ready: Option<PathBuf>,
    refusal: Option<PathBuf>,
    version: Option<String>,
    monitor: Option<window::MonitorRect>,
}

fn main() -> ExitCode {
    let raw = match collect_arguments() {
        Ok(raw) => raw,
        Err(message) => {
            log_usage_error(&message);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let arguments = match parse_arguments(raw.iter().cloned()) {
        Ok(arguments) => arguments,
        Err(message) => {
            log_usage_error(&message);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let log_path = arguments.staged.join(APPLY_LOG);
    log(&log_path, "helper started");
    log(
        &log_path,
        &format!(
            "pid={} target={} staged={} exe={} detached={}",
            arguments.pid,
            arguments.target.display(),
            arguments.staged.display(),
            arguments.exe,
            arguments.detached
        ),
    );

    // Every path is absolute now, so leave the installation and staging trees
    // as the working directory; a current directory inside the target blocks
    // the rename.
    let _ = std::env::set_current_dir(std::env::temp_dir());

    let current = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            log(
                &log_path,
                &format!("cannot resolve own path: {}", error.kind()),
            );
            return ExitCode::from(EXIT_APPLY);
        }
    };
    if let Some(destination) =
        helper_relocation_destination(&current, &arguments.target, &arguments.staged)
    {
        if arguments.detached {
            log(
                &log_path,
                "refusing to run from the installation or staging directory",
            );
            return ExitCode::from(EXIT_APPLY);
        }
        if let Err(error) = relocate_helper(&current, &destination) {
            log(
                &log_path,
                &format!("cannot relocate helper: {}", error.code()),
            );
            return ExitCode::from(EXIT_APPLY);
        }
        log(
            &log_path,
            &format!("relocated helper to {}", destination.display()),
        );
        match spawn_detached(&destination, &raw) {
            Ok(()) => return ExitCode::from(EXIT_OK),
            Err(message) => {
                log(&log_path, &format!("cannot restart helper: {message}"));
                return ExitCode::from(EXIT_APPLY);
            }
        }
    }

    // DPI awareness must be selected before any window is created.
    window::set_dpi_awareness();
    let version = arguments
        .version
        .clone()
        .or_else(|| read_staged_version(&arguments.staged));
    let update_window = match window::spawn(arguments.monitor, version) {
        Ok(update_window) => Some(update_window),
        Err(message) => {
            log(
                &log_path,
                &format!("cannot create the update window: {message}"),
            );
            // The client waits for the marker; without a window it must stay
            // open and show the error instead of quitting.
            if arguments.ready.is_some() {
                return ExitCode::from(EXIT_APPLY);
            }
            None
        }
    };

    // Only now is the window created, shown and able to receive stage
    // messages, so the client may finish and let this process take over.
    if let Some(ready) = arguments.ready.as_deref() {
        if let Err(error) = write_marker(ready, b"ready\n") {
            log(
                &log_path,
                &format!("cannot write helper-ready marker: {error}"),
            );
            return ExitCode::from(EXIT_APPLY);
        }
    }
    if let Err(message) = wait_for_process(arguments.pid) {
        log(&log_path, &format!("wait failed: {message}"));
        return ExitCode::from(EXIT_WAIT);
    }
    log(&log_path, "client exited");

    if let Some(update_window) = &update_window {
        update_window.set_stage(stages::Stage::Verifying);
    }
    // Directory replacement belongs to the helper, and only after the
    // application exited. The same recovery runs in the app as an observer.
    match recover_install(&arguments.target, RecoveryMode::Updater) {
        Ok(outcome) => log(&log_path, &format!("recovery: {outcome:?}")),
        Err(error) => log(
            &log_path,
            &format!("recovery failed: {} ({error})", error.code()),
        ),
    }

    let app = arguments.staged.join(APP_DIR);
    // Repeat the offline proof here: the app checked it before handing over,
    // but the staging tree can still change between that check and the copy.
    // A replacement of `app/package.json` must cancel the apply.
    match verify_staged_proof(&arguments.staged, &arguments.target) {
        Ok(staged) => log(
            &log_path,
            &format!("staged proof verified for {}", staged.version),
        ),
        Err(error) => {
            log(
                &log_path,
                &format!("staged proof failed: {} ({error})", error.code()),
            );
            log(&log_path, "apply skipped: the staged proof is not valid");
            return fail_apply(&arguments, &log_path);
        }
    }
    let observer = WindowObserver(update_window.as_ref());
    match apply_staged_observed(
        &arguments.target,
        &app,
        &arguments.exe,
        &arguments.handshake,
        &observer,
    ) {
        Ok(outcome) => {
            log(
                &log_path,
                &format!("apply succeeded backup_removed={}", outcome.backup_removed),
            );
            // The handshake is confirmed: the installation surface is done.
            drop(update_window);
            let relocated = is_relocated_helper(&current, &arguments.staged);
            let cleanup = if relocated {
                cleanup_after_install_payload(&arguments.staged)
            } else {
                cleanup_after_install(&arguments.staged).or_else(|error| {
                    log(
                        &log_path,
                        &format!("staging cleanup deferred: {}", error.code()),
                    );
                    cleanup_after_install_payload(&arguments.staged)
                })
            };
            if let Err(error) = cleanup {
                log(
                    &log_path,
                    &format!("staging cleanup failed: {}", error.code()),
                );
            }
            cleanup_other_versions(&arguments.staged, &log_path);
            ExitCode::from(EXIT_OK)
        }
        Err(error) => {
            log(
                &log_path,
                &format!("apply failed: {} ({error})", error.code()),
            );
            drop(update_window);
            fail_apply(&arguments, &log_path)
        }
    }
}

/// Recovery, refusal and one restart after the new version was not proven.
///
/// Shared by an `apply_staged` error and a failed proof check: in both cases
/// the new version is not installed and the restored client must start once.
fn fail_apply(arguments: &Arguments, log_path: &Path) -> ExitCode {
    let recovery = recover_install(&arguments.target, RecoveryMode::Updater);
    let action = failure_action(&recovery);
    log(log_path, &format!("recovery after failure: {recovery:?}"));
    if action == FailureAction::RefuseAndRestart {
        if let Some(path) = arguments.refusal.as_deref() {
            let version = arguments
                .version
                .clone()
                .or_else(|| read_staged_version(&arguments.staged));
            if let Some(version) = version {
                // The refusal body carries only the version; paths and
                // errors stay in `apply.log`.
                let body = format!("{{\"version\":\"{}\"}}", version);
                if let Err(error) = write_marker(path, body.as_bytes()) {
                    log(log_path, &format!("cannot record install refusal: {error}"));
                }
            }
        }
        // One normal start of the restored version. The refusal marker above
        // stops it from auto-installing the same version again; a manual retry
        // stays available. A completed update is never started a second time.
        let client = arguments.target.join(&arguments.exe);
        if client.is_file() {
            match Command::new(&client).current_dir(&arguments.target).spawn() {
                Ok(_) => log(log_path, "started the restored client"),
                Err(error) => log(
                    log_path,
                    &format!("cannot start the restored client: {error}"),
                ),
            }
        }
    }
    ExitCode::from(EXIT_APPLY)
}

/// What the helper does after a failed apply, decided by recovery.
///
/// `Completed` means the swap is proven and the new version already confirmed
/// startup: writing a refusal or starting the old client would fight the
/// running installation. Every other answer — including a recovery error —
/// leaves this version unproven, so it is refused and the restored client is
/// started once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureAction {
    KeepInstalled,
    RefuseAndRestart,
}

fn failure_action(
    recovery: &Result<RecoveryOutcome, voxhold_update::UpdateError>,
) -> FailureAction {
    match recovery {
        Ok(RecoveryOutcome::Completed { .. }) => FailureAction::KeepInstalled,
        _ => FailureAction::RefuseAndRestart,
    }
}

/// Forwards apply stages to the helper window. Absent window means the helper
/// continues headless (only possible without `--ready`, see `main`).
struct WindowObserver<'a>(Option<&'a window::WindowHandle>);

impl ApplyObserver for WindowObserver<'_> {
    fn stage(&self, stage: ApplyStage) {
        if let Some(update_window) = self.0 {
            update_window.set_stage(stage.into());
            if stage == ApplyStage::Done {
                // The handshake is confirmed; the installation surface has
                // nothing left to show while the backup is cleaned up.
                update_window.request_close();
            }
        }
    }
}

fn read_staged_version(staged: &Path) -> Option<String> {
    let bytes = std::fs::read(staged.join("ready.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

fn write_marker(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

/// Remove staging directories of older attempts before the helper exits.
///
/// Runs only after a successful apply. Windows cannot delete the running
/// helper image, so the version directory of this run is kept (with
/// `apply.log`); the application removes it on the next start or after its
/// post-handshake cleanup timer. Every other entry is best-effort: an entry
/// that is still locked is skipped and retried by the next cleanup.
fn cleanup_other_versions(staged: &Path, log_path: &Path) {
    // `staged` is `<base>\<installation-id>\<version>`; the version directory's
    // parent belongs to this installation only. Falling back to the shared
    // base would let one portable copy delete another copy's staging.
    let root = match staged.parent() {
        Some(root) => root.to_path_buf(),
        None => match updates_root() {
            Ok(root) => root,
            Err(error) => {
                log(
                    log_path,
                    &format!("stale cleanup skipped: {}", error.code()),
                );
                return;
            }
        },
    };
    let keep = staged
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    match cleanup_stale(&root, keep.as_deref()) {
        Ok(0) => {}
        Ok(removed) => log(
            log_path,
            &format!("removed {removed} stale staging entries"),
        ),
        Err(error) => log(log_path, &format!("stale cleanup failed: {}", error.code())),
    }
}

fn collect_arguments() -> Result<Vec<String>, String> {
    std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "argument is not valid UTF-8".to_string())
        })
        .collect()
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut pid = None;
    let mut target = None;
    let mut staged = None;
    let mut exe = None;
    let mut handshake = None;
    let mut detached = false;
    let mut ready = None;
    let mut refusal = None;
    let mut version = None;
    let mut monitor = None;
    while let Some(flag) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))
        };
        match flag.as_str() {
            "--pid" => {
                let raw = value()?;
                pid = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("invalid pid {raw}"))?,
                );
            }
            "--target" => target = Some(absolute_path("--target", &value()?)?),
            "--staged" => staged = Some(absolute_path("--staged", &value()?)?),
            "--exe" => {
                let name = value()?;
                validate_exe_name(&name)?;
                exe = Some(name);
            }
            "--handshake" => handshake = Some(absolute_path("--handshake", &value()?)?),
            "--ready" => ready = Some(absolute_path("--ready", &value()?)?),
            "--refusal" => refusal = Some(absolute_path("--refusal", &value()?)?),
            "--version" => version = Some(value()?),
            "--monitor" => {
                let raw = value()?;
                monitor = Some(
                    window::MonitorRect::parse(&raw)
                        .ok_or_else(|| format!("invalid monitor rectangle {raw}"))?,
                );
            }
            "--detached" => detached = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let target = target.ok_or("missing --target")?;
    let staged = staged.ok_or("missing --staged")?;
    let target = std::fs::canonicalize(&target)
        .map_err(|error| format!("--target {}: {error}", target.display()))?;
    let staged = std::fs::canonicalize(&staged)
        .map_err(|error| format!("--staged {}: {error}", staged.display()))?;
    if !target.is_dir() {
        return Err(format!("--target {} is not a directory", target.display()));
    }
    if !staged.is_dir() {
        return Err(format!("--staged {} is not a directory", staged.display()));
    }
    Ok(Arguments {
        pid: pid.ok_or("missing --pid")?,
        target,
        staged,
        exe: exe.ok_or("missing --exe")?,
        handshake: handshake.ok_or("missing --handshake")?,
        detached,
        ready,
        refusal,
        version,
        monitor,
    })
}

fn absolute_path(flag: &str, raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!("{flag} must be an absolute path, got {raw}"));
    }
    Ok(path)
}

fn validate_exe_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        return Err(format!("--exe must be a plain file name, got {name}"));
    }
    Ok(())
}

/// Suppress the hard-error dialogs a child with a broken image would raise.
///
/// The mode is inherited by processes started from this one, so a damaged
/// client or helper image fails with an exit code instead of blocking the
/// update behind a modal dialog.
#[cfg(windows)]
fn suppress_child_hard_error_dialogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX,
    };
    // SAFETY: the call takes no pointers and only changes this process mode.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

#[cfg(not(windows))]
fn suppress_child_hard_error_dialogs() {}

fn spawn_detached(exe: &Path, raw: &[String]) -> Result<(), String> {
    let mut command = Command::new(exe);
    command.args(raw);
    command.arg("--detached");
    command.current_dir(std::env::temp_dir());
    suppress_child_hard_error_dialogs();
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot start {}: {error}", exe.display()))
}

fn log_usage_error(message: &str) {
    eprintln!("voxhold-updater: {message}");
    if let Ok(root) = updates_root() {
        log(&root.join("helper-usage.log"), message);
    }
}

fn log(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{seconds} {message}");
    }
}

#[cfg(windows)]
fn wait_for_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    if pid == 0 {
        return Ok(());
    }
    // SAFETY: the handle is closed on every path below.
    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            // ERROR_INVALID_PARAMETER means the process no longer exists.
            // Any other failure must not silently race a running client.
            let code = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or_default();
            if code == 87 {
                return Ok(());
            }
            return Err(format!("OpenProcess failed with code {code}"));
        }
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            match WaitForSingleObject(handle, 5_000) {
                WAIT_OBJECT_0 => {
                    CloseHandle(handle);
                    return Ok(());
                }
                WAIT_TIMEOUT => {
                    if Instant::now() >= deadline {
                        CloseHandle(handle);
                        return Err("client did not exit within the limit".into());
                    }
                }
                other => {
                    CloseHandle(handle);
                    return Err(format!("WaitForSingleObject returned {other}"));
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn wait_for_process(pid: u32) -> Result<(), String> {
    let deadline = Instant::now() + WAIT_LIMIT;
    let path = PathBuf::from(format!("/proc/{pid}"));
    while path.exists() {
        if Instant::now() >= deadline {
            return Err("client did not exit within the limit".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        temp: tempfile::TempDir,
        target: PathBuf,
        staged: PathBuf,
        handshake: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("VoxHold");
            let staged = temp.path().join("updates").join("0.2.0");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::create_dir_all(&staged).unwrap();
            let handshake = temp.path().join("ready.handshake");
            Self {
                temp,
                target,
                staged,
                handshake,
            }
        }

        fn args(&self) -> Vec<String> {
            vec![
                "--pid".into(),
                "0".into(),
                "--target".into(),
                self.target.to_string_lossy().into_owned(),
                "--staged".into(),
                self.staged.to_string_lossy().into_owned(),
                "--exe".into(),
                "Voxhold.exe".into(),
                "--handshake".into(),
                self.handshake.to_string_lossy().into_owned(),
            ]
        }
    }

    #[test]
    fn rejects_relative_paths() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        args[3] = "VoxHold".into();
        let error = parse_arguments(args.into_iter()).unwrap_err();
        assert!(error.contains("--target"), "{error}");

        let mut args = fixture.args();
        args[5] = "updates/0.2.0".into();
        let error = parse_arguments(args.into_iter()).unwrap_err();
        assert!(error.contains("--staged"), "{error}");

        let mut args = fixture.args();
        args[9] = "ready.handshake".into();
        let error = parse_arguments(args.into_iter()).unwrap_err();
        assert!(error.contains("--handshake"), "{error}");
    }

    #[test]
    fn canonicalises_paths_before_changing_directory() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.temp.path().join("nested")).unwrap();
        let mut args = fixture.args();
        args[3] = fixture
            .temp
            .path()
            .join("nested")
            .join("..")
            .join("VoxHold")
            .to_string_lossy()
            .into_owned();
        let parsed = parse_arguments(args.into_iter()).unwrap();
        assert_eq!(
            parsed.target,
            std::fs::canonicalize(&fixture.target).unwrap()
        );
        assert_eq!(
            parsed.staged,
            std::fs::canonicalize(&fixture.staged).unwrap()
        );
    }

    #[test]
    fn accepts_the_handoff_markers() {
        let fixture = Fixture::new();
        let ready = fixture.temp.path().join("helper-ready.ok");
        let refusal = fixture.temp.path().join("prefs.refusal.json");
        let mut args = fixture.args();
        args.extend([
            "--ready".into(),
            ready.to_string_lossy().into_owned(),
            "--refusal".into(),
            refusal.to_string_lossy().into_owned(),
            "--version".into(),
            "0.2.0".into(),
        ]);
        let parsed = parse_arguments(args.into_iter()).unwrap();
        assert_eq!(parsed.ready.as_deref(), Some(ready.as_path()));
        assert_eq!(parsed.refusal.as_deref(), Some(refusal.as_path()));
        assert_eq!(parsed.version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn accepts_the_monitor_rectangle() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        args.extend(["--monitor".into(), "-1920,0,0,1040".into()]);
        let parsed = parse_arguments(args.into_iter()).unwrap();
        assert_eq!(
            parsed.monitor,
            Some(window::MonitorRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1040,
            })
        );

        let mut args = fixture.args();
        args.extend(["--monitor".into(), "0,0,0,100".into()]);
        let error = parse_arguments(args.into_iter()).unwrap_err();
        assert!(error.contains("monitor"), "{error}");
    }

    #[test]
    fn accepts_the_detached_flag() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        args.push("--detached".into());
        let parsed = parse_arguments(args.into_iter()).unwrap();
        assert!(parsed.detached);
    }

    #[test]
    fn rejects_unsafe_exe_names() {
        for name in [
            "",
            ".",
            "..",
            "bin/Voxhold.exe",
            "bin\\Voxhold.exe",
            "C:Voxhold.exe",
        ] {
            assert!(validate_exe_name(name).is_err(), "{name}");
        }
        assert!(validate_exe_name("Voxhold.exe").is_ok());
    }

    #[test]
    fn rejects_missing_directories() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        args[5] = fixture
            .temp
            .path()
            .join("absent")
            .to_string_lossy()
            .into_owned();
        let error = parse_arguments(args.into_iter()).unwrap_err();
        assert!(error.contains("--staged"), "{error}");
    }

    #[test]
    fn a_proven_completion_never_refuses_or_restarts_the_client() {
        assert_eq!(
            failure_action(&Ok(RecoveryOutcome::Completed {
                operation_id: "op".into(),
                backup_removed: false,
            })),
            FailureAction::KeepInstalled,
            "the new version is already running"
        );
    }

    #[test]
    fn every_unproven_outcome_refuses_and_restarts_once() {
        let outcomes = [
            Ok(RecoveryOutcome::Clean),
            Ok(RecoveryOutcome::RolledBack {
                operation_id: "op".into(),
            }),
            Ok(RecoveryOutcome::NeedsUpdater {
                operation_id: None,
                stage: None,
            }),
            Ok(RecoveryOutcome::Ambiguous { backup: None }),
            Ok(RecoveryOutcome::Corrupt { backup: None }),
            Err(voxhold_update::UpdateError::Apply("recovery failed".into())),
        ];
        for outcome in outcomes {
            assert_eq!(
                failure_action(&outcome),
                FailureAction::RefuseAndRestart,
                "{outcome:?}"
            );
        }
    }
}
