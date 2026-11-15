//! A client connecting to a host that kept running gets the screen as it is,
//! even when what runs redraws in place (as agents do) and the output that
//! drew it is long gone; and the console repaints it right after.
//!
//! The program is a fake full screen program: this test binary run with
//! `--fake-tui` (no test harness, so nothing else gets printed).

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(ix) = args.iter().position(|arg| arg == "--fake-tui") {
        tests::fake_tui(std::path::Path::new(&args[ix + 1]));
        return;
    }
    tests::run();
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
#[allow(clippy::disallowed_methods, reason = "a test starting the host")]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::io::{ErrorKind, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{Receiver, channel};
    use std::thread;
    use std::time::{Duration, Instant};

    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    const COLUMNS: usize = 80;
    const ROWS: usize = 24;
    const MESSAGES: usize = 60;
    const FRAMES: usize = 600;
    const REPLAY_END: &[u8] = b"\x1b]6974;replay-end\x07";
    const ERROR_PIPE_BUSY: i32 = 231;

    fn live_region(frame: usize) -> [String; 4] {
        let spinner = ["|", "/", "-", "\\"][frame % 4];
        [
            format!("\x1b[38;5;208m{spinner}\x1b[0m Working... ({frame} frames, esc to interrupt)"),
            format!("\x1b[2m+{}+\x1b[0m", "-".repeat(40)),
            format!(
                "\x1b[2m|\x1b[0m > typed {frame:>5}{}\x1b[2m|\x1b[0m",
                " ".repeat(26)
            ),
            format!("\x1b[2m+{}+\x1b[0m", "=".repeat(40)),
        ]
    }

    fn message(line: usize) -> String {
        format!("\x1b[32m*\x1b[0m message {line:>3}: \x1b[1msome\x1b[0m output")
    }

    /// Draws like an agent: messages, then a live region at the bottom it
    /// redraws in place by moving up over it, frame after frame; then says
    /// it's done in `done` and waits to be ended.
    pub fn fake_tui(done: &Path) {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
            SetConsoleMode,
        };
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0;
            GetConsoleMode(handle, &mut mode);
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        let mut out = std::io::stdout();
        for line in 0..MESSAGES {
            write!(out, "{}\r\n", message(line)).ok();
        }
        write!(out, "\x1b[?25l{}", live_region(0).join("\r\n")).ok();
        out.flush().ok();
        for frame in 1..FRAMES {
            let lines: Vec<String> = live_region(frame)
                .iter()
                .map(|line| format!("\x1b[2K{line}"))
                .collect();
            write!(out, "\x1b[3A\r{}", lines.join("\r\n")).ok();
            out.flush().ok();
            thread::sleep(Duration::from_millis(3));
        }
        std::fs::write(done, b"done").ok();
        thread::sleep(Duration::from_secs(120));
    }

    /// The screen the fake program ends with.
    fn expected_screen() -> Vec<String> {
        let mut lines: Vec<String> = (0..MESSAGES).map(message).collect();
        lines.extend(live_region(FRAMES - 1));
        let lines: Vec<String> = lines.iter().map(|line| plain(line)).collect();
        lines[lines.len() - ROWS..].to_vec()
    }

    /// Text without its escape sequences.
    fn plain(text: &str) -> String {
        let mut term = new_term(200, 1);
        feed(&mut term, text.as_bytes());
        rows(&term)[0].clone()
    }

    fn new_term(columns: usize, rows: usize) -> Term<VoidListener> {
        Term::new(
            Config::default(),
            &TermSize::new(columns, rows),
            VoidListener,
        )
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        Processor::<StdSyncHandler>::new().advance(term, bytes);
    }

    fn rows(term: &Term<VoidListener>) -> Vec<String> {
        let grid = term.grid();
        (0..grid.screen_lines())
            .map(|line| {
                let row = &grid[Line(line as i32)];
                let mut text = String::new();
                for column in 0..grid.columns() {
                    let cell = &row[Column(column)];
                    if !cell
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                    {
                        text.push(cell.c);
                    }
                }
                text.trim_end().to_string()
            })
            .collect()
    }

    fn wrong_lines(actual: &[String], expected: &[String]) -> usize {
        actual
            .iter()
            .zip(expected)
            .filter(|(actual, expected)| actual != expected)
            .count()
            + actual.len().abs_diff(expected.len())
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "solo-terminal-host-test-{name}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn start(host_exe: &Path, pipe: &str, command_line: &str, dir: &Path) -> Child {
        let mut child = Command::new(host_exe)
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
            "columns": COLUMNS,
            "rows": ROWS,
            "nonce": null,
            "terminal_id": null,
            "log_dir": dir,
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(spec.to_string().as_bytes())
            .unwrap();
        child
    }

    fn open(name: &str, read: bool) -> File {
        let started = Instant::now();
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

    struct Client {
        header: String,
        output: Receiver<Vec<u8>>,
        _input: File,
        control: File,
    }

    fn connect(pipe: &str) -> Client {
        let mut output = open(&format!("{pipe}-out"), true);
        let input = open(&format!("{pipe}-in"), false);
        let control = open(&format!("{pipe}-ctl"), false);
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while output.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        let (sender, receiver) = channel();
        // Read all along, as Solo does, so the host never waits on it.
        thread::spawn(move || {
            let mut buffer = vec![0u8; 64 * 1024];
            while let Ok(read) = output.read(&mut buffer) {
                if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        Client {
            header: String::from_utf8_lossy(&line).into_owned(),
            output: receiver,
            _input: input,
            control,
        }
    }

    /// What arrives within `wait`, or up to `until` if it does sooner.
    fn read(client: &Client, wait: Duration, until: Option<&[u8]>) -> Vec<u8> {
        let started = Instant::now();
        let mut bytes = Vec::new();
        while let Some(left) = wait.checked_sub(started.elapsed()) {
            match client.output.recv_timeout(left) {
                Ok(chunk) => bytes.extend(chunk),
                Err(_) => break,
            }
            if let Some(until) = until
                && bytes.windows(until.len()).any(|window| window == until)
            {
                break;
            }
        }
        bytes
    }

    fn split_at_replay_end(bytes: &[u8]) -> (&[u8], &[u8]) {
        let at = bytes
            .windows(REPLAY_END.len())
            .position(|window| window == REPLAY_END)
            .expect("no end of the screen");
        (&bytes[..at], &bytes[at + REPLAY_END.len()..])
    }

    fn wait_for(path: &Path, timeout: Duration) {
        let started = Instant::now();
        while !path.exists() {
            assert!(started.elapsed() < timeout, "{} never came", path.display());
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Ends the host if a test fails before it does.
    struct Host(Child);

    impl Drop for Host {
        fn drop(&mut self) {
            self.0.kill().ok();
        }
    }

    /// Runs the fake program in `host`, connects, goes while it draws, and
    /// connects again once it's done: the screen sent must be the program's.
    /// With Windows' own console, it also repaints the screen right after
    /// (Solo's bundled one doesn't); that must be the program's screen too.
    fn check_reconnecting(name: &str, host_exe: Option<&Path>) {
        let dir = temp_dir(name);
        let done = dir.join("done");
        let pipe = format!(r"\\.\pipe\solo-terminal-test-{name}-{}", std::process::id());
        let exe = std::env::current_exe().unwrap();
        let command_line = format!("\"{}\" --fake-tui \"{}\"", exe.display(), done.display());
        let host_exe = match host_exe {
            Some(host_exe) => {
                // Alone in a folder: no bundled console next to it.
                let copy = dir.join("solo-terminal-host.exe");
                std::fs::copy(host_exe, &copy).unwrap();
                copy
            }
            None => PathBuf::from(env!("CARGO_BIN_EXE_solo-terminal-host")),
        };
        let mut host = Host(start(&host_exe, &pipe, &command_line, &dir));

        // Solo watches the program start, says to keep it, and goes while it
        // still draws.
        let mut first = connect(&pipe);
        first.control.write_all(b"P").unwrap();
        read(&first, Duration::from_millis(800), None);
        drop(first);

        wait_for(&done, Duration::from_secs(60));
        // The console's last frame reaches the host.
        thread::sleep(Duration::from_millis(500));

        let mut next = connect(&pipe);
        let fields: Vec<&str> = next.header.split(' ').collect();
        assert_eq!(
            fields.get(5..7),
            Some(&["80", "24"][..]),
            "header {:?}",
            next.header
        );
        let bytes = read(&next, Duration::from_secs(10), Some(REPLAY_END));
        let (screen, after) = split_at_replay_end(&bytes);
        let expected = expected_screen();

        // The screen the host sent, in a new terminal of its size.
        let mut term = new_term(COLUMNS, ROWS);
        feed(&mut term, screen);
        let restored = rows(&term);
        assert_eq!(
            wrong_lines(&restored, &expected),
            0,
            "restored:\n{}\nexpected:\n{}",
            restored.join("\n"),
            expected.join("\n")
        );

        let mut repaint = after.to_vec();
        repaint.extend(read(&next, Duration::from_millis(1500), None));
        if bundled_console(&host_exe) {
            eprintln!(
                "{name}: the bundled console sent {} bytes after the screen",
                repaint.len()
            );
        } else {
            // On its own, in a blank terminal, the repaint draws the whole
            // screen.
            assert!(!repaint.is_empty(), "the console didn't repaint");
            let mut blank = new_term(COLUMNS, ROWS);
            feed(&mut blank, &repaint);
            let repainted = rows(&blank);
            assert_eq!(
                wrong_lines(&repainted, &expected),
                0,
                "repainted:\n{}\nexpected:\n{}",
                repainted.join("\n"),
                expected.join("\n")
            );
        }
        // After the screen sent, whatever the console sends changes nothing.
        feed(&mut term, &repaint);
        assert_eq!(rows(&term), restored);

        next.control.write_all(b"K").unwrap();
        let started = Instant::now();
        while host.0.try_wait().unwrap().is_none() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "K didn't end the host"
            );
            thread::sleep(Duration::from_millis(50));
        }
        drop(host);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn bundled_console(host_exe: &Path) -> bool {
        host_exe
            .parent()
            .is_some_and(|dir| dir.join("conpty.dll").is_file())
    }

    fn with_the_console_next_to_the_host() {
        check_reconnecting("bundled", None);
    }

    fn with_windows_console() {
        check_reconnecting(
            "windows",
            Some(Path::new(env!("CARGO_BIN_EXE_solo-terminal-host"))),
        );
    }

    pub fn run() {
        let tests: [(&str, fn()); 2] = [
            (
                "the_screen_is_restored_with_the_console_next_to_the_host",
                with_the_console_next_to_the_host,
            ),
            (
                "the_screen_is_restored_and_repainted_with_windows_console",
                with_windows_console,
            ),
        ];
        let mut failed = 0;
        for (name, test) in tests {
            let result = std::panic::catch_unwind(test);
            println!(
                "test {name} ... {}",
                if result.is_ok() { "ok" } else { "FAILED" }
            );
            failed += usize::from(result.is_err());
        }
        if failed > 0 {
            std::process::exit(1);
        }
    }
}
