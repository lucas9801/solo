//! The terminal's screen as the host saw it: all its output, parsed as Solo
//! parses it, so a Solo that connects again gets the screen itself (see
//! `solo_terminal_snapshot`) rather than the tail of the output that drew
//! it. Replaying output from the middle draws garbage: what a full screen
//! program redraws in place with relative cursor moves, colors and modes
//! depend on output that's no longer there.
//!
//! Also kept: the shell's last working directory and properties (from its
//! integration's sequences), which Solo would otherwise only learn at the
//! shell's next prompt.

use std::sync::{Arc, Mutex, MutexGuard};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, ShellMark};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::term::Config;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use solo_terminal_snapshot::{SnapshotOptions, snapshot};

/// Lines of scrollback kept and sent along with the screen.
pub const HISTORY_LINES: usize = 3000;

/// Shell integration state the screen doesn't show.
#[derive(Default)]
struct Marks {
    /// The last `OSC 7` URI.
    working_directory: Option<String>,
    /// The last `OSC 633 ; P` of each key: key, value and nonce.
    properties: Vec<(String, String, Option<String>)>,
}

/// Keeps the marks the terminal reports.
pub struct Listener(Arc<Mutex<Marks>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let Event::ShellMark { mark, .. } = event else {
            // Queries were answered by the client connected then, or by no
            // one; titles and modes are in the terminal's state.
            return;
        };
        let mut marks = lock(&self.0);
        match mark {
            ShellMark::WorkingDirectory(uri) => marks.working_directory = Some(uri),
            ShellMark::Property { key, value, nonce } => {
                marks.properties.retain(|(kept, _, _)| *kept != key);
                marks.properties.push((key, value, nonce));
            }
            _ => {}
        }
    }
}

fn lock(marks: &Mutex<Marks>) -> MutexGuard<'_, Marks> {
    marks.lock().unwrap_or_else(|error| error.into_inner())
}

pub struct Screen {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    marks: Arc<Mutex<Marks>>,
}

impl Screen {
    pub fn new(columns: u16, rows: u16) -> Self {
        let config = Config {
            scrolling_history: HISTORY_LINES,
            ..Config::default()
        };
        let marks = Arc::new(Mutex::new(Marks::default()));
        Self {
            term: Term::new(config, &size(columns, rows), Listener(marks.clone())),
            parser: Processor::new(),
            marks,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// The console's size changed.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        if (self.columns(), self.rows()) != (columns.max(2), rows.max(1)) {
            self.term.resize(size(columns, rows));
        }
    }

    pub fn columns(&self) -> u16 {
        self.term.columns() as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.screen_lines() as u16
    }

    /// Bytes that draw this screen in a new terminal of its size: first the
    /// shell's working directory and properties (before the screen, which
    /// may be the alternate one, where Solo ignores them), then the screen
    /// with its scrollback, cursor and modes.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let marks = lock(&self.marks);
        if let Some(uri) = &marks.working_directory {
            bytes.extend_from_slice(b"\x1b]7;");
            bytes.extend(uri.bytes().filter(|byte| !byte.is_ascii_control()));
            bytes.push(0x07);
        }
        for (key, value, nonce) in &marks.properties {
            bytes.extend_from_slice(b"\x1b]633;P;");
            bytes.extend(escape(&format!("{key}={value}")).bytes());
            if let Some(nonce) = nonce {
                bytes.push(b';');
                bytes.extend(escape(nonce).bytes());
            }
            bytes.push(0x07);
        }
        // The client's terminal is new: no reset, which would also undo its
        // own settings (alternate scroll).
        let options = SnapshotOptions {
            max_history_lines: HISTORY_LINES,
            reset: false,
        };
        bytes.extend(snapshot(&self.term, &options).bytes);
        bytes
    }

    #[cfg(test)]
    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }
}

fn size(columns: u16, rows: u16) -> TermSize {
    TermSize::new(columns.max(2) as usize, rows.max(1) as usize)
}

