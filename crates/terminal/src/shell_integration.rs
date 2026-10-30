//! Shell integration: the shell marks its prompts and commands with OSC 133 and
//! reports its working directory with OSC 7, which lets the terminal split its
//! output into command blocks.
//!
//! The marks are forwarded by the vendored `alacritty_terminal` as
//! [`AlacShellMark`]s, each with the scrollback row it arrived on.

use std::path::PathBuf;
use std::time::Instant;

use alacritty_terminal::event::ShellMark as AlacShellMark;
use base64::Engine as _;
use util::shell::{Shell, ShellKind};

const POWERSHELL_SCRIPT: &str = include_str!("shell_integration/powershell.ps1");

/// Upper bound on remembered blocks; older ones have scrolled out of history anyway.
const MAX_BLOCKS: usize = 10_000;

/// Adds the integration script to an interactive shell that supports it.
///
/// Shells with user-provided arguments are left alone, since the script has to
/// own the command line to load itself after the user's profile.
///
/// `program` is what `shell` resolves to, which for [`Shell::System`] is the
/// system's default shell.
pub fn integrate_shell(shell: Shell, program: &str, is_windows: bool) -> Shell {
    let program = match &shell {
        Shell::System => program,
        Shell::Program(program) => program.as_str(),
        Shell::WithArguments { .. } => return shell,
    };
    match ShellKind::new(program, is_windows) {
        ShellKind::PowerShell | ShellKind::Pwsh => Shell::WithArguments {
            program: program.to_string(),
            args: vec![
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-EncodedCommand".to_string(),
                powershell_encoded_command(POWERSHELL_SCRIPT),
            ],
            title_override: None,
        },
        _ => shell,
    }
}

/// `-EncodedCommand` takes base64 of the script's UTF-16LE bytes.
fn powershell_encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// One prompt and the command run from it.
///
/// Lines are scrollback rows counted from the top of the history, as reported
/// when each mark arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandBlock {
    /// Row where the prompt starts (OSC 133 A).
    pub prompt_line: usize,
    /// Row where command input starts, after the prompt (OSC 133 B).
    pub input_line: Option<usize>,
    /// Row where the command's output starts (OSC 133 C).
    pub output_line: Option<usize>,
    /// Row the cursor was on when the command finished (OSC 133 D).
    pub end_line: Option<usize>,
    pub exit_code: Option<i32>,
    /// Working directory the shell reported before this prompt.
    pub working_directory: Option<PathBuf>,
    pub started_at: Option<Instant>,
    pub finished_at: Option<Instant>,
}

impl CommandBlock {
    pub fn new(prompt_line: usize, working_directory: Option<PathBuf>) -> Self {
        Self {
            prompt_line,
            input_line: None,
            output_line: None,
            end_line: None,
            exit_code: None,
            working_directory,
            started_at: None,
            finished_at: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.started_at.is_some() && self.finished_at.is_none()
    }
}

/// Command blocks and working directory reported by the shell.
#[derive(Default, Debug)]
pub struct ShellIntegration {
    blocks: Vec<CommandBlock>,
    working_directory: Option<PathBuf>,
}

impl ShellIntegration {
    /// Whether the shell has sent any marks, i.e. integration is working.
    pub fn is_active(&self) -> bool {
        !self.blocks.is_empty()
    }

    pub fn blocks(&self) -> &[CommandBlock] {
        &self.blocks
    }

    pub fn working_directory(&self) -> Option<&PathBuf> {
        self.working_directory.as_ref()
    }

    pub(crate) fn handle_mark(&mut self, mark: AlacShellMark, line: usize) {
        match mark {
            AlacShellMark::PromptStart => {
                if self.blocks.len() == MAX_BLOCKS {
                    self.blocks.remove(0);
                }
                self.blocks
                    .push(CommandBlock::new(line, self.working_directory.clone()));
            }
            AlacShellMark::CommandStart => {
                if let Some(block) = self.blocks.last_mut() {
                    block.input_line = Some(line);
                }
            }
            AlacShellMark::CommandExecuted => {
                if let Some(block) = self.blocks.last_mut() {
                    block.output_line = Some(line);
                    block.started_at = Some(Instant::now());
                }
            }
            AlacShellMark::CommandFinished { exit_code } => {
                if let Some(block) = self.blocks.last_mut() {
                    block.end_line = Some(line);
                    block.exit_code = exit_code;
                    block.finished_at = Some(Instant::now());
                    if block.started_at.is_none() {
                        block.started_at = block.finished_at;
                    }
                }
            }
            AlacShellMark::WorkingDirectory(uri) => {
                if let Some(path) = path_from_file_uri(&uri) {
                    self.working_directory = Some(path);
                }
            }
        }
    }
}

/// Parses `file://host/path` as sent in OSC 7. The host is ignored; Windows
/// paths arrive as `/C:/dir`.
fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let path = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?
        .into_owned();
    let bytes = path.as_bytes();
    let is_drive_path = bytes.len() >= 3 && bytes[2] == b':' && bytes[1].is_ascii_alphabetic();
    if is_drive_path {
        Some(PathBuf::from(path[1..].replace('/', "\\")))
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_a_command_block() {
        let mut integration = ShellIntegration::default();
        integration.handle_mark(
            AlacShellMark::WorkingDirectory("file://host/C:/work%20dir".into()),
            0,
        );
        integration.handle_mark(AlacShellMark::PromptStart, 0);
        integration.handle_mark(AlacShellMark::CommandStart, 0);
        integration.handle_mark(AlacShellMark::CommandExecuted, 1);
        integration.handle_mark(AlacShellMark::CommandFinished { exit_code: Some(2) }, 4);
        integration.handle_mark(AlacShellMark::PromptStart, 5);

        let blocks = integration.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].prompt_line, 0);
        assert_eq!(blocks[0].input_line, Some(0));
        assert_eq!(blocks[0].output_line, Some(1));
        assert_eq!(blocks[0].end_line, Some(4));
        assert_eq!(blocks[0].exit_code, Some(2));
        assert!(!blocks[0].is_running());
        assert_eq!(
            blocks[0].working_directory,
            Some(PathBuf::from("C:\\work dir"))
        );
        assert_eq!(blocks[1].prompt_line, 5);
    }

    #[test]
    fn parses_unix_file_uri() {
        assert_eq!(
            path_from_file_uri("file://host/home/me"),
            Some(PathBuf::from("/home/me"))
        );
        assert_eq!(path_from_file_uri("http://host/x"), None);
    }

    #[test]
    fn integrates_only_powershell_without_user_args() {
        let shell = integrate_shell(Shell::System, "powershell.exe", true);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args.len() == 4));

        let shell = integrate_shell(Shell::Program("bash".into()), "bash", false);
        assert_eq!(shell, Shell::Program("bash".into()));

        let user_shell = Shell::WithArguments {
            program: "pwsh".into(),
            args: vec!["-NoProfile".into()],
            title_override: None,
        };
        assert_eq!(
            integrate_shell(user_shell.clone(), "pwsh", true),
            user_shell
        );
    }
}
