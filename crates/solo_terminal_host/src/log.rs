//! The host's own log: what Solo told it (the control bytes), its clients
//! coming and going, and why it ended, so a terminal that didn't survive
//! Solo quitting can be explained. One file a day in Solo's logs folder,
//! shared by all hosts; files older than [`KEEP_DAYS`] go.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const PREFIX: &str = "terminal-host-";
const KEEP_DAYS: u64 = 7;

pub struct Log {
    dir: Option<PathBuf>,
    /// Who's logging: the host's process id and the end of its pipe's name.
    who: String,
    lock: Mutex<()>,
}

impl Log {
    pub fn new(dir: Option<&str>, pipe: &str) -> Self {
        let dir = dir.map(PathBuf::from);
        if let Some(dir) = &dir {
            remove_old(dir);
        }
        let name = pipe.rsplit('\\').next().unwrap_or(pipe);
        Self {
            dir,
            who: format!("{} {name}", std::process::id()),
            lock: Mutex::new(()),
        }
    }

    pub fn write(&self, message: &str) {
        let Some(dir) = &self.dir else {
            return;
        };
        let now = local_time();
        let path = dir.join(file_name(&now));
        let line = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03} [{}] {message}\r\n",
            now.wYear,
            now.wMonth,
            now.wDay,
            now.wHour,
            now.wMinute,
            now.wSecond,
            now.wMilliseconds,
            self.who
        );
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Opened for each line, as other hosts write to it too.
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            file.write_all(line.as_bytes()).ok();
        }
    }
}

fn local_time() -> SYSTEMTIME {
    unsafe {
        let mut time = std::mem::zeroed();
        GetLocalTime(&mut time);
        time
    }
}

fn file_name(time: &SYSTEMTIME) -> String {
    format!(
        "{PREFIX}{:04}-{:02}-{:02}.log",
        time.wYear, time.wMonth, time.wDay
    )
}

/// Removes this log's files last written more than [`KEEP_DAYS`] ago.
fn remove_old(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let limit = std::time::Duration::from_secs(KEEP_DAYS * 24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(PREFIX) && name.ends_with(".log")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_writes_a_file_a_day() {
        let dir = std::env::temp_dir().join(format!(
            "solo-terminal-host-log-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = Log::new(dir.to_str(), r"\\.\pipe\solo-terminal-1-abc");
        log.write("got P");
        log.write("client gone, keep running: true");
        let path = dir.join(file_name(&local_time()));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with(&format!(
                "[{} solo-terminal-1-abc] got P",
                std::process::id()
            )),
            "{text}"
        );
        // Without a folder (an older Solo), nothing is written.
        Log::new(None, "x").write("nothing");
        std::fs::remove_dir_all(&dir).ok();
    }
}
