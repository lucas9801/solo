//! Shell integration: the shell marks its prompts and commands with OSC 133 and
//! reports its working directory with OSC 7, which lets the terminal split its
//! output into command blocks.
//!
//! The marks are forwarded by the vendored `alacritty_terminal` as
//! [`AlacShellMark`]s, each with the scrollback row it arrived on.

use std::path::{Path, PathBuf};
use std::time::Instant;

use alacritty_terminal::event::ShellMark as AlacShellMark;
use base64::Engine as _;
use util::shell::Shell;

const POWERSHELL_SCRIPT: &str = include_str!("shell_integration/powershell.ps1");
const BASH_SCRIPT: &str = include_str!("shell_integration/bash.sh");

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
    let stem = Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase());
    let args = match stem.as_deref() {
        Some("powershell" | "pwsh") => vec![
            "-NoLogo".to_string(),
            "-NoExit".to_string(),
            "-EncodedCommand".to_string(),
            powershell_encoded_command(POWERSHELL_SCRIPT),
        ],
        // The bash.exe in System32 runs WSL, which can't read the script's path.
        Some("bash") if !(is_windows && program.to_lowercase().contains("system32")) => {
            match write_bash_script() {
                Ok(path) => vec!["--rcfile".to_string(), path, "-i".to_string()],
                Err(error) => {
                    log::warn!("failed to write the bash shell integration script: {error}");
                    return shell;
                }
            }
        }
        _ => return shell,
    };
    Shell::WithArguments {
        program: program.to_string(),
        args,
        title_override: None,
    }
}

/// bash only loads an rc file from disk, so the script goes to the temp dir.
fn write_bash_script() -> std::io::Result<String> {
    let dir = std::env::temp_dir().join("solo-shell-integration");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("bash-integration.sh");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(BASH_SCRIPT) {
        std::fs::write(&path, BASH_SCRIPT)?;
    }
    // Forward slashes work both natively and in Git Bash on Windows.
    Ok(path.to_string_lossy().replace('\\', "/"))
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
    /// The command line, if the shell reported it (OSC 633 E).
    pub command: Option<String>,
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
            command: None,
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

    /// Moves every block's lines with `f`, after the grid's rows moved. Blocks
    /// whose prompt has no new line are dropped.
    pub(crate) fn remap_lines(&mut self, f: impl Fn(usize) -> Option<usize>) {
        self.blocks.retain_mut(|block| {
            let Some(prompt_line) = f(block.prompt_line) else {
                return false;
            };
            block.prompt_line = prompt_line;
            block.input_line = block.input_line.and_then(&f);
            block.output_line = block.output_line.and_then(&f);
            block.end_line = block.end_line.and_then(&f);
            true
        });
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
            AlacShellMark::CommandLine(command) => {
                if let Some(block) = self.blocks.last_mut() {
                    block.command = Some(command);
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
    fn integrates_supported_shells_without_user_args() {
        let shell = integrate_shell(Shell::System, "powershell.exe", true);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args.len() == 4));

        let shell = integrate_shell(Shell::Program("bash".into()), "bash", false);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args[0] == "--rcfile"));

        for unsupported in ["zsh", "wsl.exe", r"C:\Windows\System32\bash.exe"] {
            let shell = Shell::Program(unsupported.into());
            assert_eq!(integrate_shell(shell.clone(), unsupported, true), shell);
        }

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
