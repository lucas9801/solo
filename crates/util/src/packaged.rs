//! SOLO: Solo installed as an MSIX package (the Microsoft Store edition).
//!
//! A packaged Solo runs from `C:\Program Files\WindowsApps\<Name>_<Version>_...`,
//! a folder that changes with every update (and whose old version is removed
//! once nothing runs from it). Programs handed to processes that outlive
//! Solo, such as `SOLO_CLI` in a terminal or the hook commands agents are
//! given, are copied to Solo's data folder first (`terminal::stable_programs`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The package family name (`<Name>_<PublisherId>`) of the package Solo runs
/// from, or `None` when it isn't packaged (the installer, the zip, a build).
pub fn package_family_name() -> Option<&'static str> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(current_package_family_name).as_deref()
}

/// Whether Solo runs as a packaged app, e.g. installed from the Microsoft
/// Store, which updates it.
pub fn is_packaged() -> bool {
    package_family_name().is_some()
}

#[cfg(windows)]
fn current_package_family_name() -> Option<String> {
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    use windows::core::PWSTR;

    let mut length = 0u32;
    // SAFETY: with no buffer it only writes the length it needs.
    let result = unsafe { GetCurrentPackageFamilyName(&mut length, None) };
    // APPMODEL_ERROR_NO_PACKAGE when the process has no package identity.
    if result != ERROR_INSUFFICIENT_BUFFER || length == 0 {
        return None;
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `buffer` holds `length` characters, as the call asked for.
    let result =
        unsafe { GetCurrentPackageFamilyName(&mut length, Some(PWSTR(buffer.as_mut_ptr()))) };
    if result != ERROR_SUCCESS {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let name = String::from_utf16(&buffer[..end]).ok()?;
    (!name.is_empty()).then_some(name)
}

#[cfg(not(windows))]
fn current_package_family_name() -> Option<String> {
    None
}

/// A program Solo ships next to its own executable (`solo-cli`,
/// `solo-agent-hook`), or `None` if it isn't there. When Solo is packaged,
/// this folder changes with each update.
pub fn bundled_executable(stem: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    select_bundled_executable(stem, exe.parent()?)
}

/// When Solo is packaged, a folder of its own that programs outside the
/// package see at the same path: in the package's `LocalCache` folder
/// (`%LOCALAPPDATA%\Packages\<family name>\LocalCache\Solo`), which Windows
/// removes with the package. `None` when Solo isn't packaged.
///
/// Files Solo hands to its terminals' shells and agents go here. Without the
/// `unvirtualizedResources` capability (which the Store doesn't grant Solo),
/// a package's new files in `%LOCALAPPDATA%` and `%APPDATA%` (Solo's data
/// folder among them) are redirected to a place only the package sees at
/// the original path, while the shells in Solo's terminals run outside it.
pub fn package_shared_dir() -> Option<PathBuf> {
    let family_name = package_family_name()?;
    let local = dirs::data_local_dir()?;
    Some(
        local
            .join("Packages")
            .join(family_name)
            .join("LocalCache")
            .join("Solo"),
    )
}

fn select_bundled_executable(stem: &str, exe_dir: &Path) -> Option<PathBuf> {
    let bundled = exe_dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
    bundled.is_file().then_some(bundled)
}

/// Flags for `RegisterApplicationRestart`: restart only after an update
/// (`RESTART_NO_CRASH | RESTART_NO_HANG | RESTART_NO_REBOOT`), not after a
/// crash, a hang or a reboot.
#[cfg(windows)]
const RESTART_AFTER_UPDATE_ONLY: u32 = 1 | 2 | 8;

/// The longest command line `RegisterApplicationRestart` takes
/// (`RESTART_MAX_CMD_LINE`), in characters with the terminating null.
const RESTART_MAX_COMMAND_LINE: usize = 1024;

/// When Solo runs as a packaged app, asks Windows to start it again, with
/// `args`, if an update has to close it (a Store update that doesn't wait for
/// Solo to quit). Does nothing otherwise: the installer and Solo's own
/// updater start Solo again themselves.
pub fn register_restart_after_update(args: &[impl AsRef<OsStr>]) {
    if !is_packaged() {
        return;
    }
    let Some(command_line) = restart_command_line(args) else {
        log::warn!("not registering for a restart after updates: the command line is too long");
        return;
    };
    register_restart(&command_line);
}

#[cfg(windows)]
fn register_restart(command_line: &str) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn RegisterApplicationRestart(command_line: *const u16, flags: u32) -> i32;
    }
    let wide: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is null-terminated and outlives the call, which copies it.
    let result = unsafe { RegisterApplicationRestart(wide.as_ptr(), RESTART_AFTER_UPDATE_ONLY) };
    if result < 0 {
        log::warn!("RegisterApplicationRestart failed: {result:#x}");
    }
}

#[cfg(not(windows))]
fn register_restart(_command_line: &str) {}

/// The arguments for `RegisterApplicationRestart` (without the program, which
/// Windows puts first), quoted the way `CommandLineToArgvW` splits them;
/// `None` if they're too long.
fn restart_command_line(args: &[impl AsRef<OsStr>]) -> Option<String> {
    let command_line = args
        .iter()
        .map(|arg| quote_windows_arg(&arg.as_ref().to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    (command_line.encode_utf16().count() < RESTART_MAX_COMMAND_LINE).then_some(command_line)
}

/// `arg` quoted for `CommandLineToArgvW` (and the Rust runtime's parsing).
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn finds_the_program_next_to_solo() {
        let install = tempfile::tempdir().unwrap();
        let hook = install
            .path()
            .join(format!("solo-agent-hook{}", std::env::consts::EXE_SUFFIX));
        touch(&hook);
        assert_eq!(
            select_bundled_executable("solo-agent-hook", install.path()),
            Some(hook)
        );
        assert_eq!(select_bundled_executable("solo-cli", install.path()), None);
    }

    #[test]
    fn restart_command_line_quotes_arguments() {
        assert_eq!(restart_command_line(&[] as &[&str]).as_deref(), Some(""));
        assert_eq!(
            restart_command_line(&["--user-data-dir", r"C:\Users\A B\solo data\"]).as_deref(),
            Some(r#"--user-data-dir "C:\Users\A B\solo data\\""#)
        );
        assert_eq!(
            restart_command_line(&[r#"say "hi""#, ""]).as_deref(),
            Some(r#""say \"hi\"" """#)
        );
        let long = "x".repeat(RESTART_MAX_COMMAND_LINE);
        assert_eq!(restart_command_line(&[long]), None);
    }
}
