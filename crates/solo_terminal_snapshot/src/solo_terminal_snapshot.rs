//! SOLO: a terminal's screen as VT bytes: written to a fresh terminal of the
//! same size, they draw the same screen, with the same scrollback (up to a
//! limit), cursor, pen, and the modes that change what later output and
//! input mean (alternate screen, scroll region, bracketed paste, mouse
//! reporting, application cursor keys and so on).
//!
//! Replaying a program's raw output from the middle doesn't do that: a
//! program that redraws in place with relative cursor moves (Claude Code's
//! spinner, say) needs to know where it left the cursor, and that's lost
//! once the start of its output is. A snapshot of the parsed screen doesn't
//! depend on the output that drew it.
//!
//! Used by the phone client (the first screen of a terminal, before its
//! live output, `docs/design/mobile.md`) and meant for the terminal host,
//! which can send one to a Solo that connects again instead of its kept
//! output. It only depends on `alacritty_terminal`, so the host can
//! use it without gpui.
//!
//! Not restored: custom tab stops, the active charset when switched with
//! SI/SO (the designated G0–G3 charsets are), the title stack, and the kitty
//! keyboard mode stack beyond its top.

use alacritty_terminal::Term;
use alacritty_terminal::grid::{Dimensions as _, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::{
    CharsetIndex, Color, CursorShape, NamedColor, Rgb, StandardCharset,
};

#[cfg(test)]
mod tests;

/// What to put in a snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotOptions {
    /// At most this many lines of scrollback, the latest.
    pub max_history_lines: usize,
    /// Start with a full reset (RIS), for a terminal that may have content
    /// or modes of its own. Without it, the bytes expect a fresh terminal.
    pub reset: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            max_history_lines: 5000,
            reset: true,
        }
    }
}

/// A terminal's screen as VT bytes, for a terminal of this size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub columns: usize,
    pub screen_lines: usize,
    pub bytes: Vec<u8>,
}

/// Serializes `term`'s screen. Take it with the terminal locked, and send
/// the output parsed after it to keep the copy up to date.
pub fn snapshot<T>(term: &Term<T>, options: &SnapshotOptions) -> Snapshot {
    let mut writer = Writer::default();
    if options.reset {
        writer.out.extend_from_slice(b"\x1bc");
    }
    writer.palette(term);
    if let Some(title) = term.title() {
        writer.title(title);
    }

    let mode = *term.mode();
    let alternate = mode.contains(TermMode::ALT_SCREEN);
    let primary = if alternate {
        term.inactive_grid()
    } else {
        term.grid()
    };
    let history = primary.history_size().min(options.max_history_lines);
    writer.rows(primary, -(history as i32), primary.screen_lines() as i32);
    if alternate {
        // Entering the alternate screen keeps the primary screen's cursor
        // (where it goes back to) and saves it, as it did originally.
        writer.cursor(primary, 0);
        writer.csi("?1049h");
        // The alternate screen's cursor starts with the primary one's pen,
        // and is cleared with it: clear it with none.
        writer.csi("0m");
        writer.pen = Pen::default();
        writer.set_link(None);
        writer.csi("H");
        writer.csi("2J");
        let grid = term.grid();
        writer.rows(grid, 0, grid.screen_lines() as i32);
    }

    let grid = term.grid();
    writer.saved_cursor(grid);
    let region = term.scroll_region();
    if region.start != Line(0) || region.end != Line(grid.screen_lines() as i32) {
        writer.csi(&format!("{};{}r", region.start.0 + 1, region.end.0));
    }
    let origin = if mode.contains(TermMode::ORIGIN) {
        writer.csi("?6h");
        region.start.0
    } else {
        0
    };
    writer.cursor(grid, origin);
    writer.charsets(grid);
    writer.modes(mode);
    if let Some(style) = term.program_cursor_style() {
        let code = match style.shape {
            CursorShape::Block => Some(2),
            CursorShape::Underline => Some(4),
            CursorShape::Beam => Some(6),
            CursorShape::HollowBlock | CursorShape::Hidden => None,
        };
        if let Some(code) = code {
            writer.csi(&format!("{} q", code - u8::from(style.blinking)));
        }
    }

    Snapshot {
        columns: grid.columns(),
        screen_lines: grid.screen_lines(),
        bytes: writer.out,
    }
}

