//! SOLO: `solo-cli` and `solo-agent-hook` by a path that outlives updates.
//!
//! Agents keep the hook commands they're given for as long as they run, and
//! Claude Code's settings file and Codex's trust are keyed by the hook's
//! path; `SOLO_CLI` stays in a terminal's environment. A packaged Solo (the
//! Microsoft Store edition) runs from a folder that changes with every
//! update, whose old version is removed once nothing runs from it, so it
//! copies the two programs to `bin` in the package's shared folder
//! ([`util::packaged::package_shared_dir`], which programs outside the
//! package see too) and hands out that path. (Execution aliases would need an `Application` without a Start menu
//! entry for each, which the Store doesn't accept.) Installed by the
//! installer or from the zip, Solo's folder stays the same and the programs
//! next to it are used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::{Context as _, Result};

/// The folder a packaged Solo copies the programs to.
pub fn stable_programs_dir() -> PathBuf {
    util::packaged::package_shared_dir()
        .unwrap_or_else(|| paths::data_dir().clone())
        .join("bin")
}

/// The path to give out for `stem` (`solo-cli`, `solo-agent-hook`), or
/// `None` if Solo doesn't ship it.
pub fn program_path(stem: &str) -> Option<PathBuf> {
    static PATHS: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> =
        LazyLock::new(Default::default);
    let mut paths = PATHS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    paths
        .entry(stem.to_string())
        .or_insert_with(|| {
            let bundled = util::packaged::bundled_executable(stem)?;
            if !util::packaged::is_packaged() {
                return Some(bundled);
            }
            match install_stable_copy(&bundled, &stable_programs_dir()) {
                Ok(path) => Some(path),
                Err(error) => {
                    log::warn!(
                        "using {} from Solo's versioned folder: {error:#}",
                        bundled.display()
                    );
                    Some(bundled)
                }
            }
        })
        .clone()
}

/// Copies `source` into `dir` under its own name unless an identical copy is
/// there, and returns where the copy really is.
///
/// A copy in use (an agent's hook running, `solo-cli` waiting) can't be
/// overwritten, but can be renamed: it's moved aside, the new one put in its
/// place, and the old one deleted later. If even that fails, the old copy
/// stays (it's replaced on a later start) and its path is returned: a stable
/// path matters more to agents than the newest hook.
fn install_stable_copy(source: &Path, dir: &Path) -> Result<PathBuf> {
    let name = source.file_name().context("no file name")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = dir.join(name);
    remove_leftovers(dir, &name.to_string_lossy());

    let wanted = std::fs::read(source).with_context(|| format!("reading {}", source.display()))?;
    if std::fs::read(&target).is_ok_and(|current| current == wanted) {
        return real_path(&target);
    }

    let nonce = crate::shell_integration::new_nonce();
    let staged = dir.join(format!("{}.new-{nonce}", name.to_string_lossy()));
    std::fs::write(&staged, &wanted).with_context(|| format!("writing {}", staged.display()))?;
    if std::fs::rename(&staged, &target).is_err() {
        let aside = dir.join(format!("{}.old-{nonce}", name.to_string_lossy()));
        let replaced =
            std::fs::rename(&target, &aside).is_ok() && std::fs::rename(&staged, &target).is_ok();
        if !replaced {
            std::fs::remove_file(&staged).ok();
            if target.is_file() {
                log::warn!(
                    "{} is in use; it's updated on a later start",
                    target.display()
                );
                return real_path(&target);
            }
            anyhow::bail!("couldn't put {} in place", target.display());
        }
    }
    real_path(&target)
}

/// Deletes copies moved aside, or left half-written, by earlier starts; one
/// still running stays until a later start.
fn remove_leftovers(dir: &Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let old = format!("{name}.old-");
    let new = format!("{name}.new-");
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with(&old) || file_name.starts_with(&new) {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// Where `path` really is on disk, without links in it: what agents keep
/// as the hook's path.
fn real_path(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).with_context(|| format!("resolving {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(dir: &Path, contents: &[u8]) -> PathBuf {
        let path = dir.join("solo-agent-hook.exe");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn copies_once_and_again_when_the_program_changes() {
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let bin = data.path().join("bin");
        let source = exe(package.path(), b"version 1");

        let copy = install_stable_copy(&source, &bin).unwrap();
        assert_eq!(
            copy,
            dunce::canonicalize(bin.join("solo-agent-hook.exe")).unwrap()
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"version 1");

        // Same contents: left alone (a marker written into it stays).
        std::fs::write(&copy, b"version 1").unwrap();
        let modified = std::fs::metadata(&copy).unwrap().modified().unwrap();
        assert_eq!(install_stable_copy(&source, &bin).unwrap(), copy);
        assert_eq!(
            std::fs::metadata(&copy).unwrap().modified().unwrap(),
            modified
        );

        // An update ships another program: same path, new contents.
        let source = exe(package.path(), b"version 2");
        assert_eq!(install_stable_copy(&source, &bin).unwrap(), copy);
        assert_eq!(std::fs::read(&copy).unwrap(), b"version 2");
        assert_eq!(files(&bin), ["solo-agent-hook.exe"]);
    }

    #[test]
    fn removes_leftovers_of_earlier_starts() {
        let package = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        for name in [
            "solo-agent-hook.exe.old-1234",
            "solo-agent-hook.exe.new-5678",
            "solo-cli.exe",
        ] {
            std::fs::write(bin.path().join(name), b"").unwrap();
        }
        let source = exe(package.path(), b"hook");
        install_stable_copy(&source, bin.path()).unwrap();
        assert_eq!(files(bin.path()), ["solo-agent-hook.exe", "solo-cli.exe"]);
    }

    /// A copy that can be neither replaced nor moved aside (held open
    /// without sharing delete access) stays, and its path is still given out.
    #[cfg(windows)]
    #[test]
    fn keeps_a_copy_that_cant_be_replaced() {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 1;

        let package = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let copy = install_stable_copy(&exe(package.path(), b"old"), bin.path()).unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&copy)
            .unwrap();

        let source = exe(package.path(), b"new");
        assert_eq!(install_stable_copy(&source, bin.path()).unwrap(), copy);
        drop(held);
        assert_eq!(std::fs::read(&copy).unwrap(), b"old");
        assert_eq!(files(bin.path()), ["solo-agent-hook.exe"]);

        // The next start replaces it.
        install_stable_copy(&source, bin.path()).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"new");
    }
}
