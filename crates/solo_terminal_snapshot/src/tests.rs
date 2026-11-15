use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Config;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{CharsetIndex, Processor, StdSyncHandler};

use super::*;

const SPINNER: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];

fn new_term(columns: usize, lines: usize) -> Term<VoidListener> {
    let config = Config {
        scrolling_history: 10_000,
        kitty_keyboard: true,
        ..Config::default()
    };
    Term::new(config, &TermSize::new(columns, lines), VoidListener)
}

fn feed(term: &mut Term<VoidListener>, bytes: impl AsRef<[u8]>) {
    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(term, bytes.as_ref());
}

fn restore(term: &Term<VoidListener>, options: &SnapshotOptions) -> Term<VoidListener> {
    let snapshot = snapshot(term, options);
    assert_eq!(snapshot.columns, term.columns());
    assert_eq!(snapshot.screen_lines, term.screen_lines());
    let mut restored = new_term(snapshot.columns, snapshot.screen_lines);
    feed(&mut restored, &snapshot.bytes);
    restored
}

/// Everything about a cell that shows or that later output depends on.
#[derive(Debug, PartialEq)]
struct CellView {
    c: char,
    fg: Color,
    bg: Color,
    flags: Flags,
    zerowidth: Vec<char>,
    underline_color: Option<Color>,
    hyperlink: Option<(String, String)>,
}

/// With `exact_ids`, compares the ids alacritty makes up for links without
/// one; it makes up a new one for every link, so the same output written to
/// two terminals gets different ones.
fn view(cell: &Cell, exact_ids: bool) -> CellView {
    CellView {
        c: cell.c,
        fg: cell.fg,
        bg: cell.bg,
        flags: cell.flags,
        zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
        underline_color: cell.underline_color(),
        hyperlink: cell.hyperlink().map(|link| {
            let id = if !exact_ids && link.id().ends_with("_alacritty") {
                "made up"
            } else {
                link.id()
            };
            (id.to_string(), link.uri().to_string())
        }),
    }
}