/// The graphic rendition cells are written with.
#[derive(Clone, PartialEq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
    underline_color: Option<Color>,
}

impl Default for Pen {
    fn default() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
            underline_color: None,
        }
    }
}

/// The flags SGR sets; the others are about the cell's place (wide chars,
/// wrapping).
const PEN_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

impl Pen {
    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & PEN_FLAGS,
            underline_color: cell.underline_color(),
        }
    }
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    /// The pen and hyperlink the terminal being written to has.
    pen: Pen,
    link: Option<Hyperlink>,
}

impl Writer {
    fn csi(&mut self, sequence: &str) {
        self.out.extend_from_slice(b"\x1b[");
        self.out.extend_from_slice(sequence.as_bytes());
    }

    fn osc(&mut self, sequence: &str) {
        self.out.extend_from_slice(b"\x1b]");
        self.out.extend_from_slice(sequence.as_bytes());
        self.out.push(0x07);
    }

    /// Colors the program changed (OSC 4, 10, 11, 12).
    fn palette<T>(&mut self, term: &Term<T>) {
        let colors = term.colors();
        let spec = |rgb: Rgb| format!("rgb:{:02x}/{:02x}/{:02x}", rgb.r, rgb.g, rgb.b);
        for index in 0..256 {
            if let Some(rgb) = colors[index] {
                self.osc(&format!("4;{index};{}", spec(rgb)));
            }
        }
        for (named, osc) in [
            (NamedColor::Foreground, 10),
            (NamedColor::Background, 11),
            (NamedColor::Cursor, 12),
        ] {
            if let Some(rgb) = colors[named] {
                self.osc(&format!("{osc};{}", spec(rgb)));
            }
        }
    }

    fn title(&mut self, title: &str) {
        let title: String = title.chars().filter(|c| !c.is_control()).collect();
        self.osc(&format!("2;{title}"));
    }

