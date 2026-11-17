use std::collections::HashSet;
use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::LibraryLoader::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

use crate::log::Log;
use crate::screen::Screen;
use crate::{HEADER, PROTOCOL, REPLAY_END, Running, Spec, running};

/// Only the user who made the pipes (Solo's user) may open them. SOLO: and
/// only from medium integrity up: unlabeled, a low integrity process of that
/// user (a sandboxed browser, say) could still read them (R3 L-H1).
const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)S:(ML;;NWNR;;;ME)";

pub fn main() {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(2);
    }
    let Ok(spec) = serde_json::from_str::<Spec>(&input) else {
        std::process::exit(2);
    };
    if spec.protocol != PROTOCOL {
        std::process::exit(2);
    }
    let log = Arc::new(Log::new(spec.log_dir.as_deref(), &spec.pipe));
    // The pipes first, so Solo can open them while the program starts (which
    // can take seconds with many terminals starting at once, or security
    // software scanning). Solo then waits for the header, which follows once
    // the program runs, or for the pipes to break if it can't start.
    let Some(first) = Pipes::make(&spec.pipe, true) else {
        exit(
            &log,
            4,
            "couldn't make the pipes (someone else has the name)",
        );
    };
    let console = match Console::spawn(&spec, &log) {
        Ok(console) => Arc::new(console),
        Err(error) => exit(
            &log,
            3,
            &format!("couldn't start {}: {error}", spec.command_line),
        ),
    };
    log.write(&format!(
        "started {} (pid {}) in {}",
        spec.command_line,
        console.pid,
        spec.working_directory.as_deref().unwrap_or("-")
    ));
    running::add(
        spec.running_dir.as_deref(),
        Running {
            protocol: PROTOCOL,
            host_pid: std::process::id(),
            host_created: running::own_creation_time(),
            child_pid: console.pid,
            pipe: spec.pipe.clone(),
            terminal_id: spec.terminal_id.clone(),
            command_line: spec.command_line.clone(),
            working_directory: spec.working_directory.clone(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
            clients: 0,
            connected: false,
        },
    );
    serve(spec, console, first, log);
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn file(handle: HANDLE) -> File {
    unsafe { File::from_raw_handle(handle as RawHandle) }
}

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

/// The pseudo console API: from the `conpty.dll` next to the host (with its
/// newer `OpenConsole.exe`, as Solo itself uses), else Windows' own.
struct ConptyApi {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

impl ConptyApi {
    fn load() -> Self {
        let bundled = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("conpty.dll")))
            .filter(|dll| dll.is_file());
        if let Some(dll) = bundled {
            unsafe {
                let module = LoadLibraryW(wide(&dll).as_ptr());
                if !module.is_null() {
                    let create = GetProcAddress(module, c"CreatePseudoConsole".as_ptr().cast());
                    let resize = GetProcAddress(module, c"ResizePseudoConsole".as_ptr().cast());
                    let close = GetProcAddress(module, c"ClosePseudoConsole".as_ptr().cast());
                    if let (Some(create), Some(resize), Some(close)) = (create, resize, close) {
                        return Self {
                            create: transmute::<unsafe extern "system" fn() -> isize, CreateFn>(
                                create,
                            ),
                            resize: transmute::<unsafe extern "system" fn() -> isize, ResizeFn>(
                                resize,
                            ),
                            close: transmute::<unsafe extern "system" fn() -> isize, CloseFn>(
                                close,
                            ),
                        };
                    }
                }
            }
        }
        Self {
            create: CreatePseudoConsole,
            resize: ResizePseudoConsole,
            close: ClosePseudoConsole,
        }
    }
}

/// `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY`'s value for "the process
/// being created starts its children outside the package".
const PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE: u32 = 0x01;

/// Whether the host runs in a package (Solo from the Microsoft Store).
fn is_packaged() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentPackageFamilyName(length: *mut u32, name: *mut u16) -> i32;
    }
    let mut length = 0u32;
    // SAFETY: with no buffer it only writes the length it needs; it fails
    // with APPMODEL_ERROR_NO_PACKAGE in a process without a package.
    let result = unsafe { GetCurrentPackageFamilyName(&mut length, null_mut()) };
    result == ERROR_INSUFFICIENT_BUFFER as i32
}