fn line_text(grid: &Grid<Cell>, line: i32) -> String {
    let row = &grid[Line(line)];
    (0..grid.columns())
        .map(|column| row[Column(column)].c)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Asserts the last `history` lines of scrollback and the screen are alike.
fn assert_grids_eq(
    expected: &Grid<Cell>,
    actual: &Grid<Cell>,
    history: usize,
    exact_ids: bool,
    what: &str,
) {
    assert_eq!(expected.columns(), actual.columns(), "{what}: columns");
    assert_eq!(
        expected.screen_lines(),
        actual.screen_lines(),
        "{what}: lines"
    );
    assert_eq!(actual.history_size(), history, "{what}: history size");
    for line in -(history as i32)..expected.screen_lines() as i32 {
        for column in 0..expected.columns() {
            let point = (Line(line), Column(column));
            assert_eq!(
                view(&expected[point.0][point.1], exact_ids),
                view(&actual[point.0][point.1], exact_ids),
                "{what}: cell {line}:{column}\nexpected line: {:?}\nactual line:   {:?}",
                line_text(expected, line),
                line_text(actual, line),
            );
        }
    }
    let (expected_cursor, actual_cursor) = (&expected.cursor, &actual.cursor);
    assert_eq!(expected_cursor.point, actual_cursor.point, "{what}: cursor");
    assert_eq!(
        expected_cursor.input_needs_wrap, actual_cursor.input_needs_wrap,
        "{what}: pending wrap"
    );
    assert_eq!(
        view(&expected_cursor.template, exact_ids),
        view(&actual_cursor.template, exact_ids),
        "{what}: pen"
    );
    for index in [
        CharsetIndex::G0,
        CharsetIndex::G1,
        CharsetIndex::G2,
        CharsetIndex::G3,
    ] {
        assert_eq!(
            expected_cursor.charsets[index], actual_cursor.charsets[index],
            "{what}: charset {index:?}"
        );
    }
    let (expected_saved, actual_saved) = (&expected.saved_cursor, &actual.saved_cursor);
    assert_eq!(
        expected_saved.point, actual_saved.point,
        "{what}: saved cursor"
    );
    assert_eq!(
        view(&expected_saved.template, exact_ids),
        view(&actual_saved.template, exact_ids),
        "{what}: saved pen"
    );
}

/// Asserts `actual` (restored from a snapshot of `expected` with up to
/// `max_history` lines of scrollback) is the same terminal.
fn assert_terms_eq(
    expected: &Term<VoidListener>,
    actual: &Term<VoidListener>,
    max_history: usize,
    exact_ids: bool,
) {
    let history = expected.grid().history_size().min(max_history);
    assert_eq!(expected.mode(), actual.mode(), "modes");
    assert_eq!(
        expected.scroll_region(),
        actual.scroll_region(),
        "scroll region"
    );
    assert_eq!(expected.title(), actual.title(), "title");
    assert_eq!(
        expected.program_cursor_style(),
        actual.program_cursor_style(),
        "cursor style"
    );
    for index in 0..=NamedColor::Cursor as usize {
        assert_eq!(
            expected.colors()[index],
            actual.colors()[index],
            "color {index}"
        );
    }
    if expected.mode().contains(TermMode::ALT_SCREEN) {
        let primary_history = expected.inactive_grid().history_size().min(max_history);
        assert_grids_eq(
            expected.inactive_grid(),
            actual.inactive_grid(),
            primary_history,
            exact_ids,
            "primary screen",
        );
        assert_grids_eq(
            expected.grid(),
            actual.grid(),
            0,
            exact_ids,
            "alternate screen",
        );
    } else {
        assert_grids_eq(expected.grid(), actual.grid(), history, exact_ids, "screen");
    }
}

/// Restores `term` from a snapshot, compares, then feeds `more` to both and
/// compares again: later output must land where it did originally.
fn assert_round_trip(term: &mut Term<VoidListener>, more: &[&str]) {
    let options = SnapshotOptions::default();
    let mut restored = restore(term, &options);
    assert_terms_eq(term, &restored, options.max_history_lines, true);
    for bytes in more {
        feed(term, bytes);
        feed(&mut restored, bytes);
        assert_terms_eq(term, &restored, usize::MAX, false);
    }
}

/// Output shaped like Claude Code's while it works: finished messages above
/// a live region (spinner, status, input box) that it redraws in place by
/// moving the cursor up from where it left it and erasing lines.
fn claude_like_session(frames: usize) -> (String, Vec<String>) {
    let mut output = String::new();
    output.push_str("\x1b[38;2;215;119;87m╭───────────────────────────╮\x1b[39m\r\n");
    output.push_str("\x1b[38;2;215;119;87m│\x1b[39m \x1b[1m✻ Welcome to Claude Code!\x1b[22m \x1b[38;2;215;119;87m│\x1b[39m\r\n");
    output.push_str("\x1b[38;2;215;119;87m╰───────────────────────────╯\x1b[39m\r\n\r\n");
    for index in 0..120 {
        output.push_str(&format!(
            "\x1b[2m  ⎿ \x1b[22m读取 src/file_{index}.rs \x1b[90m({index} lines)\x1b[39m\r\n"
        ));
    }
    // The live region: three lines, the cursor left at the end of the last.
    output.push_str("\x1b[38;5;174m✻ Thinking…\x1b[39m\r\n\r\n\x1b[2m> \x1b[22m");
    let mut redraws = Vec::new();
    for frame in 0..frames {
        let mut redraw = String::new();
        // Up to the spinner line, from wherever the cursor is.
        redraw.push_str("\x1b[2A\r\x1b[2K");
        if frame % 7 == 3 {
            // A finished message goes above the region, which moves down.
            redraw.push_str(&format!(
                "\x1b[32m●\x1b[39m 完成第 {frame} 步：更新了 \x1b]8;;file:///src/main.rs\x1b\\main.rs\x1b]8;;\x1b\\\r\n\r\n"
            ));
            redraw.push_str("\x1b[2K");
        }
        redraw.push_str(&format!(
            "\x1b[38;5;174m{} Coalescing… \x1b[2m({}s · ↓ {} tokens · esc to interrupt)\x1b[22;39m\r\n",
            SPINNER[frame % SPINNER.len()],
            frame,
            frame * 37
        ));
        redraw.push_str("\x1b[2K\r\n\x1b[2K\x1b[2m> \x1b[22m");
        if frame % 5 == 0 {
            redraw.push_str("输入中文");
        }
        redraws.push(redraw);
    }
    (output, redraws)
}

#[test]
fn test_claude_like_output_with_relative_moves() {
    let mut term = new_term(80, 24);
    let (output, redraws) = claude_like_session(60);
    feed(&mut term, &output);
    let (first, later) = redraws.split_at(30);
    for redraw in first {
        feed(&mut term, redraw);
    }
    assert!(term.grid().history_size() > 100);
    let later: Vec<&str> = later.iter().map(String::as_str).collect();
    assert_round_trip(&mut term, &later);
}

/// What the snapshot is for: replaying only the end of the output (as the
/// terminal host does once its buffer dropped the start) puts the relative
/// moves in the wrong place, and the snapshot doesn't.
#[test]
fn test_tail_replay_breaks_where_a_snapshot_does_not() {
    let (output, redraws) = claude_like_session(40);
    let mut all = output;
    for redraw in &redraws {
        all.push_str(redraw);
    }
    let mut term = new_term(80, 24);
    feed(&mut term, &all);

    // The last 2000 bytes, from a line start.
    let tail_start = all.len() - 2000;
    let tail_start = tail_start + all[tail_start..].find('\n').unwrap() + 1;
    let mut replayed = new_term(80, 24);
    feed(&mut replayed, &all[tail_start..]);
    let screen = |term: &Term<VoidListener>| -> Vec<String> {
        (0..24).map(|line| line_text(term.grid(), line)).collect()
    };
    assert_ne!(screen(&term), screen(&replayed));

    let restored = restore(&term, &SnapshotOptions::default());
    assert_eq!(screen(&term), screen(&restored));
}

#[test]
fn test_wide_chars_and_cjk() {
    let mut term = new_term(10, 6);
    // 测 doesn't fit in the last column: a leading spacer there, then it wraps.
    feed(&mut term, "a中文字符测试\r\n");
    // Combining marks and an emoji.
    feed(&mut term, "e\u{301}x\u{20e3} 👍ok\r\n");
    // A wide char overwritten by a narrow one leaves a space behind.
    feed(&mut term, "汉字\x1b[3D小\r\n");
    // Ends with a wide char in the last two columns: the next one wraps.
    feed(&mut term, "abcdefgh宽");
    assert!(term.grid().cursor.input_needs_wrap);
    assert_round_trip(&mut term, &["宽字", "\x1b[A\r中间\x1b[B", "x"]);
}

#[test]
fn test_colors_and_attributes() {
    let mut term = new_term(40, 8);
    feed(
        &mut term,
        concat!(
            "\x1b[1mbold\x1b[22m \x1b[2mdim\x1b[22m \x1b[3mitalic\x1b[23m \x1b[7minv\x1b[27m ",
            "\x1b[8mhidden\x1b[28m \x1b[9mstrike\x1b[29m\r\n",
            "\x1b[4mu\x1b[4:2md\x1b[4:3mc\x1b[4:4md\x1b[4:5md\x1b[24m ",
            "\x1b[4;58;2;255;0;0mred-underline\x1b[59;24m \x1b[4;58;5;33mix\x1b[0m\r\n",
            "\x1b[31mred\x1b[92mbright\x1b[38;5;208m208\x1b[38;2;1;2;3mrgb\x1b[39m ",
            "\x1b[44mblue\x1b[103mbr\x1b[48;5;17mx\x1b[48;2;9;8;7my\x1b[49m\r\n",
            // Erasing with a background fills the line with it.
            "\x1b[45mfilled\x1b[K\x1b[0m\r\n",
            // Hyperlinks, with and without an id.
            "\x1b]8;id=a;https://example.com\x1b\\link\x1b]8;;\x1b\\ ",
            "\x1b]8;;https://solo.dev/x;y\x1b\\other\x1b]8;;\x1b\\\r\n",
            // Tabs are kept in the cells they start from.
            "a\tb\t\x1b[41mc\x1b[0m\r\n",
            // The pen left set for what comes next.
            "\x1b[1;3;38;5;99;48;2;10;20;30m",
        ),
    );
    assert_round_trip(&mut term, &["more", "\r\n\x1b[0mplain"]);
}

#[test]
fn test_alternate_screen() {
    let mut term = new_term(30, 10);
    for index in 0..40 {
        feed(&mut term, format!("shell line {index}\r\n"));
    }
    // A saved cursor and pen on the primary screen, then a full-screen program.
    feed(&mut term, "\x1b[3;5H\x1b[33m\x1b7\x1b[10;1H\x1b[32m$ vim");
    feed(
        &mut term,
        concat!(
            "\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1h\x1b=\x1b[?1004h\x1b[5 q",
            "\x1b[H\x1b[44m\x1b[2J\x1b[1;1Htitle bar\x1b[K\x1b[0m",
            "\x1b[2;9r\x1b[2;1H~\r\n~\r\n~ 中文 \x1b[7mrev\x1b[0m",
            "\x1b[10;1H\x1b[1m-- INSERT --\x1b[0m\x1b[5;3H\x1b[?25l",
        ),
    );
    assert_round_trip(
        &mut term,
        &[
            "\x1b[9;1H\n\n\x1b[5;3Hx",
            "\x1b[r\x1b[?25h\x1b[?1049l",
            "\x1b8back",
        ],
    );
}

#[test]
fn test_scroll_region_and_origin_mode() {
    let mut term = new_term(20, 12);
    feed(&mut term, "header\r\n");
    feed(&mut term, "\x1b[3;8r\x1b[?6h");
    for index in 0..15 {
        feed(&mut term, format!("\x1b[6;1Hrow {index}\n"));
    }
    feed(&mut term, "\x1b[12;1Hfooter\x1b[2;4H");
    assert_round_trip(
        &mut term,
        &["\n\n\n\n\nz", "\x1b[1;1Htop\x1b[?6l\x1b[1;1Hreal"],
    );
}

#[test]
fn test_pending_wrap_and_wrapped_lines() {
    let mut term = new_term(20, 6);
    // A line that wraps, then the wrapped part erased: still one line.
    feed(&mut term, "abcdefghijklmnopqrstU\x1b[2K\r\n");
    // A line that wraps twice.
    feed(&mut term, &"0123456789".repeat(5));
    feed(&mut term, "\r\n");
    // Exactly the width: the next character wraps.
    feed(&mut term, "ABCDEFGHIJKLMNOPQRST");
    assert!(term.grid().cursor.input_needs_wrap);
    assert_round_trip(&mut term, &["X", "\r\nlast"]);
}

#[test]
fn test_history_limit() {
    let mut term = new_term(20, 5);
    for index in 0..300 {
        feed(&mut term, format!("\x1b[3{}mline {index}\r\n", index % 8));
    }
    let options = SnapshotOptions {
        max_history_lines: 100,
        reset: true,
    };
    let restored = restore(&term, &options);
    assert_eq!(restored.grid().history_size(), 100);
    assert_terms_eq(&term, &restored, 100, true);
    // The cursor is on an empty last line, below lines 296 to 299.
    assert_eq!(line_text(restored.grid(), -100), "line 196");
}

#[test]
fn test_palette_title_charsets_and_modes() {
    let mut term = new_term(20, 4);
    feed(
        &mut term,
        concat!(
            "\x1b]4;1;rgb:12/34/56\x07\x1b]4;200;rgb:ff/00/ff\x07",
            "\x1b]10;rgb:ee/ee/ee\x07\x1b]11;rgb:11/11/11\x07\x1b]12;rgb:00/ff/00\x07",
            "\x1b]2;龙之谷 · Claude\x07",
            // Line drawing in G0 while drawing, ASCII after; G1 stays line drawing.
            "\x1b(0lqqk\x1b(B box\x1b)0\r\n",
            "\x1b[4h\x1b[20h\x1b[?7l\x1b[?1007l\x1b[?1042l\x1b[?1000h\x1b[>5u\x1b[3 q",
        ),
    );
    assert_round_trip(&mut term, &["ab\x1b[1;1Hx", "\x1b)B\x0elqk\x0f"]);
}

#[test]
fn test_reset_clears_what_the_terminal_had() {
    let mut term = new_term(20, 5);
    feed(&mut term, "\x1b[31mhello\r\nworld");
    let snapshot = snapshot(&term, &SnapshotOptions::default());
    let mut restored = new_term(20, 5);
    feed(
        &mut restored,
        "\x1b[?1049h\x1b[42mjunk everywhere\x1b[?2004h\x1b[3;4r\x1b]2;old\x07",
    );
    feed(&mut restored, &snapshot.bytes);
    assert_terms_eq(&term, &restored, usize::MAX, true);
}

#[test]
fn test_empty_terminal() {
    let mut term = new_term(10, 3);
    let snapshot = snapshot(
        &term,
        &SnapshotOptions {
            max_history_lines: 0,
            reset: false,
        },
    );
    // Nothing to write but the line breaks down to the last line and the
    // cursor.
    assert!(
        snapshot.bytes.len() < 40,
        "{:?}",
        String::from_utf8_lossy(&snapshot.bytes)
    );
    assert_round_trip(&mut term, &["first"]);
}
