//! SOLO: terminals whose console a `solo-terminal-host` process holds
//! (`crates/solo_terminal_host`), so the shell and what runs in it can outlive
//! Solo quitting or restarting for an update, and Solo can connect to them
//! again when it opens the terminal next time.
//!
//! Each terminal gets its own host, on its own pipes. A host ends with the
//! program it runs; when the terminal is closed; and when Solo goes (quits,
//! closes the window or crashes) while the terminal isn't running anything:
//! Solo keeps telling the host whether it is ([`HostLink::set_keep_running`]),
//! so the host knows even when Solo can't say goodbye.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::IntoRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::tty;
use anyhow::{Context as _, Result, anyhow, bail};
use parking_lot::Mutex;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use crate::alacritty::AlacrittyPty;

/// Must match `solo_terminal_host::PROTOCOL` and `HEADER`.
const PROTOCOL: u32 = 1;
const HEADER: &str = "SOLO-TERMINAL-HOST";
const HOST_EXE: &str = "solo-terminal-host.exe";
/// What runs next to the host, copied with it.
const HOST_FILES: &[&str] = &[HOST_EXE, "conpty.dll", "OpenConsole.exe"];

/// How long a new host gets to make its pipes.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for a host that's busy with another client.
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Solo's side of a terminal host.
#[derive(Clone)]
pub struct HostLink {
    pipe: String,
    keep_running: Arc<AtomicBool>,
    control: Arc<Mutex<File>>,
}

impl HostLink {
    pub fn pipe(&self) -> &str {
        &self.pipe
    }

    /// Whether the host keeps running when Solo goes: while the terminal is
    /// running something. Told to the host when it changes.
    pub fn set_keep_running(&self, keep: bool) {
        if self.keep_running.swap(keep, Ordering::SeqCst) != keep {
            self.control
                .lock()
                .write_all(if keep { b"P" } else { b"E" })
                .ok();
        }
    }

    /// Ends the program and the host now.
    pub fn end(&self) {
        self.control.lock().write_all(b"K").ok();
    }
}

/// The console's controls, for alacritty.
struct HostControl {
    link: HostLink,
}

impl tty::windows::HostedConsole for HostControl {
    fn on_resize(&mut self, window_size: WindowSize) {
        let mut message = vec![b'R'];
        message.extend(window_size.num_cols.to_le_bytes());
        message.extend(window_size.num_lines.to_le_bytes());
        self.link.control.lock().write_all(&message).ok();
    }
}

/// The terminal was closed (or Solo dropped it while quitting, keeping it if
/// it's busy).
impl Drop for HostControl {
    fn drop(&mut self) {
        if !self.link.keep_running.load(Ordering::SeqCst) {
            self.link.end();
        }
    }
}

/// A terminal connected to its host.
pub(crate) struct Connected {
    pub pty: AlacrittyPty,
    pub link: HostLink,
    pub nonce: Option<String>,
    pub terminal_id: Option<String>,
}

#[derive(serde::Serialize)]
struct Spec<'a> {
    protocol: u32,
    pipe: &'a str,
    command_line: String,
    working_directory: Option<String>,
    env: Vec<(&'a String, &'a String)>,
    columns: u16,
    rows: u16,
    nonce: Option<&'a str>,
    terminal_id: Option<&'a str>,
}

