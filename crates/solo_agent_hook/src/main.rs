//! SOLO: A Claude Code hook that tells the Solo terminal Claude runs in what
//! it's doing, with `OSC 6973 ; agent ; <kind> ; <ms since the epoch> ; <session id>`
//! (parsed by `terminal::shell_integration::AgentHook`).
//!
//! Claude Code starts it for every hook event Solo asks for, so it does as
//! little as possible: read the event from stdin, write one escape sequence to
//! the terminal, exit.

// No console of its own: it attaches to the terminal's below, and allocating
// one would only slow down every hook.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Hooks run in the background and may finish out of order; the terminal
    // orders them by this.
    let sent_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().to_string())
        .unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("report") {
        match report_sequence(&args[1..], &sent_at_ms) {
            Ok(sequence) => {
                if let Err(error) = write_to_terminal(sequence.as_bytes()) {
                    eprintln!("solo-agent-hook: failed to write to the terminal: {error}");
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("solo-agent-hook: {error}\n{REPORT_USAGE}");
                std::process::exit(2);
            }
        }
        return;
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("solo-agent-hook: failed to read the hook input: {error}");
    }
    let argument = std::env::args().nth(1);
    let Some(kind) = kind_of(&input, argument.as_deref()) else {
        return;
    };
    let session_id = session_id_of(&input).unwrap_or_default();
    let sequence = format!("\x1b]6973;agent;{kind};{sent_at_ms};{session_id}\x07");
    if let Err(error) = write_to_terminal(sequence.as_bytes()) {
        eprintln!("solo-agent-hook: failed to write to the terminal: {error}");
    }
}

const REPORT_USAGE: &str = "usage: solo-agent-hook report --state <working|blocked|idle> \
[--agent <name>] [--session <id>] [--seq <n>]";

/// The escape sequence for `solo-agent-hook report ...`: the public form of
/// the protocol (docs/agent-status-protocol.md), for agents and scripts.
fn report_sequence(args: &[String], sent_at_ms: &str) -> Result<String, String> {
    let mut state = None;
    let mut agent = String::new();
    let mut session = String::new();
    let mut order = sent_at_ms.to_string();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--state" => state = Some(value()?),
            "--agent" => agent = value()?,
            "--session" => session = value()?,
            "--seq" => order = value()?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let kind = match state.as_deref() {
        Some("working" | "prompt") => "prompt",
        Some("blocked" | "input") => "input",
        Some("idle" | "stop") => "stop",
        Some("start") => "start",
        Some(other) => return Err(format!("unknown state {other:?}")),
        None => return Err("--state is required".to_string()),
    };
    if order.parse::<u64>().is_err() {
        return Err(format!("--seq {order:?} isn't a number"));
    }
    // Everything goes into an escape sequence: nothing that could end it.
    if !session
        .chars()
        .all(|character| character.is_ascii_hexdigit() || character == '-')
    {
        return Err(format!("--session {session:?} isn't a UUID"));
    }
    if agent.chars().count() > 40
        || !agent.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ' ')
        })
    {
        return Err(format!(
            "--agent {agent:?} must be up to 40 letters, digits, spaces, '-', '_' or '.'"
        ));
    }
    Ok(format!(
        "\x1b]6973;agent;{kind};{order};{session};{agent}\x07"
    ))
}

/// The kind of report for a hook event, from the hook's JSON input, or else
/// from the argument the settings gave for it.
///
/// The kinds are those of `terminal::shell_integration::AgentHookKind`.
fn kind_of(input: &str, argument: Option<&str>) -> Option<&'static str> {
    let event = serde_json::from_str::<serde_json::Value>(input).ok();
    let field = |name: &str| {
        event
            .as_ref()
            .and_then(|event| event.get(name))
            .and_then(|value| value.as_str())
    };
    let Some(event_name) = field("hook_event_name") else {
        return match argument? {
            "start" => Some("start"),
            "prompt" => Some("prompt"),
            "input" => Some("input"),
            "stop" => Some("stop"),
            "idle" => Some("idle"),
            _ => None,
        };
    };
    match event_name {
        "SessionStart" => Some("start"),
        "UserPromptSubmit" => Some("prompt"),
        "PermissionRequest" => Some("input"),
        // Questions for the user and plan approval.
        "PreToolUse" => matches!(field("tool_name"), Some("AskUserQuestion" | "ExitPlanMode"))
            .then_some("input"),
        "Notification" => match field("notification_type")? {
            "permission_prompt" | "elicitation_dialog" | "agent_needs_input" => Some("input"),
            "idle_prompt" => Some("idle"),
            _ => None,
        },
        "Stop" | "StopFailure" => Some("stop"),
        _ => None,
    }
}

/// The session id from the hook's JSON input, if it looks like one: it goes
/// into an escape sequence, so nothing that could end it.
fn session_id_of(input: &str) -> Option<String> {
    let event = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let session_id = event.get("session_id")?.as_str()?;
    session_id
        .chars()
        .all(|character| character.is_ascii_hexdigit() || character == '-')
        .then(|| session_id.to_string())
}

