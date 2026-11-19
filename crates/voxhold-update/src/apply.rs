//! Applying a staged update from the helper process.
//!
//! The installed directory is moved to `<name>.voxhold-backup`, the staged
//! package is copied in, user files outside the package are preserved and the
//! new executable is started. The new version must write a handshake file
//! within the timeout; otherwise the previous directory is restored.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::error::{Result, UpdateError};
use crate::package::{read_package_json_from_dir, sha256_file, PackageIndex, PACKAGE_JSON};

/// Default time the new version has to confirm startup.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Suffix of the backup directory next to the installation.
pub const BACKUP_SUFFIX: &str = ".voxhold-backup";
/// Argument passed to the new executable so it can confirm startup.
pub const HANDSHAKE_ARG: &str = "--update-handshake";

/// A process started after the swap.
pub trait LaunchedProcess {
    fn try_wait(&mut self) -> Result<Option<i32>>;
    fn kill(&mut self);
}

/// Starts the new executable.
pub trait UpdateLauncher {
    fn launch(&self, exe: &Path, handshake: &Path) -> Result<Box<dyn LaunchedProcess>>;
}

/// Launcher used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessLauncher;

impl UpdateLauncher for ProcessLauncher {
    fn launch(&self, exe: &Path, handshake: &Path) -> Result<Box<dyn LaunchedProcess>> {
        let mut command = Command::new(exe);
        command.arg(HANDSHAKE_ARG).arg(handshake);
        if let Some(parent) = exe.parent() {
            command.current_dir(parent);
        }
        let child = command
            .spawn()
            .map_err(|error| UpdateError::Apply(format!("cannot start new version: {error}")))?;
        Ok(Box::new(ChildProcess { child }))
    }
}

struct ChildProcess {
    child: Child,
}

impl LaunchedProcess for ChildProcess {
    fn try_wait(&mut self) -> Result<Option<i32>> {
        let status = self
            .child
            .try_wait()
            .map_err(|error| UpdateError::Apply(format!("cannot poll new version: {error}")))?;
        Ok(status.map(|status| status.code().unwrap_or(-1)))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Apply a staged update.
pub fn apply_staged(
    target_dir: &Path,
    staged_app: &Path,
    exe_name: &str,
    handshake_path: &Path,
) -> Result<()> {
    apply_staged_with(
        target_dir,
        staged_app,
        exe_name,
        handshake_path,
        &ProcessLauncher,
        HANDSHAKE_TIMEOUT,
    )
}

/// Apply with an explicit launcher and timeout (used by tests).
pub fn apply_staged_with(
    target_dir: &Path,
    staged_app: &Path,
    exe_name: &str,
    handshake_path: &Path,
    launcher: &dyn UpdateLauncher,
    timeout: Duration,
) -> Result<()> {
    let new_index = read_package_json_from_dir(staged_app)?;
    let exe = staged_app.join(exe_name);
    if !exe.is_file() {
        return Err(UpdateError::NotStaged(exe));
    }
    let old_index = read_package_json_from_dir(target_dir).ok();
    let backup = backup_path(target_dir);

    if backup.exists() && !target_dir.exists() {
        std::fs::rename(&backup, target_dir).map_err(|error| UpdateError::io(&backup, error))?;
    }
    if backup.exists() {
        remove_dir_retry(&backup)?;
    }

    let had_target = target_dir.exists();
    if had_target {
        rename_retry(target_dir, &backup)?;
    }
    std::fs::create_dir_all(target_dir).map_err(|error| UpdateError::io(target_dir, error))?;

    let result = populate(
        target_dir,
        staged_app,
        &new_index,
        old_index.as_ref(),
        had_target,
        &backup,
    )
    .and_then(|()| launch_and_wait(target_dir, exe_name, handshake_path, launcher, timeout));

    match result {
        Ok(()) => {
            // Best effort: the helper itself may still be running from the
            // backup directory, so a failure here is expected and the next
            // startup removes the leftover through `cleanup_stale`.
            let _ = std::fs::remove_dir_all(&backup);
            let _ = std::fs::remove_dir_all(staged_app);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(target_dir);
            if had_target && backup.exists() {
                let _ = std::fs::rename(&backup, target_dir);
            }
            let _ = std::fs::remove_dir_all(staged_app);
            Err(error)
        }
    }
}

fn populate(
    target_dir: &Path,
    staged_app: &Path,
    new_index: &PackageIndex,
    old_index: Option<&PackageIndex>,
    had_target: bool,
    backup: &Path,
) -> Result<()> {
    for file in &new_index.files {
        let source = staged_app.join(Path::new(&file.path));
        let destination = target_dir.join(Path::new(&file.path));
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
        }
        std::fs::copy(&source, &destination)
            .map_err(|error| UpdateError::io(&destination, error))?;
        if sha256_file(&destination)? != file.sha256 {
            return Err(UpdateError::HashMismatch { path: destination });
        }
    }
    let staged_json = staged_app.join(PACKAGE_JSON);
    let target_json = target_dir.join(PACKAGE_JSON);
    std::fs::copy(&staged_json, &target_json)
        .map_err(|error| UpdateError::io(&target_json, error))?;

    if had_target && backup.exists() {
        let old_files: BTreeSet<String> = old_index
            .map(|index| index.files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default();
        let new_files: BTreeSet<String> = new_index
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        copy_user_files(backup, target_dir, &old_files, &new_files)?;
    }
    Ok(())
}

fn copy_user_files(
    backup: &Path,
    target_dir: &Path,
    old_files: &BTreeSet<String>,
    new_files: &BTreeSet<String>,
) -> Result<()> {
    let mut stack = vec![backup.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in
            std::fs::read_dir(&current).map_err(|error| UpdateError::io(&current, error))?
        {
            let entry = entry.map_err(|error| UpdateError::io(&current, error))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| UpdateError::io(&path, error))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(backup)
                .unwrap_or(&path)
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if relative == PACKAGE_JSON
                || old_files.contains(&relative)
                || new_files.contains(&relative)
            {
                continue;
            }
            let destination = target_dir.join(Path::new(&relative));
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
            }
            std::fs::copy(&path, &destination)
                .map_err(|error| UpdateError::io(&destination, error))?;
        }
    }
    Ok(())
}

