//! SOLO: installs a downloaded Solo update once Solo has exited.
//!
//! ```text
//! solo-update-helper <Solo's process id> <installer> <install folder> <installer log> [--no-launch]
//! ```
//!
//! Solo starts it from a copy next to the downloaded installer, since the
//! installer replaces the one next to solo.exe. It waits for Solo to exit, runs
//! the installer silently into the install folder (which starts Solo again,
//! unless `--no-launch`), and exits. Problems go to
//! `update-helper.log` next to the installer log.
//!
//! It replaces a hidden PowerShell script, which security software tends to
//! stop.

// A background program: no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(
    clippy::disallowed_methods,
    reason = "a standalone program with nothing to block but itself"
)]

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (pid, installer, install_dir, log_file, relaunch) = match args.as_slice() {
        [pid, installer, install_dir, log_file] => (pid, installer, install_dir, log_file, true),
        [pid, installer, install_dir, log_file, flag] if flag == "--no-launch" => {
            (pid, installer, install_dir, log_file, false)
        }
        _ => {
            eprintln!(
                "usage: solo-update-helper <Solo's process id> <installer> <install folder> <installer log> [--no-launch]"
            );
            std::process::exit(2);
        }
    };
    let installer = Path::new(installer);
    let install_dir = Path::new(install_dir);
    let log_file = Path::new(log_file);
    let helper_log = log_file.with_file_name("update-helper.log");

    if let Some(pid) = pid.to_str().and_then(|pid| pid.parse::<u32>().ok()) {
        wait_for_exit(pid);
    } else {
        log(&helper_log, &format!("not a process id: {pid:?}"));
    }
    if let Err(error) = run_installer(installer, install_dir, log_file, relaunch) {
        log(
            &helper_log,
            &format!("couldn't run {}: {error}", installer.display()),
        );
        std::process::exit(1);
    }
}

fn log(path: &Path, message: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        writeln!(file, "{message}").ok();
    }
}

/// The installer's command line after its path. Inno Setup takes
/// `/DIR="..."` and `/LOG="..."` with the quotes inside the argument, and
/// Windows paths can't contain `"`. `/NOLAUNCH` is Solo's own (solo.iss):
/// don't start Solo after installing.
fn installer_args(install_dir: &Path, log_file: &Path, relaunch: bool) -> Vec<String> {
    let install_dir = install_dir.to_string_lossy();
    // A trailing backslash would read as escaping the closing quote.
    let install_dir = install_dir.trim_end_matches(['\\', '/']);
    let mut args = vec![
        "/SILENT".to_string(),
        "/SUPPRESSMSGBOXES".to_string(),
        "/NORESTART".to_string(),
        format!("/DIR=\"{install_dir}\""),
        format!("/LOG=\"{}\"", log_file.display()),
    ];
    if !relaunch {
        args.push("/NOLAUNCH".to_string());
    }
    args
}

#[cfg(windows)]
fn run_installer(
    installer: &Path,
    install_dir: &Path,
    log_file: &Path,
    relaunch: bool,
) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt as _;

    let mut command = std::process::Command::new(installer);
    for arg in installer_args(install_dir, log_file, relaunch) {
        command.raw_arg(arg);
    }
    // The installer outlives the helper; it shows its own progress.
    command.spawn().map(|_child| ())
}

#[cfg(not(windows))]
fn run_installer(
    _installer: &Path,
    install_dir: &Path,
    log_file: &Path,
    relaunch: bool,
) -> std::io::Result<()> {
    let _ = installer_args(install_dir, log_file, relaunch);
    Err(std::io::Error::other(
        "updates are only installed on Windows",
    ))
}

#[cfg(windows)]
fn wait_for_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: the handle is only used here and closed after waiting. A
    // process that's already gone can't be opened, which is as good as
    // having waited for it.
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, INFINITE);
            CloseHandle(process).ok();
        }
    }
}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_args_quote_paths_with_spaces_and_chinese() {
        assert_eq!(
            installer_args(
                Path::new(r"D:\Program Files\终端 Solo's\"),
                Path::new(r"C:\Users\张三\AppData\Local\Temp\solo-update\install.log"),
                true,
            ),
            vec![
                "/SILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                r#"/DIR="D:\Program Files\终端 Solo's""#,
                r#"/LOG="C:\Users\张三\AppData\Local\Temp\solo-update\install.log""#,
            ]
        );
    }
}