    fn set_pen(&mut self, pen: &Pen) {
        if *pen == self.pen {
            return;
        }
        let mut sgr = String::from("0");
        let flags = pen.flags;
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "4:2"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if flags.contains(flag) {
                sgr.push(';');
                sgr.push_str(code);
            }
        }
        color_sgr(&mut sgr, pen.fg, 30, 90, 38);
        color_sgr(&mut sgr, pen.bg, 40, 100, 48);
        match pen.underline_color {
            Some(Color::Spec(rgb)) => sgr.push_str(&format!(";58;2;{};{};{}", rgb.r, rgb.g, rgb.b)),
            Some(Color::Indexed(index)) => sgr.push_str(&format!(";58;5;{index}")),
            Some(Color::Named(named)) if (named as usize) < 16 => {
                sgr.push_str(&format!(";58;5;{}", named as usize))
            }
            Some(Color::Named(_)) | None => {}
        }
        self.csi(&format!("{sgr}m"));
        self.pen = pen.clone();
    }

    fn set_link(&mut self, link: Option<Hyperlink>) {
        if link == self.link {
            return;
        }
        match &link {
            Some(link) => {
                self.out.extend_from_slice(b"\x1b]8;id=");
                self.out.extend_from_slice(link.id().as_bytes());
                self.out.push(b';');
                self.out.extend_from_slice(link.uri().as_bytes());
                self.out.extend_from_slice(b"\x1b\\");
            }
            None => self.out.extend_from_slice(b"\x1b]8;;\x1b\\"),
        }
        self.link = link;
    }

    /// Writes a cell where the cursor is, as its program did.
    fn cell(&mut self, cell: &Cell) {
        self.set_pen(&Pen::of(cell));
        self.set_link(cell.hyperlink());
        let mut buffer = [0; 4];
        self.out
            .extend_from_slice(cell.c.encode_utf8(&mut buffer).as_bytes());
        for c in cell.zerowidth().into_iter().flatten() {
            self.out
                .extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
    }

    /// Writes the lines `start..end` of `grid` (negative ones are history)
    /// from the cursor down, scrolling what doesn't fit into the history.
    fn rows(&mut self, grid: &Grid<Cell>, start: i32, end: i32) {
        let columns = grid.columns();
        let last = Column(columns - 1);
        let mut follows_wrap = false;
        for line in start..end {
            let row = &grid[Line(line)];
            let wraps = row[last].flags.contains(Flags::WRAPLINE) && line + 1 < end;
            let mut length = if wraps {
                columns
            } else {
                (0..columns)
                    .rev()
                    .find(|&column| !is_blank(&row[Column(column)]))
                    .map_or(0, |column| column + 1)
            };
            // Something has to follow a wrap for the terminal to wrap.
            if follows_wrap {
                length = length.max(1);
            }

            // The column the next character lands in.
            let mut at = 0;
            for column in 0..length {
                if at > column {
                    continue;
                }
                let cell = &row[Column(column)];
                if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) && column + 1 == columns {
                    // Writing the wide char on the next line puts it here
                    // again, unless the wide char isn't there any more.
                    if wraps
                        && grid[Line(line + 1)][Column(0)]
                            .flags
                            .contains(Flags::WIDE_CHAR)
                    {
                        continue;
                    }
                    self.cell(&Cell {
                        flags: cell.flags - Flags::LEADING_WIDE_CHAR_SPACER,
                        ..cell.clone()
                    });
                } else if cell.c == '\t' {
                    // A tab marks the cell it starts from, to copy it as a
                    // tab: write a space, go back, tab, and come back.
                    self.cell(&Cell {
                        c: ' ',
                        ..cell.clone()
                    });
                    self.csi(&format!("{}G\t\x1b[{}G", column + 1, column + 2));
                } else if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    // A spacer the wide char before it didn't write.
                    self.cell(&Cell {
                        c: ' ',
                        flags: cell.flags - Flags::WIDE_CHAR_SPACER,
                        ..cell.clone()
                    });
                } else {
                    self.cell(cell);
                    if cell.flags.contains(Flags::WIDE_CHAR) {
                        at += 1;
                    }
                }
                at += 1;
            }

            if line + 1 < end && !wraps {
                // New lines take the pen's background.
                self.set_pen(&Pen::default());
                self.out.extend_from_slice(b"\r\n");
            }
            follows_wrap = wraps;
        }
    }

    /// Restores `grid`'s cursor: where it is (lines from `origin`, the
    /// scroll region's top in origin mode), whether the next character
    /// wraps first, and its pen.
    fn cursor(&mut self, grid: &Grid<Cell>, origin: i32) {
        let cursor = &grid.cursor;
        let line = cursor.point.line;
        let column = cursor.point.column;
        if cursor.input_needs_wrap {
            // Only writing the last column leaves the next character to
            // wrap: write it again.
            let row = &grid[line];
            let mut column = column;
            if row[column].flags.contains(Flags::WIDE_CHAR_SPACER) && column.0 > 0 {
                column = Column(column.0 - 1);
            }
            self.cup(line.0 - origin, column.0);
            let cell = &row[column];
            if cell.flags.contains(Flags::WIDE_CHAR) || !cell.flags.intersects(spacers()) {
                if cell.c == '\t' {
                    self.cell(&Cell {
                        c: ' ',
                        ..cell.clone()
                    });
                } else {
                    self.cell(cell);
                }
            } else {
                self.cell(&Cell {
                    c: ' ',
                    flags: cell.flags - spacers(),
                    ..cell.clone()
                });
            }
        } else {
            self.cup(line.0 - origin, column.0);
        }
        self.set_pen(&Pen::of(&cursor.template));
        self.set_link(cursor.template.hyperlink());
    }

    /// The cursor saved with DECSC, if one was.
    fn saved_cursor(&mut self, grid: &Grid<Cell>) {
        let saved = &grid.saved_cursor;
        if *saved == Default::default() {
            return;
        }
        self.cup(saved.point.line.0, saved.point.column.0);
        self.set_pen(&Pen::of(&saved.template));
        self.set_link(saved.template.hyperlink());
        self.out.extend_from_slice(b"\x1b7");
    }

    fn cup(&mut self, line: i32, column: usize) {
        self.csi(&format!("{};{}H", line + 1, column + 1));
    }

    /// The charsets designated as G0–G3 (after the content, which they
    /// would change).
    fn charsets(&mut self, grid: &Grid<Cell>) {
        let charsets = &grid.cursor.charsets;
        for (index, intermediate) in [
            (CharsetIndex::G0, b'('),
            (CharsetIndex::G1, b')'),
            (CharsetIndex::G2, b'*'),
            (CharsetIndex::G3, b'+'),
        ] {
            if charsets[index] == StandardCharset::SpecialCharacterAndLineDrawing {
                self.out.extend_from_slice(&[0x1b, intermediate, b'0']);
            }
        }
    }

    fn modes(&mut self, mode: TermMode) {
        let private = |set: bool, number: u32, writer: &mut Self| {
            writer.csi(&format!("?{number}{}", if set { 'h' } else { 'l' }));
        };
        if !mode.contains(TermMode::SHOW_CURSOR) {
            private(false, 25, self);
        }
        for (flag, number) in [
            (TermMode::APP_CURSOR, 1),
            (TermMode::MOUSE_REPORT_CLICK, 1000),
            (TermMode::MOUSE_DRAG, 1002),
            (TermMode::MOUSE_MOTION, 1003),
            (TermMode::FOCUS_IN_OUT, 1004),
            (TermMode::UTF8_MOUSE, 1005),
            (TermMode::SGR_MOUSE, 1006),
            (TermMode::BRACKETED_PASTE, 2004),
        ] {
            if mode.contains(flag) {
                private(true, number, self);
            }
        }
        for (flag, number) in [
            (TermMode::LINE_WRAP, 7),
            (TermMode::ALTERNATE_SCROLL, 1007),
            (TermMode::URGENCY_HINTS, 1042),
        ] {
            if !mode.contains(flag) {
                private(false, number, self);
            }
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            self.out.extend_from_slice(b"\x1b=");
        }
        if mode.contains(TermMode::INSERT) {
            self.csi("4h");
        }
        if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
            self.csi("20h");
        }
        let keyboard = (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits()
            >> TermMode::DISAMBIGUATE_ESC_CODES.bits().trailing_zeros();
        if keyboard != 0 {
            self.csi(&format!(">{keyboard}u"));
        }
    }
}