/// Claude Code may start hooks without a console, or in a hidden one of their
/// own, so this attaches to the console of the shell Claude was started from
/// (`SOLO_AGENT_CONSOLE_PID`), or else Claude's own, and writes to that.
#[cfg(windows)]
fn write_to_terminal(bytes: &[u8]) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::io::Error;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FreeConsole() -> i32;
        fn AttachConsole(process_id: u32) -> i32;
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: *mut c_void,
        ) -> *mut c_void;
        fn WriteFile(
            file: *mut c_void,
            buffer: *const u8,
            bytes_to_write: u32,
            bytes_written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    let shell = std::env::var("SOLO_AGENT_CONSOLE_PID")
        .ok()
        .and_then(|pid| pid.trim().parse::<u32>().ok());
    // SAFETY: plain Win32 calls with valid arguments; the handle is closed
    // before returning.
    unsafe {
        FreeConsole();
        let attached = shell.is_some_and(|pid| AttachConsole(pid) != 0)
            || AttachConsole(ATTACH_PARENT_PROCESS) != 0;
        if !attached {
            return Err(Error::last_os_error());
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let console = CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if console.is_null() || console == INVALID_HANDLE_VALUE {
            return Err(Error::last_os_error());
        }
        let mut remaining = bytes;
        let mut result = Ok(());
        while !remaining.is_empty() {
            let mut written = 0u32;
            let length = u32::try_from(remaining.len()).unwrap_or(u32::MAX);
            if WriteFile(
                console,
                remaining.as_ptr(),
                length,
                &mut written,
                std::ptr::null_mut(),
            ) == 0
                || written == 0
            {
                result = Err(Error::last_os_error());
                break;
            }
            remaining = remaining.get(written as usize..).unwrap_or_default();
        }
        CloseHandle(console);
        result
    }
}

/// Writes to the terminal of the shell Claude was started from
/// (`SOLO_AGENT_TTY`), or else the controlling one.
#[cfg(not(windows))]
fn write_to_terminal(bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let path = std::env::var_os("SOLO_AGENT_TTY").unwrap_or_else(|| "/dev/tty".into());
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_from_the_command_line() {
        let args =
            |args: &[&str]| -> Vec<String> { args.iter().map(|arg| arg.to_string()).collect() };
        assert_eq!(
            report_sequence(
                &args(&["--state", "working", "--agent", "my agent", "--seq", "7"]),
                "123"
            ),
            Ok("\x1b]6973;agent;prompt;7;;my agent\x07".to_string())
        );
        assert_eq!(
            report_sequence(
                &args(&[
                    "--state",
                    "blocked",
                    "--session",
                    "0f6ed4b2-755b-4e18-8bd1-f60252548036"
                ]),
                "123"
            ),
            Ok("\x1b]6973;agent;input;123;0f6ed4b2-755b-4e18-8bd1-f60252548036;\x07".to_string())
        );
        assert_eq!(
            report_sequence(&args(&["--state", "idle"]), "5"),
            Ok("\x1b]6973;agent;stop;5;;\x07".to_string())
        );
        for bad in [
            &["--state", "dancing"][..],
            &[][..],
            &["--state"][..],
            &["--state", "idle", "--seq", "x"][..],
            &["--state", "idle", "--agent", "a\x07b"][..],
            &["--state", "idle", "--session", "x;y"][..],
            &["--state", "idle", "--what", "x"][..],
        ] {
            assert!(report_sequence(&args(bad), "1").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn session_ids_from_hook_input() {
        assert_eq!(
            session_id_of(r#"{"session_id":"0f6ed4b2-755b-4e18-8bd1-f60252548036"}"#).as_deref(),
            Some("0f6ed4b2-755b-4e18-8bd1-f60252548036")
        );
        assert_eq!(session_id_of("{\"session_id\":\"a\\u0007b\"}"), None);
        assert_eq!(session_id_of("not json"), None);
    }

    #[test]
    fn kinds_from_hook_input() {
        let kind = |input: &str| kind_of(input, None);
        assert_eq!(
            kind(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
            Some("start")
        );
        assert_eq!(
            kind(r#"{"session_id":"s","hook_event_name": "UserPromptSubmit","prompt":"hi"}"#),
            Some("prompt")
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#),
            Some("input")
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"}"#),
            Some("input")
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#),
            None
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#),
            Some("input")
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#),
            Some("idle")
        );
        assert_eq!(
            kind(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#),
            None
        );
        assert_eq!(kind(r#"{"hook_event_name":"Stop"}"#), Some("stop"));
        assert_eq!(kind(r#"{"hook_event_name":"StopFailure"}"#), Some("stop"));
        assert_eq!(kind(r#"{"hook_event_name":"PostToolUse"}"#), None);
    }

    #[test]
    fn the_argument_decides_without_input() {
        assert_eq!(kind_of("", Some("stop")), Some("stop"));
        assert_eq!(kind_of("not json", Some("prompt")), Some("prompt"));
        assert_eq!(kind_of("", Some("dance")), None);
        assert_eq!(kind_of("", None), None);
        // The input wins over the argument.
        assert_eq!(
            kind_of(r#"{"hook_event_name":"Stop"}"#, Some("prompt")),
            Some("stop")
        );
    }
}
