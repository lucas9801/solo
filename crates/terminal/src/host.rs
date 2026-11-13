//! SOLO: terminals whose console a `solo-terminal-host` process holds
//! (`crates/solo_terminal_host`), so the shell and what runs in it can outlive
//! Solo quitting or restarting for an update, and Solo can connect to them
//! again when it opens the terminal next time.
//!
//! Each terminal gets its own host, on its own pipes. A host ends with the
//! program it runs; when Solo lets go of the terminal while it keeps running
//! (the terminal, its workspace or a window closed); and when Solo goes
//! (quits, closes its last window, restarts for an update, or crashes) while
//! the terminal isn't running anything: Solo keeps telling the host whether
//! it is ([`HostLink::set_keep_running`]), so the host knows even when Solo
//! can't say goodbye.
//!
//! Solo doesn't end the host's program itself (as it does a console it holds
//! when the terminal goes): when Solo goes, terminals are dropped while Solo
//! still runs for a moment, and that would end what was to keep running.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::IntoRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::{Duration, Instant};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::tty;
use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{App, Global};
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

/// How long a new host gets to make its pipes, while it runs. It makes them
/// first thing, but with many terminals starting at once (restoring a
/// session) and security software scanning the new processes, starting can
/// take seconds.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a host that's busy with another client.
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Set once Solo is going: quitting (also to install an update), or closing
/// its last window, which quits it. Terminals Solo lets go of then leave
/// their hosts running what they run, for the next Solo; at any other time,
/// letting go of a terminal ends its host.
#[derive(Clone, Default)]
pub(crate) struct SoloGoing(Arc<AtomicBool>);