/// A pseudo console with the program running in it.
struct Console {
    api: ConptyApi,
    handle: HPCON,
    input: File,
    output: File,
    process: HANDLE,
    pid: u32,
}

unsafe impl Send for Console {}
unsafe impl Sync for Console {}

impl Console {
    fn spawn(spec: &Spec, log: &Log) -> io::Result<Console> {
        let api = ConptyApi::load();
        unsafe {
            let (mut in_read, mut in_write) = (null_mut(), null_mut());
            let (mut out_read, mut out_write) = (null_mut(), null_mut());
            if CreatePipe(&mut in_read, &mut in_write, null(), 0) == 0
                || CreatePipe(&mut out_read, &mut out_write, null(), 0) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let size = COORD {
                X: spec.columns.max(2) as i16,
                Y: spec.rows.max(1) as i16,
            };
            let mut handle: HPCON = zeroed();
            let result = (api.create)(size, in_read, out_write, 0, &mut handle);
            CloseHandle(in_read);
            CloseHandle(out_write);
            if result < 0 {
                return Err(io::Error::from_raw_os_error(result));
            }

            let packaged = is_packaged();
            let attributes = if packaged { 2 } else { 1 };
            let mut list_size = 0usize;
            InitializeProcThreadAttributeList(null_mut(), attributes, 0, &mut list_size);
            let mut list_buffer = vec![0u8; list_size];
            let list = list_buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            if InitializeProcThreadAttributeList(list, attributes, 0, &mut list_size) == 0
                || UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    handle as *const c_void,
                    size_of::<HPCON>(),
                    null_mut(),
                    null(),
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Started by a packaged Solo (the Store edition), the host runs in
            // Solo's package, whose writes to %APPDATA% and %LOCALAPPDATA%
            // Windows redirects to a folder of the package's own. What runs
            // in the terminal (the user's tools and agents, signing in,
            // installing) mustn't be: it's started outside the package.
            let breakaway: u32 = PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE;
            if packaged
                && UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
                    &breakaway as *const u32 as *const c_void,
                    size_of::<u32>(),
                    null_mut(),
                    null(),
                ) == 0
            {
                log.write(&format!(
                    "starting the program in Solo's package: {}",
                    io::Error::last_os_error()
                ));
            }
            let mut startup: STARTUPINFOEXW = zeroed();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            // Not the host's own standard handles: the pseudo console's.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.lpAttributeList = list;

            let mut command_line = wide(&spec.command_line);
            let directory = spec.working_directory.as_ref().map(wide);
            let environment = environment_block(&spec.env);
            let mut info: PROCESS_INFORMATION = zeroed();
            let created = CreateProcessW(
                null(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr() as *const c_void,
                directory
                    .as_ref()
                    .map_or(null(), |directory| directory.as_ptr()),
                &startup.StartupInfo,
                &mut info,
            );
            DeleteProcThreadAttributeList(list);
            if created == 0 {
                return Err(io::Error::last_os_error());
            }
            CloseHandle(info.hThread);
            Ok(Console {
                api,
                handle,
                input: file(in_write),
                output: file(out_read),
                process: info.hProcess,
                pid: info.dwProcessId,
            })
        }
    }

    fn resize(&self, columns: u16, rows: u16) {
        let size = COORD {
            X: columns.max(2) as i16,
            Y: rows.max(1) as i16,
        };
        unsafe {
            (self.api.resize)(self.handle, size);
        }
    }

    /// Ends the program and closes the console. The output must still be
    /// read meanwhile: closing waits for it to drain.
    fn kill(&self) {
        unsafe {
            TerminateProcess(self.process, 1);
            (self.api.close)(self.handle);
        }
    }
}