fn spacers() -> Flags {
    Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
}

/// Whether a cell is as a fresh terminal has it.
fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && Pen::of(cell) == Pen::default()
        && !cell
            .flags
            .intersects(PEN_FLAGS | Flags::WIDE_CHAR | spacers())
        && cell.zerowidth().is_none()
        && cell.hyperlink().is_none()
}

/// Appends a color's SGR parameters: `normal` + 0–7 for the 8 colors,
/// `bright` + 0–7 for their bright versions, `extended` for 256 colors and
/// RGB, and `normal` + 9 for the default.
fn color_sgr(sgr: &mut String, color: Color, normal: u8, bright: u8, extended: u8) {
    use std::fmt::Write as _;
    match color {
        Color::Named(named) => {
            let index = named as usize;
            if index < 8 {
                write!(sgr, ";{}", normal as usize + index).ok();
            } else if index < 16 {
                write!(sgr, ";{}", bright as usize + index - 8).ok();
            } else {
                // The default, and colors SGR can't set (dim ones are
                // drawn from the DIM flag).
                write!(sgr, ";{}", normal + 9).ok();
            }
        }
        Color::Indexed(index) => {
            write!(sgr, ";{extended};5;{index}").ok();
        }
        Color::Spec(rgb) => {
            write!(sgr, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b).ok();
        }
    }
}