/// Starts a host running what `options` say, and connects to it.
pub(crate) fn start(
    options: &tty::Options,
    window_size: WindowSize,
    nonce: Option<&str>,
    terminal_id: Option<&str>,
) -> Result<Connected> {
    let host = host_executable()?;
    let pipe = format!(
        r"\\.\pipe\solo-terminal-{}-{}",
        std::process::id(),
        crate::shell_integration::new_nonce()
    );
    let spec = Spec {
        protocol: PROTOCOL,
        pipe: &pipe,
        command_line: tty::windows::cmdline(options),
        working_directory: options
            .working_directory
            .as_ref()
            .map(|directory| directory.to_string_lossy().into_owned()),
        env: options.env.iter().collect(),
        columns: window_size.num_cols,
        rows: window_size.num_lines,
        nonce,
        terminal_id,
    };
    let spec = serde_json::to_vec(&spec)?;

    // Terminals are built on a background thread, which waits for the host
    // to make its pipes anyway.
    #[allow(clippy::disallowed_methods, reason = "runs on a background thread")]
    let spawn = |flags: u32| {
        Command::new(&host)
            .creation_flags(flags)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Out of a job that would end it with Solo, where the job allows; some
    // launchers' jobs don't, and then it can't outlive Solo started from them.
    let mut child = spawn(flags | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(flags))
        .with_context(|| format!("failed to start {}", host.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&spec)?;
    }
    connect(&pipe, START_TIMEOUT)
}

/// Connects to the host on `pipe`, if it's still there.
pub(crate) fn reconnect(pipe: &str) -> Result<Connected> {
    connect(pipe, Duration::ZERO)
}

/// The host left on `reconnect` if it's still there, else a new host; `None`
/// when neither works, for the terminal to hold its console itself. The bool
/// tells whether it reconnected.
pub(crate) fn open(
    reconnect_to: Option<&str>,
    options: &tty::Options,
    window_size: WindowSize,
    nonce: Option<&str>,
    terminal_id: Option<&str>,
) -> Option<(Connected, bool)> {
    if let Some(pipe) = reconnect_to {
        match reconnect(pipe) {
            Ok(connected) => return Some((connected, true)),
            Err(error) => log::info!("the terminal's host is gone, starting a new one: {error:#}"),
        }
    }
    match start(options, window_size, nonce, terminal_id) {
        Ok(connected) => Some((connected, false)),
        Err(error) => {
            log::warn!("failed to start a terminal host, the terminal ends with Solo: {error:#}");
            None
        }
    }
}

/// Opens the host's pipes, waiting up to `wait_for_start` for it to make
/// them.
fn connect(pipe: &str, wait_for_start: Duration) -> Result<Connected> {
    let started = Instant::now();
    let mut output = open_pipe(&format!("{pipe}-out"), true, started, wait_for_start)?;
    let input = open_pipe(&format!("{pipe}-in"), false, started, START_TIMEOUT)?;
    let control = open_pipe(&format!("{pipe}-ctl"), false, started, START_TIMEOUT)?;

    let header = read_line(&mut output)?;
    let mut fields = header.split(' ');
    if fields.next() != Some(HEADER) {
        bail!("not a terminal host: {header:?}");
    }
    let protocol: u32 = fields.next().unwrap_or("").parse()?;
    if protocol != PROTOCOL {
        bail!("terminal host protocol {protocol}, expected {PROTOCOL}");
    }
    let child_pid: u32 = fields.next().unwrap_or("").parse()?;
    let field = |value: Option<&str>| value.filter(|value| *value != "-").map(str::to_string);
    let nonce = field(fields.next());
    let terminal_id = field(fields.next());

    let child = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            child_pid,
        )
    }
    .context("the terminal host's program is gone")?;

    let link = HostLink {
        pipe: pipe.to_string(),
        keep_running: Arc::new(AtomicBool::new(false)),
        control: Arc::new(Mutex::new(control)),
    };
    let control = Box::new(HostControl { link: link.clone() });
    let pty = unsafe {
        tty::windows::new_hosted(
            output.into_raw_handle(),
            input.into_raw_handle(),
            child.0,
            control,
        )
    }?;
    Ok(Connected {
        pty,
        link,
        nonce,
        terminal_id,
    })
}

const ERROR_PIPE_BUSY: i32 = 231;

fn open_pipe(name: &str, read: bool, started: Instant, wait: Duration) -> Result<File> {
    loop {
        let opened = OpenOptions::new().read(read).write(!read).open(name);
        match opened {
            Ok(pipe) => return Ok(pipe),
            Err(error) => {
                let busy = error.raw_os_error() == Some(ERROR_PIPE_BUSY);
                let missing = error.kind() == io::ErrorKind::NotFound;
                let limit = if busy { wait.max(BUSY_TIMEOUT) } else { wait };
                if !(busy || missing) || started.elapsed() >= limit {
                    return Err(anyhow!(error).context(format!("failed to open {name}")));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn read_line(pipe: &mut File) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < 1024 {
        if pipe.read(&mut byte)? == 0 {
            bail!("the terminal host closed its pipe");
        }
        if byte[0] == b'\n' {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        line.push(byte[0]);
    }
    bail!("no header from the terminal host")
}

/// The host to start: a copy of the one next to Solo, in Solo's data folder,
/// so an update can replace Solo's files while hosts from before run on.
fn host_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let bundled_dir = exe.parent().context("no folder for Solo's executable")?;
    let bundled = bundled_dir.join(HOST_EXE);
    if !bundled.is_file() {
        bail!("{} is missing", bundled.display());
    }
    match copy_host(bundled_dir, &bundled) {
        Ok(copy) => Ok(copy),
        Err(error) => {
            log::warn!("running the terminal host from Solo's folder: {error:#}");
            Ok(bundled)
        }
    }
}

fn copy_host(bundled_dir: &Path, bundled: &Path) -> Result<PathBuf> {
    let metadata = std::fs::metadata(bundled)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hosts = paths::data_dir().join("terminal-host");
    let dir = hosts.join(format!("{}-{modified}", metadata.len()));
    let copy = dir.join(HOST_EXE);
    if copy.is_file() {
        return Ok(copy);
    }
    let staging = hosts.join(format!("staging-{}", crate::shell_integration::new_nonce()));
    std::fs::create_dir_all(&staging)?;
    for name in HOST_FILES {
        let source = bundled_dir.join(name);
        if source.is_file() {
            std::fs::copy(&source, staging.join(name))?;
        }
    }
    // Another Solo may have made it meanwhile.
    if std::fs::rename(&staging, &dir).is_err() {
        std::fs::remove_dir_all(&staging).ok();
    }
    remove_old_hosts(&hosts, &dir);
    if copy.is_file() {
        Ok(copy)
    } else {
        bail!("failed to copy the terminal host to {}", dir.display())
    }
}

/// Copies of other builds' hosts go once no host runs from them (a running
/// one keeps its folder from being deleted).
fn remove_old_hosts(hosts: &Path, current: &Path) {
    let Ok(entries) = std::fs::read_dir(hosts) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_staging = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("staging-"));
        if path != current && !is_staging && path.is_dir() {
            // Deleting the exe first fails while it runs, which keeps the rest.
            if std::fs::remove_file(path.join(HOST_EXE)).is_ok() {
                std::fs::remove_dir_all(&path).ok();
            }
        }
    }
}