fn launch_and_wait(
    target_dir: &Path,
    exe_name: &str,
    handshake_path: &Path,
    launcher: &dyn UpdateLauncher,
    timeout: Duration,
) -> Result<()> {
    if let Some(parent) = handshake_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| UpdateError::io(parent, error))?;
    }
    let _ = std::fs::remove_file(handshake_path);
    let exe = target_dir.join(exe_name);
    let mut process = launcher.launch(&exe, handshake_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        if handshake_path.is_file() {
            return Ok(());
        }
        match process.try_wait() {
            Ok(Some(code)) => {
                if code != 0 {
                    return Err(UpdateError::Apply(format!(
                        "new version exited with code {code}"
                    )));
                }
            }
            Ok(None) => {}
            Err(error) => {
                process.kill();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            process.kill();
            return Err(UpdateError::LaunchTimeout(timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn backup_path(target_dir: &Path) -> PathBuf {
    let name = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "VoxHold".into());
    target_dir.with_file_name(format!("{name}{BACKUP_SUFFIX}"))
}

fn rename_retry(from: &Path, to: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = error;
            }
            Err(error) => return Err(UpdateError::io(to, error)),
        }
    }
}

fn remove_dir_retry(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = error;
            }
            Err(error) => return Err(UpdateError::io(path, error)),
        }
    }
}

/// Restore the backup if it still exists.
pub fn rollback(target_dir: &Path) -> Result<bool> {
    let backup = backup_path(target_dir);
    if !backup.exists() {
        return Ok(false);
    }
    if target_dir.exists() {
        remove_dir_retry(target_dir)?;
    }
    std::fs::rename(&backup, target_dir).map_err(|error| UpdateError::io(target_dir, error))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::APP_DIR;
    use std::fs;

    struct FakeLauncher {
        handshake: bool,
    }

    struct FakeProcess;

    impl LaunchedProcess for FakeProcess {
        fn try_wait(&mut self) -> Result<Option<i32>> {
            Ok(None)
        }
        fn kill(&mut self) {}
    }

    impl UpdateLauncher for FakeLauncher {
        fn launch(&self, _exe: &Path, handshake: &Path) -> Result<Box<dyn LaunchedProcess>> {
            if self.handshake {
                fs::write(handshake, b"ok").unwrap();
            }
            Ok(Box::new(FakeProcess))
        }
    }

    fn install_package(root: &Path, version: &str, exe: &[u8]) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("Voxhold.exe"), exe).unwrap();
        fs::write(root.join("README.md"), b"readme").unwrap();
        let packed = crate::package::pack_with_index(root, version).unwrap();
        fs::remove_dir_all(root).unwrap();
        crate::package::unpack(&packed.bytes, root).unwrap();
    }

    #[test]
    fn applies_and_preserves_user_files() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("VoxHold");
        install_package(&target, "1.0.0", b"old exe");
        fs::write(target.join("settings.json"), b"user settings").unwrap();

        let staged = temp.path().join("staged").join(APP_DIR);
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("Voxhold.exe"), b"new exe").unwrap();
        fs::write(staged.join("README.md"), b"readme").unwrap();
        let packed = crate::package::pack_with_index(&staged, "1.1.0").unwrap();
        fs::remove_dir_all(&staged).unwrap();
        crate::package::unpack(&packed.bytes, &staged).unwrap();

        let handshake = temp.path().join("ready.handshake");
        apply_staged_with(
            &target,
            &staged,
            "Voxhold.exe",
            &handshake,
            &FakeLauncher { handshake: true },
            Duration::from_millis(500),
        )
        .unwrap();

        assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"new exe");
        assert_eq!(
            fs::read(target.join("settings.json")).unwrap(),
            b"user settings"
        );
        assert!(!backup_path(&target).exists());
        assert!(!staged.exists());
    }

    #[test]
    fn rolls_back_when_launch_fails() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("VoxHold");
        install_package(&target, "1.0.0", b"old exe");
        fs::write(target.join("settings.json"), b"user settings").unwrap();

        let staged = temp.path().join("staged").join(APP_DIR);
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("Voxhold.exe"), b"new exe").unwrap();
        fs::write(staged.join("README.md"), b"readme").unwrap();
        let packed = crate::package::pack_with_index(&staged, "1.1.0").unwrap();
        fs::remove_dir_all(&staged).unwrap();
        crate::package::unpack(&packed.bytes, &staged).unwrap();

        let handshake = temp.path().join("ready.handshake");
        let error = apply_staged_with(
            &target,
            &staged,
            "Voxhold.exe",
            &handshake,
            &FakeLauncher { handshake: false },
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(matches!(error, UpdateError::LaunchTimeout(_)));
        assert_eq!(fs::read(target.join("Voxhold.exe")).unwrap(), b"old exe");
        assert_eq!(
            fs::read(target.join("settings.json")).unwrap(),
            b"user settings"
        );
        assert!(!backup_path(&target).exists());
    }

    #[test]
    fn explicit_rollback_restores_backup() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("VoxHold");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("file"), b"new").unwrap();
        let backup = backup_path(&target);
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("file"), b"old").unwrap();
        assert!(rollback(&target).unwrap());
        assert_eq!(fs::read(target.join("file")).unwrap(), b"old");
        assert!(!backup.exists());
    }
}
