//! Shell integration: the shell marks its prompts and commands with OSC 133 and
//! reports its working directory with OSC 7, which lets the terminal split its
//! output into command blocks. Scripts are provided for PowerShell, bash, zsh
//! and fish.
//!
//! The marks are forwarded by the vendored `alacritty_terminal` as
//! [`AlacShellMark`]s, each with the scrollback row it arrived on.

use collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alacritty_terminal::event::ShellMark as AlacShellMark;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use util::shell::Shell;

const POWERSHELL_SCRIPT: &str = include_str!("shell_integration/powershell.ps1");
const BASH_SCRIPT: &str = include_str!("shell_integration/bash.sh");
const FISH_SCRIPT: &str = include_str!("shell_integration/fish.fish");
const ZSH_SCRIPT: &str = include_str!("shell_integration/zsh/integration.zsh");
/// Startup files for the `ZDOTDIR` zsh is started with, each sourcing the
/// user's own.
const ZSH_STARTUP_FILES: [(&str, &str); 3] = [
    (".zshenv", include_str!("shell_integration/zsh/zshenv.zsh")),
    (
        ".zprofile",
        include_str!("shell_integration/zsh/zprofile.zsh"),
    ),
    (".zshrc", include_str!("shell_integration/zsh/zshrc.zsh")),
];

/// Upper bound on remembered blocks; older ones have scrolled out of history anyway.
const MAX_BLOCKS: usize = 10_000;

/// Adds the integration script to an interactive shell that supports it.
///
/// Shells with user-provided arguments are left alone, since the script has to
/// own the command line to load itself after the user's profile.
///
/// `program` is what `shell` resolves to, which for [`Shell::System`] is the
/// system's default shell. zsh is integrated through its environment, `env`.
pub fn integrate_shell(
    shell: Shell,
    program: &str,
    is_windows: bool,
    env: &mut HashMap<String, String>,
) -> Shell {
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
            match write_script("bash-integration.sh", BASH_SCRIPT) {
                Ok(path) => vec!["--rcfile".to_string(), path, "-i".to_string()],
                Err(error) => {
                    log::warn!("failed to write the bash shell integration script: {error}");
                    return shell;
                }
            }
        }
        Some("fish") => match write_script("fish-integration.fish", FISH_SCRIPT) {
            Ok(path) => vec![
                "--init-command".to_string(),
                format!("source '{}'", path.replace('\'', "\\'")),
            ],
            Err(error) => {
                log::warn!("failed to write the fish shell integration script: {error}");
                return shell;
            }
        },
        // zsh has no option for an extra rc file; it's started with a ZDOTDIR
        // whose startup files load the user's and then the integration.
        Some("zsh") => {
            if let Err(error) = integrate_zsh(env) {
                log::warn!("failed to write the zsh shell integration scripts: {error}");
            }
            return shell;
        }
        _ => return shell,
    };
    Shell::WithArguments {
        program: program.to_string(),
        args,
        title_override: None,
    }
}

fn integrate_zsh(env: &mut HashMap<String, String>) -> std::io::Result<()> {
    let integration = write_script("zsh/integration.zsh", ZSH_SCRIPT)?;
    for (name, script) in ZSH_STARTUP_FILES {
        write_script(&format!("zsh/{name}"), script)?;
    }
    let user_zdotdir = env
        .get("ZDOTDIR")
        .cloned()
        .or_else(|| std::env::var("ZDOTDIR").ok());
    if let Some(user_zdotdir) = user_zdotdir {
        env.insert("SOLO_USER_ZDOTDIR".to_string(), user_zdotdir);
    }
    let zdotdir = Path::new(&integration).parent().unwrap_or(Path::new("."));
    env.insert(
        "ZDOTDIR".to_string(),
        zdotdir.to_string_lossy().into_owned(),
    );
    env.insert("SOLO_ZSH_INTEGRATION".to_string(), integration);
    Ok(())
}

