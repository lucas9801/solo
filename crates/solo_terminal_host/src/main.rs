//! SOLO: holds one terminal's pseudo console for Solo, so the shell and what
//! runs in it (an agent, a build) outlive Solo quitting or restarting for an
//! update. Solo starts it detached, writes what to run to its stdin (see
//! [`Spec`]), and talks to it over three named pipes only the same user can
//! open:
//!
//! - `<pipe>-out`: a header line ([`HEADER`]), the output kept while no one
//!   was connected (the last [`RING_CAPACITY`] bytes), [`REPLAY_END`], then
//!   live output;
//! - `<pipe>-in`: input for the terminal, as typed;
//! - `<pipe>-ctl`: `R` with columns and rows (two little-endian `u16`s)
//!   resizes; `P` says to keep running when the client goes (the program is
//!   busy), `E` to end then (it's idle, the default); `K` ends the program
//!   and the host now.
//!
//! One client at a time. When it goes (Solo quit, closed the window, or
//! crashed), the host ends unless it was last told `P`; then the next client
//! can connect, and resizing makes the console repaint the whole screen for
//! it. The host ends with the program it runs.

#[cfg(windows)]
mod host;

/// Bumped when the pipes or the spec change; Solo only talks to hosts of its
/// own protocol.
pub const PROTOCOL: u32 = 1;

/// The first line on `<pipe>-out`: `SOLO-TERMINAL-HOST <protocol> <child pid>
/// <nonce or -> <terminal id or ->`.
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
}

fn main() {
    #[cfg(windows)]
    host::main();
    #[cfg(not(windows))]
    eprintln!("solo-terminal-host only runs on Windows");
}