impl SoloGoing {
    fn set(&self, why: &str) {
        if !self.0.swap(true, Ordering::SeqCst) {
            log::debug!("terminal hosts: Solo is going ({why})");
        }
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct GlobalSoloGoing(SoloGoing);

impl Global for GlobalSoloGoing {}

/// Watches for Solo going, see [`SoloGoing`]. Quit handlers run before the
/// windows (and their terminals) are dropped, and window-closed handlers
/// before the closed window's terminals are.
pub(crate) fn init(cx: &mut App) {
    let going = SoloGoing::default();
    cx.set_global(GlobalSoloGoing(going.clone()));
    let quitting = going.clone();
    cx.on_app_quit(move |_| {
        quitting.set("quitting");
        async {}
    })
    .detach();
    cx.on_window_closed(move |cx, _| {
        if cx.windows().is_empty() {
            going.set("its last window closed");
        }
    })
    .detach();
}

/// The flag [`init`] keeps; one never set without it (tests).
pub(crate) fn solo_going(cx: &App) -> SoloGoing {
    cx.try_global::<GlobalSoloGoing>()
        .map(|global| global.0.clone())
        .unwrap_or_default()
}

/// The hosts' pipes the terminals open in this Solo use, and those of the
/// saved terminals they were restored from, so a saved terminal is restored
/// once: restoring it again (its workspace opened a second time, say) would
/// start a second shell and agent, or wait for its host, busy with the first.
static OPEN_TERMINALS: LazyLock<Mutex<HashMap<String, Weak<()>>>> = LazyLock::new(Default::default);

/// Held by a terminal for as long as it has the pipes it claimed.
pub(crate) type PipeClaim = Arc<()>;

/// Claims `pipe` for a terminal being restored from it, or fails if a
/// terminal in this Solo has it.
pub(crate) fn claim_saved_pipe(pipe: &str) -> Result<PipeClaim> {
    let mut open = OPEN_TERMINALS.lock();
    open.retain(|_, claim| claim.strong_count() > 0);
    if open.contains_key(pipe) {
        bail!("the saved terminal {pipe} is already open in this Solo");
    }
    let claim = PipeClaim::default();
    open.insert(pipe.to_string(), Arc::downgrade(&claim));
    Ok(claim)
}

/// Adds the pipe of a terminal's own host to its claim.
pub(crate) fn add_to_claim(claim: &PipeClaim, pipe: &str) {
    OPEN_TERMINALS
        .lock()
        .insert(pipe.to_string(), Arc::downgrade(claim));
}

/// Solo's side of a terminal host.
#[derive(Clone)]
pub struct HostLink {
    pipe: String,
    keep_running: Arc<AtomicBool>,
    control: Arc<Mutex<Box<dyn Write + Send>>>,
    solo_going: SoloGoing,
}

impl HostLink {
    fn new(pipe: &str, control: Box<dyn Write + Send>, solo_going: SoloGoing) -> Self {
        Self {
            pipe: pipe.to_string(),
            keep_running: Arc::new(AtomicBool::new(false)),
            control: Arc::new(Mutex::new(control)),
            solo_going,
        }
    }

    pub fn pipe(&self) -> &str {
        &self.pipe
    }

    /// Whether the host keeps running when Solo goes: while the terminal is
    /// running something. Told to the host when it changes.
    pub fn set_keep_running(&self, keep: bool) {
        if self.keep_running.swap(keep, Ordering::SeqCst) != keep {
            let byte = if keep { b"P" } else { b"E" };
            log::debug!("terminal host {}: sent {}", self.pipe, char::from(byte[0]));
            self.control.lock().write_all(byte).ok();
        }
    }

    pub fn keeps_running(&self) -> bool {
        self.keep_running.load(Ordering::SeqCst)
    }

    /// Ends the program and the host now.
    pub fn end(&self) {
        log::debug!("terminal host {}: sent K", self.pipe);
        self.control.lock().write_all(b"K").ok();
    }

    /// Solo let go of the terminal: the host ends, unless Solo is going and
    /// the terminal runs something, which then waits for the next Solo.
    pub(crate) fn release(&self) {
        let going = self.solo_going.is_set();
        let keep = self.keeps_running();
        if going && keep {
            log::debug!(
                "terminal host {}: left running for the next Solo",
                self.pipe
            );
        } else {
            log::debug!(
                "terminal host {}: ending it (Solo going: {going}, keep running: {keep})",
                self.pipe
            );
            self.end();
        }
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

/// The terminal was closed (or Solo dropped it while going, keeping it if
/// it's busy).
impl Drop for HostControl {
    fn drop(&mut self) {
        self.link.release();
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
    /// Where the host logs what it's told and why it ends: Solo's logs
    /// folder (that of `--user-data-dir` too). Hosts from before ignore it.
    log_dir: Option<String>,
}

/// Starts a host running what `options` say, and connects to it.
pub(crate) fn start(
    options: &tty::Options,
    window_size: WindowSize,
    nonce: Option<&str>,
    terminal_id: Option<&str>,
    solo_going: SoloGoing,
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
        // Tests don't write to Solo's own logs.
        log_dir: (!cfg!(test)).then(|| paths::logs_dir().to_string_lossy().into_owned()),
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
    let started = Instant::now();
    let connected = connect(&pipe, START_TIMEOUT, Some(&mut child), solo_going);
    if connected.is_err() {
        // Not left waiting for a client that won't come.
        child.kill().ok();
    }
    log::debug!(
        "terminal host {} {} in {:?}",
        child.id(),
        if connected.is_ok() {
            "connected"
        } else {
            "failed"
        },
        started.elapsed()
    );
    connected
}

/// Connects to the host on `pipe`, if it's still there.
pub(crate) fn reconnect(pipe: &str, solo_going: SoloGoing) -> Result<Connected> {
    connect(pipe, Duration::ZERO, None, solo_going)
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
    solo_going: SoloGoing,
) -> Option<(Connected, bool)> {
    if let Some(pipe) = reconnect_to {
        match reconnect(pipe, solo_going.clone()) {
            Ok(connected) => return Some((connected, true)),
            Err(error) => log::info!("the terminal's host is gone, starting a new one: {error:#}"),
        }
    }
    match start(options, window_size, nonce, terminal_id, solo_going) {
        Ok(connected) => Some((connected, false)),
        Err(error) => {
            log::warn!("failed to start a terminal host, the terminal ends with Solo: {error:#}");
            None
        }
    }
}

/// Opens the host's pipes, waiting up to `wait_for_start` for it to make
/// them, while `host` (one just started) runs.
fn connect(
    pipe: &str,
    wait_for_start: Duration,
    mut host: Option<&mut Child>,
    solo_going: SoloGoing,
) -> Result<Connected> {
    let started = Instant::now();
    let mut output = open_pipe(
        &format!("{pipe}-out"),
        true,
        started,
        wait_for_start,
        host.as_deref_mut(),
    )?;
    let input = open_pipe(
        &format!("{pipe}-in"),
        false,
        started,
        START_TIMEOUT,
        host.as_deref_mut(),
    )?;
    let control = open_pipe(&format!("{pipe}-ctl"), false, started, START_TIMEOUT, host)?;

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

    let link = HostLink::new(pipe, Box::new(control), solo_going);
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

fn open_pipe(
    name: &str,
    read: bool,
    started: Instant,
    wait: Duration,
    mut host: Option<&mut Child>,
) -> Result<File> {
    loop {
        let opened = OpenOptions::new().read(read).write(!read).open(name);
        match opened {
            Ok(pipe) => return Ok(pipe),
            Err(error) => {
                let busy = error.raw_os_error() == Some(ERROR_PIPE_BUSY);
                let missing = error.kind() == io::ErrorKind::NotFound;
                let limit = if busy { wait.max(BUSY_TIMEOUT) } else { wait };
                if !(busy || missing) || started.elapsed() >= limit {
                    return Err(anyhow!(error).context(format!(
                        "failed to open {name} after {:?}",
                        started.elapsed()
                    )));
                }
                if let Some(status) = host
                    .as_mut()
                    .and_then(|host| host.try_wait().ok().flatten())
                {
                    bail!("the terminal host exited ({status}) before making {name}");
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
    if cfg!(test) {
        test_host_executable()
    } else {
        installed_host_executable()
    }
}

/// Tests run from `target/<profile>/deps`; the host is built (with `cargo
/// build -p solo_terminal_host`) in the folder above, and runs from there
/// rather than from a copy in Solo's data folder.
fn test_host_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let host = exe
        .parent()
        .and_then(Path::parent)
        .context("no target folder")?
        .join(HOST_EXE);
    if !host.is_file() {
        bail!(
            "{} is missing: build it first (cargo build -p solo_terminal_host)",
            host.display()
        );
    }
    Ok(host)
}

fn installed_host_executable() -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use collections::HashMap;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use util::paths::PathStyle;
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::WaitForSingleObject;

    use super::*;
    use crate::terminal_settings::{AlternateScroll, CursorShape};
    use crate::{Terminal, TerminalBuilder, TerminalMode};

    fn init_test(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            let settings = settings::SettingsStore::test(cx);
            cx.set_global(settings);
            crate::init(cx);
        });
    }

    fn build(
        command: &str,
        env: HashMap<String, String>,
        cx: &mut TestAppContext,
    ) -> gpui::Task<Result<TerminalBuilder>> {
        cx.update(|cx| {
            TerminalBuilder::new(
                None,
                TerminalMode::interactive(),
                task::Shell::WithArguments {
                    program: "cmd.exe".to_string(),
                    args: vec!["/d".to_string(), "/q".to_string(), command.to_string()],
                    title_override: None,
                },
                env,
                CursorShape::default(),
                AlternateScroll::On,
                None,
                vec![],
                Duration::ZERO,
                false,
                0,
                cx,
                vec![],
                PathStyle::local(),
            )
        })
    }

    /// A terminal in a terminal host, running `command` with cmd.exe.
    async fn hosted_terminal(cx: &mut TestAppContext, command: &str) -> Entity<Terminal> {
        let builder = build(command, HashMap::default(), cx)
            .await
            .expect("failed to build the terminal");
        let terminal = cx.new(|cx| builder.subscribe(cx));
        assert!(
            terminal.read_with(cx, |terminal, _| terminal.host_pipe().is_some()),
            "the terminal isn't in a host"
        );
        terminal
    }

    fn pipe_and_pid(terminal: &Entity<Terminal>, cx: &TestAppContext) -> (String, u32) {
        terminal.read_with(cx, |terminal, _| {
            (
                terminal.host_pipe().unwrap().to_string(),
                terminal.pid().expect("no program").as_u32(),
            )
        })
    }

    fn is_running(pid: u32) -> bool {
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
                return false;
            };
            let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
            CloseHandle(process).ok();
            running
        }
    }

    /// Waits for `pid` to end, ten seconds at most.
    fn ends(pid: u32) -> bool {
        let started = Instant::now();
        while is_running(pid) {
            if started.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// Drops the terminals, as quitting does with the windows', and runs
    /// what dropping them started, past the 100 ms after which Solo ends a
    /// console it holds itself.
    fn drop_terminals(terminals: Vec<Entity<Terminal>>, cx: &mut TestAppContext) {
        drop(terminals);
        // Entities are released when effects are flushed.
        cx.update(|_| {});
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    }

    /// Restarting for an update quits Solo as Quit does (`cx.quit()`, after
    /// the windows were prepared to close): the terminal running an agent (a
    /// long ping here) keeps running for the next Solo, which connects to the
    /// same program; the idle shell ends.
    #[gpui::test]
    async fn test_update_restart_keeps_the_busy_terminal(cx: &mut TestAppContext) {
        init_test(cx);
        let busy = hosted_terminal(cx, "/c ping -n 120 127.0.0.1").await;
        let idle = hosted_terminal(cx, "/k").await;
        let (busy_pipe, busy_pid) = pipe_and_pid(&busy, cx);
        let (idle_pipe, idle_pid) = pipe_and_pid(&idle, cx);
        // What the terminal views say: one runs something, one doesn't.
        busy.read_with(cx, |terminal, _| terminal.keep_running_after_quit(true));
        idle.read_with(cx, |terminal, _| terminal.keep_running_after_quit(false));

        // Quitting runs the quit handlers, then drops the windows' terminals,
        // while Solo still runs for a moment.
        cx.update(|cx| cx.shutdown());
        drop_terminals(vec![busy, idle], cx);

        assert!(ends(idle_pid), "the idle shell kept running");
        assert!(reconnect(&idle_pipe, SoloGoing::default()).is_err());
        std::thread::sleep(Duration::from_millis(500));
        assert!(is_running(busy_pid), "the busy terminal's program ended");

        // The next Solo finds the same program.
        let connected =
            reconnect(&busy_pipe, SoloGoing::default()).expect("the busy terminal's host is gone");
        assert_eq!(
            connected.pty.child_watcher().pid(),
            NonZeroU32::new(busy_pid)
        );
        connected.link.end();
        drop(connected);
        assert!(ends(busy_pid), "K didn't end the program");
    }

    /// Closing a terminal (or its workspace, or a window) while Solo keeps
    /// running ends its host even while it runs something, rather than
    /// leave a host no terminal will connect to.
    #[gpui::test]
    async fn test_closing_a_busy_terminal_ends_its_host(cx: &mut TestAppContext) {
        init_test(cx);
        let busy = hosted_terminal(cx, "/c ping -n 120 127.0.0.1").await;
        let (busy_pipe, busy_pid) = pipe_and_pid(&busy, cx);
        busy.read_with(cx, |terminal, _| terminal.keep_running_after_quit(true));

        drop_terminals(vec![busy], cx);

        assert!(ends(busy_pid), "the closed terminal's program kept running");
        assert!(reconnect(&busy_pipe, SoloGoing::default()).is_err());
    }

    /// A saved terminal is restored once: restoring it again while the first
    /// is open fails, rather than start a second shell and agent.
    #[gpui::test]
    async fn test_a_saved_terminal_is_restored_once(cx: &mut TestAppContext) {
        init_test(cx);
        let saved = format!(
            r"\\.\pipe\solo-terminal-test-gone-{}",
            crate::shell_integration::new_nonce()
        );
        let env = HashMap::from_iter([(crate::HOST_RECONNECT_ENV.to_string(), saved.clone())]);
        // Its host is gone: a new one starts.
        let first = build("/k", env.clone(), cx)
            .await
            .expect("the first restore failed");
        let first = cx.new(|cx| first.subscribe(cx));
        let (first_pipe, first_pid) = pipe_and_pid(&first, cx);
        assert_ne!(first_pipe, saved);

        let error = build("/k", env.clone(), cx)
            .await
            .err()
            .expect("restored twice");
        assert!(error.to_string().contains("already open"), "{error:#}");
        // Nor can another terminal connect to the first one's host.
        let first_pipe_env =
            HashMap::from_iter([(crate::HOST_RECONNECT_ENV.to_string(), first_pipe.clone())]);
        assert!(build("/k", first_pipe_env, cx).await.is_err());

        // Once the first is closed, the saved terminal can be opened again.
        drop_terminals(vec![first], cx);
        assert!(ends(first_pid));
        let again = build("/k", env, cx)
            .await
            .expect("couldn't restore after closing");
        let again = cx.new(|cx| again.subscribe(cx));
        let (_, again_pid) = pipe_and_pid(&again, cx);
        drop_terminals(vec![again], cx);
        assert!(ends(again_pid));
    }

    struct EmptyWindow;

    impl gpui::Render for EmptyWindow {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::Empty
        }
    }

    /// Closing the last window quits Solo: its terminals, dropped before the
    /// quit handlers run, already count as Solo going.
    #[gpui::test]
    fn test_solo_goes_when_its_last_window_closes(cx: &mut TestAppContext) {
        cx.update(init);
        let going = cx.update(|cx| solo_going(cx));
        let first = cx.add_window(|_, _| EmptyWindow);
        let last = cx.add_window(|_, _| EmptyWindow);
        first
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(!going.is_set(), "a window closed, but not the last");
        last.update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(going.is_set());
    }

    #[test]
    fn test_releasing_a_link() {
        #[derive(Clone, Default)]
        struct Sent(Arc<Mutex<Vec<u8>>>);
        impl Write for Sent {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let release = |keep: bool, going: bool| {
            let sent = Sent::default();
            let solo_going = SoloGoing::default();
            if going {
                solo_going.set("test");
            }
            let link = HostLink::new("test", Box::new(sent.clone()), solo_going);
            link.set_keep_running(keep);
            link.release();
            String::from_utf8(sent.0.lock().clone()).unwrap()
        };
        // Solo going: what runs something waits for the next Solo.
        assert_eq!(release(true, true), "P");
        assert_eq!(release(false, true), "K");
        // Closed while Solo runs on: ended either way.
        assert_eq!(release(true, false), "PK");
        assert_eq!(release(false, false), "K");
    }
}
