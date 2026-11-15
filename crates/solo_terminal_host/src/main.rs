//! SOLO: holds one terminal's pseudo console for Solo, so the shell and what
//! runs in it (an agent, a build) outlive Solo quitting or restarting for an
//! update. Solo starts it detached, writes what to run to its stdin (see
//! [`Spec`]), and talks to it over three named pipes only the same user can
//! open:
//!
//! - `<pipe>-out`: a header line ([`HEADER`]), the screen as it is (drawn
//!   for a new terminal of the size the header says, with up to
//!   `screen::HISTORY_LINES` lines of scrollback), [`REPLAY_END`], then live
//!   output;
//! - `<pipe>-in`: input for the terminal, as typed;
//! - `<pipe>-ctl`: `R` with columns and rows (two little-endian `u16`s)
//!   resizes; `P` says to keep running when the client goes (the program is
//!   busy), `E` to end then (it's idle, the default); `K` ends the program
//!   and the host now.
//!
//! One client at a time. When it goes (Solo quit, closed the window, or
//! crashed), the host ends unless it was last told `P`; then the next client
//! can connect, starting from `P` (so connecting and going without a word
//! doesn't end it). Each client gets the screen; then Windows' own console
//! repaints it too (resized to its size; the bundled one doesn't). The host
//! ends with the program it runs, and ends it if no first client connects
//! within a minute (Solo crashed or was ended after starting the host). What
//! it's told and why it ends goes to its log in Solo's logs folder; while it
//! runs, it's listed in Solo's data folder (see [`Running`]).

#[cfg(windows)]
mod host;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod running;
#[cfg(windows)]
mod screen;

/// Bumped when the pipes or the spec change; Solo only talks to hosts of its
/// own protocol.
pub const PROTOCOL: u32 = 1;

/// The first line on `<pipe>-out`: `SOLO-TERMINAL-HOST <protocol> <child pid>
/// <nonce or -> <terminal id or -> <columns> <rows>`, the last two the size
/// of the screen that follows (hosts before Solo 2.3 don't send them, and
/// replay their kept output instead).
pub const HEADER: &str = "SOLO-TERMINAL-HOST";

/// After the output kept for a client: queries in it (for the terminal's
/// type, the cursor's position) were answered when they were made, and Solo
/// mustn't answer them again, or the answers reach the program as typed.
pub const REPLAY_END: &[u8] = b"\x1b]6974;replay-end\x07";

/// How much output is kept for a client that connects later.
pub const RING_CAPACITY: usize = 2 * 1024 * 1024;

/// What Solo asks the host to run, as JSON on its stdin.
#[derive(serde::Deserialize)]
pub struct Spec {
    pub protocol: u32,
    /// The pipes' name without the `-out`/`-in`/`-ctl` suffix.
    pub pipe: String,
    /// The command line, quoted for `CreateProcessW`.
    pub command_line: String,
    pub working_directory: Option<String>,
    /// Added to the host's own environment (which it got from Solo).
    pub env: Vec<(String, String)>,
    pub columns: u16,
    pub rows: u16,
    /// Reported back in the header, for Solo to trust the shell's sequences
    /// and know the terminal's id after reconnecting.
    pub nonce: Option<String>,
    pub terminal_id: Option<String>,
    /// Solo's logs folder, for the host's log (see `log.rs`); none from
    /// Solo before 2.3.
    #[serde(default)]
    pub log_dir: Option<String>,
    /// Where to list the host while it runs (see [`Running`]); none from
    /// Solo before 2.3.
    #[serde(default)]
    pub running_dir: Option<String>,
}

/// A running host, as listed in `<running_dir>/<host pid>.json`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Running {
    pub protocol: u32,
    pub host_pid: u32,
    /// When the host process was created (100 ns since 1601), which tells it
    /// from a later process given the same pid.
    pub host_created: u64,
    pub child_pid: u32,
    pub pipe: String,
    pub terminal_id: Option<String>,
    pub command_line: String,
    pub working_directory: Option<String>,
    /// Seconds since 1970.
    pub started_at: u64,
    /// Clients so far, and whether one is connected now.
    pub clients: u32,
    pub connected: bool,
}

fn main() {
    #[cfg(windows)]
    host::main();
    #[cfg(not(windows))]
    eprintln!("solo-terminal-host only runs on Windows");
}
