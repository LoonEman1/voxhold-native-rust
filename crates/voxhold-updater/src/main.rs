//! Helper process that applies a staged update after the client exits.
//!
//! Arguments:
//!
//! ```text
//! --pid <process id> --target <install dir> --staged <staging dir>
//! --exe <executable name> --handshake <file>
//! ```
//!
//! `--staged` is the version directory (`...\updates\<version>`); the helper
//! uses `<staged>\app` as the prepared payload and writes `<staged>\apply.log`.
//! The log contains local paths and error codes only, never URLs or secrets.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use voxhold_update::apply::apply_staged;
use voxhold_update::stage::{cleanup_after_install, APPLY_LOG, APP_DIR};

const EXIT_OK: u8 = 0;
const EXIT_USAGE: u8 = 2;
const EXIT_WAIT: u8 = 3;
const EXIT_APPLY: u8 = 4;
const WAIT_LIMIT: Duration = Duration::from_secs(300);

struct Arguments {
    pid: u32,
    target: PathBuf,
    staged: PathBuf,
    exe: String,
    handshake: PathBuf,
}

fn main() -> ExitCode {
    let arguments = match parse_arguments(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("voxhold-updater: {message}");
            eprintln!(
                "usage: voxhold-updater --pid <pid> --target <dir> --staged <dir> --exe <name> --handshake <file>"
            );
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let log_path = arguments.staged.join(APPLY_LOG);
    log(&log_path, "helper started");
    log(
        &log_path,
        &format!(
            "pid={} target={} staged={} exe={}",
            arguments.pid,
            arguments.target.display(),
            arguments.staged.display(),
            arguments.exe
        ),
    );
    // Do not hold the installation directory or the staging directory as the
    // current directory: the swap renames the installation and the success
    // path removes the staging directory. Use its parent instead.
    let workdir = arguments
        .staged
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    if let Err(error) = std::env::set_current_dir(&workdir) {
        log(
            &log_path,
            &format!("cannot change directory: {}", error.kind()),
        );
    }

    if let Err(message) = wait_for_process(arguments.pid) {
        log(&log_path, &format!("wait failed: {message}"));
        return ExitCode::from(EXIT_WAIT);
    }
    log(&log_path, "client exited");

    let app = arguments.staged.join(APP_DIR);
    let result = apply_staged(
        &arguments.target,
        &app,
        &arguments.exe,
        &arguments.handshake,
    );
    match result {
        Ok(()) => {
            log(&log_path, "apply succeeded");
            if let Err(error) = cleanup_after_install(&arguments.staged) {
                log(&log_path, &format!("cleanup failed: {}", error.code()));
            }
            ExitCode::from(EXIT_OK)
        }
        Err(error) => {
            log(
                &log_path,
                &format!("apply failed: {} ({error})", error.code()),
            );
            ExitCode::from(EXIT_APPLY)
        }
    }
}

fn parse_arguments(mut arguments: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut pid = None;
    let mut target = None;
    let mut staged = None;
    let mut exe = None;
    let mut handshake = None;
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
            "--target" => target = Some(PathBuf::from(value()?)),
            "--staged" => staged = Some(PathBuf::from(value()?)),
            "--exe" => exe = Some(value()?),
            "--handshake" => handshake = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Arguments {
        pid: pid.ok_or("missing --pid")?,
        target: target.ok_or("missing --target")?,
        staged: staged.ok_or("missing --staged")?,
        exe: exe.ok_or("missing --exe")?,
        handshake: handshake.ok_or("missing --handshake")?,
    })
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
