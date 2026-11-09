//! Many hosts starting at once, as when Solo restores a session with many
//! terminals: each must make its pipes and be reachable.
#![cfg(windows)]
#![allow(clippy::disallowed_methods, reason = "a test starting the host")]

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const HOSTS: usize = 16;
const ERROR_PIPE_BUSY: i32 = 231;

fn start(pipe: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_solo-terminal-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start the host");
    let spec = serde_json::json!({
        "protocol": 1,
        "pipe": pipe,
        "command_line": "cmd.exe /d /q /k",
        "working_directory": null,
        "env": [],
        "columns": 80,
        "rows": 24,
        "nonce": null,
        "terminal_id": null,
    });
    let mut stdin = child.stdin.take().expect("no stdin");
    stdin
        .write_all(spec.to_string().as_bytes())
        .expect("failed to write the spec");
    child
}

fn open(name: &str, read: bool, started: Instant) -> File {
    loop {
        match OpenOptions::new().read(read).write(!read).open(name) {
            Ok(file) => return file,
            Err(error)
                if (error.kind() == ErrorKind::NotFound
                    || error.raw_os_error() == Some(ERROR_PIPE_BUSY))
                    && started.elapsed() < Duration::from_secs(30) =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("failed to open {name}: {error}"),
        }
    }
}

fn header(output: &mut File) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while output.read(&mut byte).expect("failed to read the header") == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    String::from_utf8_lossy(&line).into_owned()
}

#[test]
fn many_hosts_start_at_once() {
    let started = Instant::now();
    let hosts: Vec<(String, Child)> = (0..HOSTS)
        .map(|ix| {
            let pipe = format!(r"\\.\pipe\solo-terminal-test-{}-{ix}", std::process::id());
            let child = start(&pipe);
            (pipe, child)
        })
        .collect();

    let clients: Vec<_> = hosts
        .iter()
        .map(|(pipe, _)| {
            let pipe = pipe.clone();
            thread::spawn(move || {
                let mut output = open(&format!("{pipe}-out"), true, started);
                let _input = open(&format!("{pipe}-in"), false, started);
                let mut control = open(&format!("{pipe}-ctl"), false, started);
                let header = header(&mut output);
                let elapsed = started.elapsed();
                // Done: end the program and the host.
                control.write_all(b"K").ok();
                (header, elapsed)
            })
        })
        .collect();

    for client in clients {
        let (header, elapsed) = client.join().expect("a client failed");
        assert!(
            header.starts_with("SOLO-TERMINAL-HOST 1 "),
            "unexpected header {header:?}"
        );
        eprintln!("connected after {elapsed:?}");
    }
    for (_, mut child) in hosts {
        let status = child.wait().expect("failed to wait for a host");
        assert!(status.success(), "a host ended with {status}");
    }
}
