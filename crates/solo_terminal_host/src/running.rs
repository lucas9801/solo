//! The host's entry in Solo's list of running hosts: `<pid>.json` in the
//! folder Solo names (`terminal-host/running` in its data folder), written
//! when the program starts, rewritten as clients come and go, and deleted
//! when the host ends. A Solo starting up reads the list to find hosts no
//! terminal of its own connects to (see [`crate::Running`]).

use std::path::PathBuf;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use crate::Running;

static ENTRY: Mutex<Option<(PathBuf, Running)>> = Mutex::new(None);

/// When this process was created, as Windows counts (100 ns since 1601):
/// with the pid, it tells this host from a later process given its pid.
pub fn own_creation_time() -> u64 {
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [created, exited, kernel, user] = &mut times;
    unsafe {
        GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user);
    }
    (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime)
}

/// Lists the host in `dir`.
pub fn add(dir: Option<&str>, entry: Running) {
    let Some(dir) = dir else {
        return;
    };
    let dir = PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.json", entry.host_pid));
    let mut kept = ENTRY.lock().unwrap_or_else(|error| error.into_inner());
    write(&path, &entry);
    *kept = Some((path, entry));
}

/// Updates the listed entry.
pub fn update(change: impl FnOnce(&mut Running)) {
    let mut kept = ENTRY.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((path, entry)) = kept.as_mut() {
        change(entry);
        write(path, entry);
    }
}

/// The host ends: off the list.
pub fn remove() {
    let mut kept = ENTRY.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((path, _)) = kept.take() {
        std::fs::remove_file(path).ok();
    }
}

/// Written whole, then renamed over the old one, so a reader never sees
/// half of it.
fn write(path: &PathBuf, entry: &Running) {
    let Ok(json) = serde_json::to_vec_pretty(entry) else {
        return;
    };
    let partial = path.with_extension("json.partial");
    if std::fs::write(&partial, json).is_ok() && std::fs::rename(&partial, path).is_err() {
        std::fs::remove_file(&partial).ok();
    }
}