/// `env` first, then the host's own variables not in it; Windows variables
/// are case-insensitive and the first of a name wins.
fn environment_block(env: &[(String, String)]) -> Vec<u16> {
    let mut block = Vec::new();
    let mut seen = HashSet::new();
    let own = std::env::vars_os().map(|(key, value)| {
        (
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        )
    });
    for (key, value) in env.iter().cloned().chain(own) {
        if key.is_empty() || !seen.insert(key.to_uppercase()) {
            continue;
        }
        block.extend(OsStr::new(&key).encode_wide());
        block.push('=' as u16);
        block.extend(OsStr::new(&value).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Makes one instance of a one-way pipe, for a client to open. The first
/// instance of a name must be new, so nobody else's pipe is served.
fn make_pipe(name: &str, outbound: bool, first: bool) -> io::Result<HANDLE> {
    unsafe {
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(OWNER_ONLY).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let access = if outbound {
            PIPE_ACCESS_OUTBOUND
        } else {
            PIPE_ACCESS_INBOUND
        };
        let handle = CreateNamedPipeW(
            wide(name).as_ptr(),
            access
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 16,
            1 << 16,
            0,
            &attributes,
        );
        LocalFree(descriptor as HLOCAL);
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }
}

/// Waits for the client of a pipe instance (which may have opened it
/// already).
fn connect_pipe(handle: HANDLE) -> io::Result<()> {
    unsafe {
        if ConnectNamedPipe(handle, null_mut()) == 0 && GetLastError() != ERROR_PIPE_CONNECTED {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// One instance of each of the three pipes, for one client at a time.
///
/// SOLO: the host keeps these instances for as long as it runs, disconnecting
/// a client that went and connecting the next to the same ones, so the pipes'
/// names are never free for another program to take in between (R3 M9).
/// Disconnecting also ends whatever the host was still reading or writing for
/// the client that went (R3 L-H3).
struct Pipes {
    out: HANDLE,
    input: HANDLE,
    control: HANDLE,
}

impl Pipes {
    fn make(name: &str, first: bool) -> Option<Pipes> {
        let out = make_retrying(&format!("{name}-out"), true, first)?;
        let Some(input) = make_retrying(&format!("{name}-in"), false, first) else {
            unsafe { CloseHandle(out) };
            return None;
        };
        let Some(control) = make_retrying(&format!("{name}-ctl"), false, first) else {
            unsafe {
                CloseHandle(out);
                CloseHandle(input);
            }
            return None;
        };
        Some(Pipes {
            out,
            input,
            control,
        })
    }

    /// Waits for the client to open all three, in the order Solo opens them,
    /// and returns files for them (of their own, to be closed as wanted).
    fn connect(&self) -> Option<(File, File, File)> {
        for handle in [self.out, self.input, self.control] {
            if connect_pipe(handle).is_err() {
                self.disconnect();
                return None;
            }
        }
        let files = (
            duplicate(self.out).ok()?,
            duplicate(self.input).ok()?,
            duplicate(self.control).ok()?,
        );
        Some(files)
    }

    /// [`Pipes::connect`], giving up after `timeout`. The connecting thread
    /// is left waiting then, so only for a host about to end.
    fn connect_within(self: &Arc<Self>, timeout: Duration) -> Option<(File, File, File)> {
        let (sender, receiver) = mpsc::channel();
        let pipes = self.clone();
        thread::spawn(move || {
            sender.send(pipes.connect()).ok();
        });
        receiver.recv_timeout(timeout).ok().flatten()
    }

    /// Disconnects the client, if any, for the next one to connect: its ends
    /// close, and so reading or writing them fails.
    fn disconnect(&self) {
        for handle in [self.out, self.input, self.control] {
            unsafe { DisconnectNamedPipe(handle) };
        }
    }
}

/// A file of its own for the pipe instance `handle`.
fn duplicate(handle: HANDLE) -> io::Result<File> {
    unsafe {
        let process = GetCurrentProcess();
        let mut duplicate: HANDLE = null_mut();
        if DuplicateHandle(
            process,
            handle,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(file(duplicate))
    }
}

// The handles are only pipe instances this process made, kept open for as
// long as it runs.
unsafe impl Send for Pipes {}
unsafe impl Sync for Pipes {}

/// How long the host waits for its first client: Solo connects right after
/// starting it, so a host nobody connects to (Solo crashed or was ended in
/// between) ends rather than keep a shell running for no one.
const FIRST_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

/// The screen the output drew, for the next client, and the client being
/// written to. Output is parsed and passed on under one lock, so each byte
/// is either in the screen a client gets or sent to it after.
struct Shared {
    screen: Screen,
    /// SOLO: what's sent to the client goes through a queue to a thread of
    /// its own, so a client that doesn't read holds up neither the program
    /// nor the host (R3 L-H3).
    client: Option<mpsc::SyncSender<Vec<u8>>>,
}

/// How many pieces of output (up to 64 KB each) wait for a client that
/// doesn't read before the host disconnects it.
const CLIENT_QUEUE: usize = 256;

/// Writes what's queued for the client to it, until either goes.
fn write_to_client(mut out: File, queue: mpsc::Receiver<Vec<u8>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for bytes in queue {
            if out.write_all(&bytes).is_err() {
                break;
            }
        }
    })
}

/// Cancels what `thread` waits for reading or writing (a pipe's other end may
/// never read or write), again until it ends. A pipe's handles share one file
/// object, which does one thing at a time: disconnecting the pipe would wait
/// for that otherwise.
fn cancel_until_finished<T>(thread: &thread::JoinHandle<T>) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::System::IO::CancelSynchronousIo;
    while !thread.is_finished() {
        unsafe { CancelSynchronousIo(thread.as_raw_handle() as HANDLE) };
        thread::sleep(Duration::from_millis(10));
    }
}

/// The thread reading the client's control pipe, for the output thread to
/// stop it reading when the client doesn't read what it's sent.
struct ControlReader {
    thread: HANDLE,
    /// Set while the client is to be let go: the reading is cancelled until
    /// cleared.
    stalled: std::sync::atomic::AtomicBool,
}

// The thread handle is only used to cancel what it reads.
unsafe impl Send for ControlReader {}
unsafe impl Sync for ControlReader {}

impl ControlReader {
    fn current() -> Arc<Self> {
        let thread = unsafe { OpenThread(THREAD_TERMINATE, 0, GetCurrentThreadId()) };
        Arc::new(Self {
            thread,
            stalled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Stops the control thread reading until [`Self::resume`].
    fn stall(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        use windows_sys::Win32::System::IO::CancelSynchronousIo;
        if self.thread.is_null() || self.stalled.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        thread::spawn(move || {
            while this.stalled.load(Ordering::SeqCst) {
                unsafe { CancelSynchronousIo(this.thread) };
                thread::sleep(Duration::from_millis(10));
            }
        });
    }

    /// Whether it was stalled; it isn't any more.
    fn resume(&self) -> bool {
        self.stalled
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

/// [`make_pipe`], trying again for a few seconds if the pipe can't be made.
fn make_retrying(name: &str, outbound: bool, first: bool) -> Option<HANDLE> {
    for _ in 0..50 {
        match make_pipe(name, outbound, first) {
            Ok(pipe) => return Some(pipe),
            // Someone else holds the name: nobody can be served under it.
            Err(_) if first => return None,
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    None
}

/// Exits the host, saying why in its log.
fn exit(log: &Log, code: i32, why: &str) -> ! {
    running::remove();
    log.write(&format!("ending: {why}"));
    std::process::exit(code);
}

fn serve(spec: Spec, console: Arc<Console>, pipes: Pipes, log: Arc<Log>) {
    let pipes = Arc::new(pipes);
    let control_reader = ControlReader::current();
    let shared = Arc::new(Mutex::new(Shared {
        screen: Screen::new(spec.columns, spec.rows),
        client: None,
    }));

    // Output, kept and passed on. Reading it never stops, or the program
    // would block on a full console.
    {
        let (console, shared, control_reader, log) = (
            console.clone(),
            shared.clone(),
            control_reader.clone(),
            log.clone(),
        );
        thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let read = match (&console.output).read(&mut buffer) {
                    Ok(0) => break,
                    Err(error) => {
                        log.write(&format!("reading the console's output failed: {error}"));
                        break;
                    }
                    Ok(read) => read,
                };
                let bytes = &buffer[..read];
                let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
                shared.screen.feed(bytes);
                let sent = shared
                    .client
                    .as_ref()
                    .map(|client| client.try_send(bytes.to_vec()));
                match sent {
                    None | Some(Ok(())) => {}
                    Some(Err(mpsc::TrySendError::Disconnected(_))) => shared.client = None,
                    // It stopped reading: it's let go as if it went, and gets
                    // the screen anew when it connects again.
                    Some(Err(mpsc::TrySendError::Full(_))) => {
                        shared.client = None;
                        control_reader.stall();
                    }
                }
            }
        });
    }

    // The host ends with the program.
    {
        let console = console.clone();
        let log = log.clone();
        thread::spawn(move || {
            let mut code = 0u32;
            unsafe {
                WaitForSingleObject(console.process, INFINITE);
                GetExitCodeProcess(console.process, &mut code);
            }
            // Its last output reaches the client first.
            thread::sleep(Duration::from_millis(300));
            unsafe {
                (console.api.close)(console.handle);
            }
            thread::sleep(Duration::from_millis(200));
            exit(&log, 0, &format!("the program exited ({code:#x})"));
        });
    }

    let header = |columns: u16, rows: u16| {
        format!(
            "{HEADER} {PROTOCOL} {} {} {} {columns} {rows}\n",
            console.pid,
            spec.nonce.as_deref().unwrap_or("-"),
            spec.terminal_id.as_deref().unwrap_or("-"),
        )
    };
    // Whether a client said to keep running: then the host waits for the
    // next client, even past one that opened the pipes and went.
    let mut kept = false;
    let mut clients = 0u32;
    loop {
        // Only the first client is waited for at most so long; after a
        // client said to keep running, the next may come any time.
        let connected = if kept {
            pipes.connect()
        } else {
            pipes.connect_within(FIRST_CLIENT_TIMEOUT)
        };
        let Some((out, input, control)) = connected else {
            if kept {
                log.write("a client failed to connect; waiting for the next");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            console.kill();
            exit(
                &log,
                4,
                "no client connected in time (Solo crashed or ended after starting the host)",
            );
        };
        clients += 1;
        // Whether to keep running when the client goes: the first client
        // starts from "end with me", and says otherwise; the next ones from
        // "keep running", as the last one said, so a client that connects
        // and goes without a word (Solo crashing right away, say) doesn't end
        // what was to keep running.
        let mut keep = kept;
        let (size, writer) = {
            let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
            let (columns, rows) = (shared.screen.columns(), shared.screen.rows());
            let screen = shared.screen.snapshot();
            log.write(&format!(
                "client {clients} connected; sending the {columns}x{rows} screen ({} bytes)",
                screen.len()
            ));
            let (sender, queue) = mpsc::sync_channel(CLIENT_QUEUE);
            for bytes in [
                header(columns, rows).into_bytes(),
                screen,
                REPLAY_END.to_vec(),
            ] {
                sender.try_send(bytes).ok();
            }
            shared.client = Some(sender);
            ((columns, rows), write_to_client(out, queue))
        };
        // Windows' own console repaints the whole screen when resized, also
        // to the size it has, so the client also gets the console's idea of
        // the screen, should the two differ. (The newer one Solo bundles
        // doesn't repaint, even for a new size; the screen sent is all the
        // client gets then.)
        console.resize(size.0, size.1);
        running::update(|entry| {
            entry.clients = clients;
            entry.connected = true;
        });
        let forward_input = {
            let console = console.clone();
            let mut input = input;
            thread::spawn(move || {
                let mut buffer = vec![0u8; 16 * 1024];
                while let Ok(read) = input.read(&mut buffer) {
                    if read == 0 || (&console.input).write_all(&buffer[..read]).is_err() {
                        break;
                    }
                }
            })
        };
        let mut control = BufReader::new(control);
        let gone = loop {
            let mut kind = [0u8; 1];
            if let Err(error) = control.read_exact(&mut kind) {
                break format!("its control pipe closed ({error})");
            }
            match kind[0] {
                b'R' => {
                    let mut size = [0u8; 4];
                    if let Err(error) = control.read_exact(&mut size) {
                        break format!("its control pipe closed mid-message ({error})");
                    }
                    let columns = u16::from_le_bytes([size[0], size[1]]);
                    let rows = u16::from_le_bytes([size[2], size[3]]);
                    log.write(&format!("got R {columns}x{rows}"));
                    // The output after resizing is drawn for the new size.
                    shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen
                        .resize(columns, rows);
                    console.resize(columns, rows);
                }
                b'P' => {
                    log.write("got P");
                    keep = true;
                }
                b'E' => {
                    log.write("got E");
                    keep = false;
                }
                b'K' => {
                    log.write("got K");
                    console.kill();
                    thread::sleep(Duration::from_millis(200));
                    exit(&log, 0, "Solo said to end (K)");
                }
                other => break format!("it sent an unknown control byte {other:#04x}"),
            }
        };
        let gone = if control_reader.resume() {
            "it stopped reading what it was sent".to_string()
        } else {
            gone
        };
        log.write(&format!(
            "client {clients} gone: {gone}; keep running: {keep}"
        ));
        // The client went: end with it, unless the program is busy; then
        // keep running for the next one, once its pipe instances are closed.
        if !keep {
            console.kill();
            thread::sleep(Duration::from_millis(200));
            exit(
                &log,
                0,
                "the client went while the terminal wasn't to keep running",
            );
        }
        kept = true;
        drop(control);
        shared.lock().unwrap_or_else(|e| e.into_inner()).client = None;
        running::update(|entry| entry.connected = false);
        // Writing to it and forwarding its input end even if it neither
        // reads nor closes its input (R3 L-H3), then the pipe instances are
        // free for the next client.
        cancel_until_finished(&writer);
        cancel_until_finished(&forward_input);
        pipes.disconnect();
        log.write("waiting for the next client");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_pipe_name() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!(
            r"\\.\pipe\solo-terminal-host-test-{}-{nanos}",
            std::process::id()
        )
    }

    #[test]
    fn test_first_client_times_out() {
        let name = unique_pipe_name();
        let pipes = Arc::new(Pipes::make(&name, true).expect("pipes"));
        let started = std::time::Instant::now();
        assert!(pipes.connect_within(Duration::from_millis(300)).is_none());
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(300), "{waited:?}");
        assert!(waited < Duration::from_secs(10), "{waited:?}");
    }

    #[test]
    fn test_first_client_connects_in_time() {
        let name = unique_pipe_name();
        let pipes = Arc::new(Pipes::make(&name, true).expect("pipes"));
        let client = thread::spawn(move || {
            let out = std::fs::OpenOptions::new()
                .read(true)
                .open(format!("{name}-out"))
                .expect("out");
            let input = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("{name}-in"))
                .expect("in");
            let control = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("{name}-ctl"))
                .expect("ctl");
            (out, input, control)
        });
        let (mut out, _input, _control) = pipes
            .connect_within(Duration::from_secs(10))
            .expect("the client connected");
        let (mut client_out, _client_input, _client_control) = client.join().unwrap();
        out.write_all(b"hi").unwrap();
        let mut read = [0u8; 2];
        client_out.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hi");
    }

    /// The pipes are labeled medium integrity, with no reading or writing up:
    /// a low integrity process can't open them (R3 L-H1).
    #[test]
    fn test_pipes_keep_out_low_integrity() {
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
        };
        let name = unique_pipe_name();
        let pipe = make_pipe(&name, true, true).expect("pipe");
        let sddl = unsafe {
            let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
            let error = GetSecurityInfo(
                pipe,
                SE_KERNEL_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            );
            CloseHandle(pipe);
            assert_eq!(error, 0, "GetSecurityInfo");
            let mut text: windows_sys::core::PWSTR = null_mut();
            let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                LABEL_SECURITY_INFORMATION,
                &mut text,
                null_mut(),
            );
            LocalFree(descriptor as HLOCAL);
            assert_ne!(
                converted, 0,
                "ConvertSecurityDescriptorToStringSecurityDescriptorW"
            );
            let len = (0..).take_while(|&ix| *text.add(ix) != 0).count();
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text as HLOCAL);
            sddl
        };
        assert!(sddl.contains("ML;;NWNR;;;ME"), "{sddl}");
    }

    #[test]
    fn test_environment_block() {
        let block = environment_block(&[
            ("SOLO_X".to_string(), "1".to_string()),
            ("solo_x".to_string(), "2".to_string()),
        ]);
        let text = String::from_utf16_lossy(&block);
        let entries: Vec<&str> = text.split('\0').collect();
        // The first of a name wins; Windows names ignore case.
        assert!(entries.contains(&"SOLO_X=1"));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case("solo_x=2"))
        );
        // Double NUL at the end.
        assert!(block.ends_with(&[0, 0]));
    }
}
