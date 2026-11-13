//! A host outlives its client only when last told `P`, and logs what it was
//! told and why it ended.
#![cfg(windows)]
#![allow(clippy::disallowed_methods, reason = "a test starting the host")]

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const ERROR_PIPE_BUSY: i32 = 231;

fn start(pipe: &str, command_line: &str, log_dir: &Path) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_solo-terminal-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start the host");
    let spec = serde_json::json!({
        "protocol": 1,
        "pipe": pipe,
        "command_line": command_line,
        "working_directory": null,
        "env": [],
        "columns": 80,
        "rows": 24,
        "nonce": null,
        "terminal_id": null,
        "log_dir": log_dir,
    });
    let mut stdin = child.stdin.take().expect("no stdin");
    stdin
        .write_all(spec.to_string().as_bytes())
        .expect("failed to write the spec");
    child
}

fn open(name: &str, read: bool, wait: Duration) -> std::io::Result<File> {
    let started = Instant::now();
    loop {
        match OpenOptions::new().read(read).write(!read).open(name) {
            Ok(file) => return Ok(file),
            Err(error)
                if (error.kind() == ErrorKind::NotFound
                    || error.raw_os_error() == Some(ERROR_PIPE_BUSY))
                    && started.elapsed() < wait =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

struct Client {
    output: File,
    _input: File,
    control: File,
    child_pid: u32,
}

fn connect(pipe: &str, wait: Duration) -> std::io::Result<Client> {
    let mut output = open(&format!("{pipe}-out"), true, wait)?;
    let input = open(&format!("{pipe}-in"), false, Duration::from_secs(30))?;
    let control = open(&format!("{pipe}-ctl"), false, Duration::from_secs(30))?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while output.read(&mut byte)? == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    let header = String::from_utf8_lossy(&line).into_owned();
    let child_pid = header
        .split(' ')
        .nth(2)
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("unexpected header {header:?}"));
    Ok(Client {
        output,
        _input: input,
        control,
        child_pid,
    })
}

/// Waits for the host to end, ten seconds at most.
fn ended(host: &mut Child) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if host.try_wait().expect("failed to wait").is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn log_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "solo-terminal-host-test-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn log_text(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect()
}

#[test]
fn a_busy_host_waits_for_the_next_client() {
    let dir = log_dir("busy");
    let pipe = format!(r"\\.\pipe\solo-terminal-test-busy-{}", std::process::id());
    let mut host = start(&pipe, "cmd.exe /d /q /c ping -n 60 127.0.0.1", &dir);
    let mut client = connect(&pipe, Duration::from_secs(30)).expect("first client");
    let child_pid = client.child_pid;
    // Busy, then idle, then busy again: the last one counts.
    client.control.write_all(b"PEP").unwrap();
    client.control.write_all(b"R").unwrap();
    client.control.write_all(&[100, 0, 30, 0]).expect("resize");
    // Solo goes without a word (quit, crashed).
    drop(client);
    thread::sleep(Duration::from_millis(500));
    assert!(host.try_wait().unwrap().is_none(), "the host ended");

    // The next Solo connects to the same program.
    let mut next = connect(&pipe, Duration::from_secs(10)).expect("next client");
    assert_eq!(next.child_pid, child_pid);
    let mut replayed = [0u8; 1];
    next.output.read_exact(&mut replayed).unwrap();
    next.control.write_all(b"K").unwrap();
    assert!(ended(&mut host), "K didn't end the host");

    let log = log_text(&dir);
    for expected in [
        "started cmd.exe /d /q /c ping -n 60 127.0.0.1",
        "client 1 connected",
        "got P",
        "got E",
        "got R 100x30",
        "client 1 gone: its control pipe closed",
        "keep running: true",
        "client 2 connected",
        "got K",
        "ending: Solo said to end (K)",
    ] {
        assert!(log.contains(expected), "no {expected:?} in the log:\n{log}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_idle_host_ends_with_its_client() {
    let dir = log_dir("idle");
    let pipe = format!(r"\\.\pipe\solo-terminal-test-idle-{}", std::process::id());
    let mut host = start(&pipe, "cmd.exe /d /q /k", &dir);
    let mut client = connect(&pipe, Duration::from_secs(30)).expect("first client");
    client.control.write_all(b"PE").unwrap();
    drop(client);
    assert!(ended(&mut host), "the idle host kept running");
    assert!(connect(&pipe, Duration::ZERO).is_err());

    let log = log_text(&dir);
    assert!(log.contains("keep running: false"), "{log}");
    assert!(
        log.contains("ending: the client went while the terminal wasn't to keep running"),
        "{log}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