/// Escapes an `OSC 633` value as shells do: backslashes doubled, `;` and
/// control characters as `\xNN`.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            ';' => escaped.push_str("\\x3b"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                escaped.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::TermMode;

    use super::*;

    /// What a client sees after the snapshot: a new terminal of the size the
    /// header says, fed the snapshot.
    fn restore(screen: &Screen) -> Screen {
        let mut restored = Screen::new(screen.columns(), screen.rows());
        restored.feed(&screen.snapshot());
        restored
    }

    fn lines(term: &Term<Listener>, history: usize) -> Vec<String> {
        let grid = term.grid();
        let history = history.min(grid.history_size()) as i32;
        (-history..grid.screen_lines() as i32)
            .map(|line| {
                let row = &grid[Line(line)];
                (0..grid.columns())
                    .map(|column| row[Column(column)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn assert_same_screen(expected: &Screen, actual: &Screen) {
        let (expected, actual) = (expected.term(), actual.term());
        assert_eq!(lines(expected, 100), lines(actual, 100));
        assert_eq!(expected.grid().cursor.point, actual.grid().cursor.point);
        assert_eq!(expected.mode(), actual.mode());
    }

    /// A program drawing in place, as Claude Code does: messages scroll up
    /// above a live region it redraws by moving up over it.
    fn agent_like_output(screen: &mut Screen, frames: usize) {
        for line in 0..40 {
            screen.feed(format!("\x1b[32m●\x1b[0m message {line}\r\n").as_bytes());
        }
        screen.feed(b"\x1b[?2004h\x1b[?25l");
        screen.feed(b"\r\n\r\n\r\n");
        for frame in 0..frames {
            screen.feed(
                format!(
                    "\x1b[3A\r\x1b[2K\x1b[33m{} Working… ({frame}s)\x1b[0m\r\n\x1b[2K╭──────╮\r\n\x1b[2K│ > {frame:<3}│",
                    ["·", "✢", "✳"][frame % 3]
                )
                .as_bytes(),
            );
        }
    }

    #[test]
    fn test_the_screen_is_restored() {
        let mut screen = Screen::new(80, 24);
        agent_like_output(&mut screen, 500);
        let restored = restore(&screen);
        assert_same_screen(&screen, &restored);
        assert!(
            lines(restored.term(), 0)
                .iter()
                .any(|line| line.contains("Working… (499s)"))
        );
        assert!(restored.term().mode().contains(TermMode::BRACKETED_PASTE));
    }

    #[test]
    fn test_later_output_lands_where_it_did() {
        let mut screen = Screen::new(80, 24);
        agent_like_output(&mut screen, 10);
        let mut restored = restore(&screen);
        // The program goes on redrawing relative to where it left the cursor.
        let more = b"\x1b[2A\r\x1b[2K\x1b[33m* Done\x1b[0m\r\n\r\n\x1b[2K| > ok |";
        screen.feed(more);
        restored.feed(more);
        assert_same_screen(&screen, &restored);
    }

    #[test]
    fn test_the_alternate_screen_is_restored() {
        let mut screen = Screen::new(40, 10);
        screen.feed(
            b"$ vim\r\n\x1b[?1049h\x1b[H\x1b[2J\x1b[1;7H~ file ~\x1b[10;1H-- INSERT --\x1b[3;5H",
        );
        let mut restored = restore(&screen);
        assert_same_screen(&screen, &restored);
        assert!(restored.term().mode().contains(TermMode::ALT_SCREEN));
        // Leaving it shows the primary screen again.
        screen.feed(b"\x1b[?1049l");
        restored.feed(b"\x1b[?1049l");
        assert_same_screen(&screen, &restored);
        assert_eq!(lines(restored.term(), 0)[0], "$ vim");
    }

    #[test]
    fn test_resizing() {
        let mut screen = Screen::new(80, 24);
        screen.feed(b"hello");
        screen.resize(100, 30);
        assert_eq!((screen.columns(), screen.rows()), (100, 30));
        let restored = restore(&screen);
        assert_eq!((restored.columns(), restored.rows()), (100, 30));
        assert_same_screen(&screen, &restored);
    }

    /// The shell's directory and properties reach the client, also when a
    /// full screen program runs on the alternate screen.
    #[test]
    fn test_shell_state_is_sent_first() {
        let mut screen = Screen::new(80, 24);
        screen.feed(b"\x1b]7;file://host/C:/old\x07\x1b]7;file://host/C:/work%20dir\x07");
        screen.feed(b"\x1b]633;P;Shell=C:\\\\bin\\\\pwsh.exe;nonce1\x07");
        screen.feed(b"\x1b]633;P;Odd=a\\x3bb\x07");
        screen.feed(b"\x1b[?1049hfull screen");
        let snapshot = screen.snapshot();

        let mut client = Screen::new(80, 24);
        client.feed(&snapshot);
        let marks = lock(&client.marks);
        assert_eq!(
            marks.working_directory.as_deref(),
            Some("file://host/C:/work%20dir")
        );
        assert_eq!(
            marks.properties,
            vec![
                (
                    "Shell".to_string(),
                    "C:\\bin\\pwsh.exe".to_string(),
                    Some("nonce1".to_string())
                ),
                ("Odd".to_string(), "a;b".to_string(), None),
            ]
        );
        drop(marks);
        assert!(client.term().mode().contains(TermMode::ALT_SCREEN));
    }
}
