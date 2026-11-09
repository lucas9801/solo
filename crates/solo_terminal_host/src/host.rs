use std::collections::{HashSet, VecDeque};
use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::LibraryLoader::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

use crate::{HEADER, PROTOCOL, REPLAY_END, RING_CAPACITY, Spec};

/// Only the user who made the pipes (Solo's user) may open them.
const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

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
    // The pipes first, so Solo can open them while the program starts (which
    // can take seconds with many terminals starting at once, or security
    // software scanning). Solo then waits for the header, which follows once
    // the program runs, or for the pipes to break if it can't start.
    let Some(first) = Pipes::make(&spec.pipe, true) else {
        std::process::exit(4);
    };
    let console = match Console::spawn(&spec) {
        Ok(console) => Arc::new(console),
        Err(_) => std::process::exit(3),
    };
    serve(spec, console, first);
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
    fn spawn(spec: &Spec) -> io::Result<Console> {
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

            let mut list_size = 0usize;
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut list_size);
            let mut list_buffer = vec![0u8; list_size];
            let list = list_buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            if InitializeProcThreadAttributeList(list, 1, 0, &mut list_size) == 0
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
fn connect_pipe(handle: HANDLE) -> io::Result<File> {
    unsafe {
        if ConnectNamedPipe(handle, null_mut()) == 0 && GetLastError() != ERROR_PIPE_CONNECTED {
            let error = io::Error::last_os_error();
            CloseHandle(handle);
            return Err(error);
        }
        Ok(file(handle))
    }
}

/// One instance of each of the three pipes, made but not yet connected.
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

    /// Waits for the client to open all three, in the order Solo opens them.
    fn connect(self) -> Option<(File, File, File)> {
        let close = |handles: &[HANDLE]| {
            for handle in handles {
                unsafe { CloseHandle(*handle) };
            }
        };
        let Ok(out) = connect_pipe(self.out) else {
            close(&[self.input, self.control]);
            return None;
        };
        let Ok(input) = connect_pipe(self.input) else {
            close(&[self.control]);
            return None;
        };
        let control = connect_pipe(self.control).ok()?;
        Some((out, input, control))
    }
}

/// Output kept for the next client, and the client being written to.
struct Shared {
    ring: VecDeque<u8>,
    /// Whether output was dropped from the ring's start.
    truncated: bool,
    client: Option<File>,
}

impl Shared {
    /// What to replay: from the first line start, if the ring lost the start
    /// of the output, so the replay doesn't begin mid-sequence.
    fn replay(&self) -> Vec<u8> {
        let skip = if self.truncated {
            self.ring
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(0, |ix| ix + 1)
        } else {
            0
        };
        self.ring.iter().skip(skip).copied().collect()
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

fn serve(spec: Spec, console: Arc<Console>, first: Pipes) {
    let shared = Arc::new(Mutex::new(Shared {
        ring: VecDeque::new(),
        truncated: false,
        client: None,
    }));

    // Output, kept and passed on. Reading it never stops, or the program
    // would block on a full console.
    {
        let (console, shared) = (console.clone(), shared.clone());
        thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let read = match (&console.output).read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let bytes = &buffer[..read];
                let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
                shared.ring.extend(bytes);
                let excess = shared.ring.len().saturating_sub(RING_CAPACITY);
                if excess > 0 {
                    shared.ring.drain(..excess);
                    shared.truncated = true;
                }
                let failed = shared
                    .client
                    .as_mut()
                    .is_some_and(|client| client.write_all(bytes).is_err());
                if failed {
                    shared.client = None;
                }
            }
        });
    }

    // The host ends with the program.
    {
        let console = console.clone();
        thread::spawn(move || {
            unsafe {
                WaitForSingleObject(console.process, INFINITE);
            }
            // Its last output reaches the client first.
            thread::sleep(Duration::from_millis(300));
            unsafe {
                (console.api.close)(console.handle);
            }
            thread::sleep(Duration::from_millis(200));
            std::process::exit(0);
        });
    }

    let header = format!(
        "{HEADER} {PROTOCOL} {} {} {}\n",
        console.pid,
        spec.nonce.as_deref().unwrap_or("-"),
        spec.terminal_id.as_deref().unwrap_or("-"),
    );
    let mut pipes = Some(first);
    // Whether a client said to keep running: then the host waits for the
    // next client, even past one that opened the pipes and went.
    let mut kept = false;
    while let Some(next) = pipes.take().or_else(|| Pipes::make(&spec.pipe, false)) {
        let Some((mut out, input, control)) = next.connect() else {
            if kept {
                continue;
            }
            break;
        };
        // Whether to keep running when the client goes: a new client starts
        // from "end with me", and says otherwise.
        let mut keep = false;
        {
            let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
            let replay = shared.replay();
            if out.write_all(header.as_bytes()).is_ok()
                && out.write_all(&replay).is_ok()
                && out.write_all(REPLAY_END).is_ok()
            {
                shared.client = Some(out);
            }
        }
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
        loop {
            let mut kind = [0u8; 1];
            if control.read_exact(&mut kind).is_err() {
                break;
            }
            match kind[0] {
                b'R' => {
                    let mut size = [0u8; 4];
                    if control.read_exact(&mut size).is_err() {
                        break;
                    }
                    let columns = u16::from_le_bytes([size[0], size[1]]);
                    let rows = u16::from_le_bytes([size[2], size[3]]);
                    console.resize(columns, rows);
                }
                b'P' => keep = true,
                b'E' => keep = false,
                b'K' => {
                    console.kill();
                    thread::sleep(Duration::from_millis(200));
                    std::process::exit(0);
                }
                _ => break,
            }
        }
        // The client went: end with it, unless the program is busy; then
        // keep running for the next one, once its pipe instances are closed.
        if !keep {
            console.kill();
            thread::sleep(Duration::from_millis(200));
            std::process::exit(0);
        }
        kept = true;
        drop(control);
        shared.lock().unwrap_or_else(|e| e.into_inner()).client = None;
        forward_input.join().ok();
    }
    // The pipes can't be made (someone else holds the name), or a client
    // failed before saying to keep running: end rather than run on
    // unreachable.
    console.kill();
    std::process::exit(4);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(ring: &[u8], truncated: bool) -> Shared {
        Shared {
            ring: ring.iter().copied().collect(),
            truncated,
            client: None,
        }
    }

    #[test]
    fn test_replay_starts_at_a_line() {
        // All the output: replayed as it is.
        assert_eq!(
            shared(b"ab\x1b[1mcd\nef", false).replay(),
            b"ab\x1b[1mcd\nef"
        );
        // The start was dropped: from the first full line, not mid-sequence.
        assert_eq!(shared(b"[1mcd\nef\ngh", true).replay(), b"ef\ngh");
        // No line start: all of it, rather than nothing.
        assert_eq!(shared(b"abc", true).replay(), b"abc");
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