/// Shells only load extra scripts from disk, so they go to the temp dir.
fn write_script(name: &str, script: &str) -> std::io::Result<String> {
    let path = std::env::temp_dir()
        .join("solo-shell-integration")
        .join(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(script) {
        std::fs::write(&path, script)?;
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

/// The terminal's contents and command blocks, saved to show them again after
/// a restart.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Rows of the scrollback, with SGR escapes for their colors.
    pub rows: Vec<String>,
    pub blocks: Vec<SavedCommandBlock>,
}

/// A [`CommandBlock`] in a [`SessionSnapshot`]; lines count from its first row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedCommandBlock {
    pub prompt_line: usize,
    pub input_line: Option<usize>,
    pub output_line: Option<usize>,
    pub end_line: Option<usize>,
    pub exit_code: Option<i32>,
    pub command: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub duration_ms: Option<u64>,
}

/// Command blocks and working directory reported by the shell.
#[derive(Default, Debug)]
pub struct ShellIntegration {
    /// The shell program, as the integration script reported it (OSC 633 P
    /// `Shell`): a path for PowerShell, a name otherwise.
    shell: Option<String>,
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

    pub fn shell(&self) -> Option<&str> {
        self.shell.as_deref()
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

    /// The blocks whose prompt is on one of `rows`, to be saved with them.
    pub(crate) fn saved_blocks(&self, rows: std::ops::Range<usize>) -> Vec<SavedCommandBlock> {
        let relative = |line: usize| line.checked_sub(rows.start);
        self.blocks
            .iter()
            .filter(|block| rows.contains(&block.prompt_line))
            .map(|block| SavedCommandBlock {
                prompt_line: block.prompt_line - rows.start,
                input_line: block.input_line.and_then(relative),
                output_line: block.output_line.and_then(relative),
                end_line: block.end_line.and_then(relative),
                exit_code: block.exit_code,
                command: block.command.clone(),
                working_directory: block.working_directory.clone(),
                duration_ms: block
                    .started_at
                    .zip(block.finished_at)
                    .map(|(started_at, finished_at)| {
                        (finished_at - started_at).as_millis() as u64
                    }),
            })
            .collect()
    }

    /// Adds saved blocks in front of the current ones, with `line` giving
    /// where each saved line is now. Blocks that were still running when
    /// saved end at `last_line`.
    pub(crate) fn restore_blocks(
        &mut self,
        saved: &[SavedCommandBlock],
        line: impl Fn(usize) -> usize,
        last_line: usize,
    ) {
        let now = Instant::now();
        let restored = saved.iter().map(|block| {
            // A command still running when saved has no duration to show.
            let started_at = block
                .duration_ms
                .map(|ms| now.checked_sub(Duration::from_millis(ms)).unwrap_or(now));
            CommandBlock {
                prompt_line: line(block.prompt_line),
                input_line: block.input_line.map(&line),
                output_line: block.output_line.map(&line),
                end_line: Some(block.end_line.map_or(last_line, &line)),
                exit_code: block.exit_code,
                command: block.command.clone(),
                working_directory: block.working_directory.clone(),
                started_at,
                finished_at: Some(now),
            }
        });
        let mut blocks: Vec<CommandBlock> = restored.collect();
        blocks.append(&mut self.blocks);
        let excess = blocks.len().saturating_sub(MAX_BLOCKS);
        blocks.drain(..excess);
        self.blocks = blocks;
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
            AlacShellMark::Property { key, value } => {
                if key == "Shell" {
                    self.shell = Some(value);
                }
            }
            AlacShellMark::HistoryCleared => {
                self.remap_lines(|block_line| block_line.checked_sub(line));
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
    fn saves_and_restores_blocks() {
        let mut integration = ShellIntegration::default();
        for (prompt, output, end) in [(0, 1, 3), (4, 5, 6)] {
            integration.handle_mark(AlacShellMark::PromptStart, prompt);
            integration.handle_mark(AlacShellMark::CommandLine("ls".into()), prompt);
            integration.handle_mark(AlacShellMark::CommandExecuted, output);
            integration
                .handle_mark(AlacShellMark::CommandFinished { exit_code: Some(0) }, end);
        }
        integration.handle_mark(AlacShellMark::PromptStart, 7);

        let saved = integration.saved_blocks(4..7);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].prompt_line, 0);
        assert_eq!(saved[0].output_line, Some(1));
        assert_eq!(saved[0].end_line, Some(2));
        assert_eq!(saved[0].command.as_deref(), Some("ls"));

        let mut restored = ShellIntegration::default();
        restored.handle_mark(AlacShellMark::PromptStart, 20);
        restored.restore_blocks(&saved, |line| line + 10, 15);
        let blocks = restored.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].prompt_line, 10);
        assert_eq!(blocks[0].end_line, Some(12));
        assert!(!blocks[0].is_running());
        assert_eq!(blocks[1].prompt_line, 20);
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
        let env = &mut HashMap::default();
        let shell = integrate_shell(Shell::System, "powershell.exe", true, env);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args.len() == 4));

        let shell = integrate_shell(Shell::Program("bash".into()), "bash", false, env);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args[0] == "--rcfile"));

        let shell = integrate_shell(Shell::Program("/usr/bin/fish".into()), "fish", false, env);
        assert!(
            matches!(shell, Shell::WithArguments { ref args, .. } if args[0] == "--init-command")
        );
        assert!(env.is_empty());

        for unsupported in ["wsl.exe", r"C:\Windows\System32\bash.exe"] {
            let shell = Shell::Program(unsupported.into());
            assert_eq!(
                integrate_shell(shell.clone(), unsupported, true, env),
                shell
            );
        }

        let user_shell = Shell::WithArguments {
            program: "pwsh".into(),
            args: vec!["-NoProfile".into()],
            title_override: None,
        };
        assert_eq!(
            integrate_shell(user_shell.clone(), "pwsh", true, env),
            user_shell
        );
    }

    #[test]
    fn integrates_zsh_through_zdotdir() {
        let mut env =
            HashMap::from_iter([("ZDOTDIR".to_string(), "/home/me/.config/zsh".to_string())]);
        let shell = Shell::Program("zsh".into());
        assert_eq!(
            integrate_shell(shell.clone(), "zsh", false, &mut env),
            shell
        );
        assert_eq!(env["SOLO_USER_ZDOTDIR"], "/home/me/.config/zsh");
        let zdotdir = PathBuf::from(&env["ZDOTDIR"]);
        assert!(zdotdir.join(".zshrc").is_file());
        assert!(PathBuf::from(&env["SOLO_ZSH_INTEGRATION"]).is_file());
    }
}
