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
use serde::{Deserialize, Serialize};
use util::shell::Shell;

const POWERSHELL_SCRIPT: &str = include_str!("shell_integration/powershell.ps1");
const BASH_SCRIPT: &str = include_str!("shell_integration/bash.sh");
const FISH_SCRIPT: &str = include_str!("shell_integration/fish.fish");
const ZSH_SCRIPT: &str = include_str!("shell_integration/zsh/integration.zsh");
const CLAUDE_HOOK_POWERSHELL: &str = include_str!("shell_integration/claude_hook.ps1");
const CLAUDE_HOOK_SH: &str = include_str!("shell_integration/claude_hook.sh");
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
        Some("powershell" | "pwsh") => {
            match powershell_script_command("powershell-integration", POWERSHELL_SCRIPT) {
                Ok(command) => vec![
                    "-NoLogo".to_string(),
                    "-NoExit".to_string(),
                    "-Command".to_string(),
                    command,
                ],
                Err(error) => {
                    log::warn!("failed to write the PowerShell shell integration script: {error}");
                    return shell;
                }
            }
        }
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
        // Written aside and renamed, so a shell starting meanwhile (in this
        // Solo or another) never reads half a script.
        let partial = path.with_extension(format!("{}.partial", std::process::id()));
        std::fs::write(&partial, script)?;
        if let Err(error) = std::fs::rename(&partial, &path) {
            std::fs::remove_file(&partial).ok();
            // Another Solo may have just written the same script.
            if std::fs::read_to_string(&path).ok().as_deref() != Some(script) {
                return Err(error);
            }
        }
    }
    // Forward slashes work both natively and in Git Bash on Windows.
    Ok(path.to_string_lossy().replace('\\', "/"))
}

/// A `-Command` for PowerShell that runs `script`, which is written to the
/// temp dir as `<name>-<hash>.ps1` (by its contents, so Solo versions running
/// side by side don't swap each other's scripts).
///
/// The script runs as a script block read from the file rather than with
/// `-File` or `. '<path>'`: those fail under the `Restricted` execution policy,
/// Windows PowerShell's default. It replaces `-EncodedCommand`, whose base64
/// command lines security software tends to stop.
pub fn powershell_script_command(name: &str, script: &str) -> std::io::Result<String> {
    // FNV-1a, which doesn't change between Rust versions like `DefaultHasher` may.
    let hash = script.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    let path = write_script(&format!("{name}-{:08x}.ps1", hash as u32), script)?;
    Ok(powershell_load_command(&path))
}

fn powershell_load_command(path: &str) -> String {
    format!(
        ". ([scriptblock]::Create([IO.File]::ReadAllText('{}', [Text.Encoding]::UTF8)))",
        path.replace('\'', "''")
    )
}

/// SOLO: Lets Claude Code started from the integrated shell report what it's
/// doing through hooks, which is more reliable than reading it from the output.
///
/// The hooks go in a settings file whose path the shell scripts get in
/// `SOLO_CLAUDE_SETTINGS`; they wrap `claude` to pass it with `--settings`,
/// which adds to the user's own settings and hooks without changing them.
/// Each hook writes an [`AgentHook`] to the terminal.
pub fn integrate_claude_code(is_windows: bool, env: &mut HashMap<String, String>) {
    match write_claude_code_settings(is_windows) {
        Ok(path) => {
            env.insert("SOLO_CLAUDE_SETTINGS".to_string(), path);
        }
        Err(error) => log::warn!("failed to write the Claude Code hook settings: {error}"),
    }
}

fn write_claude_code_settings(is_windows: bool) -> std::io::Result<String> {
    let hook = match agent_hook_executable() {
        Some(executable) => ClaudeHook::Executable(executable.to_string_lossy().into_owned()),
        None if is_windows => {
            ClaudeHook::PowerShell(write_script("claude-hook.ps1", CLAUDE_HOOK_POWERSHELL)?)
        }
        None => ClaudeHook::Sh(write_script("claude-hook.sh", CLAUDE_HOOK_SH)?),
    };
    let settings = claude_code_settings(&hook);
    let settings = serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?;
    write_script("claude-settings.json", &settings)
}

/// Solo's hook program (the `solo_agent_hook` crate), shipped next to Solo.
fn agent_hook_executable() -> Option<PathBuf> {
    let name = format!("solo-agent-hook{}", std::env::consts::EXE_SUFFIX);
    let executable = std::env::current_exe().ok()?.parent()?.join(name);
    executable.is_file().then_some(executable)
}

/// What Claude Code runs for a hook.
#[derive(Clone, Debug, PartialEq)]
enum ClaudeHook {
    /// Solo's hook program, which starts in a few milliseconds.
    Executable(String),
    /// The scripts, when the program is missing. PowerShell takes most of a
    /// second to start.
    PowerShell(String),
    Sh(String),
}

/// Claude Code settings with a hook for each event that changes what it's doing.
fn claude_code_settings(hook: &ClaudeHook) -> serde_json::Value {
    let command = |kind: AgentHookKind| {
        // Exec form, so there's no shell in between. In the background, so
        // Claude Code doesn't wait for them.
        let (program, args) = match hook {
            // It reads the event from its input; the kind is a fallback.
            ClaudeHook::Executable(path) => (path.as_str(), vec![kind.as_str()]),
            ClaudeHook::PowerShell(script) => (
                "powershell.exe",
                vec![
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    script.as_str(),
                    kind.as_str(),
                ],
            ),
            ClaudeHook::Sh(script) => ("sh", vec![script.as_str(), kind.as_str()]),
        };
        serde_json::json!({
            "type": "command",
            "command": program,
            "args": args,
            "async": true,
        })
    };
    let group = |matcher: Option<&str>, kind: AgentHookKind| {
        let mut group = serde_json::json!({ "hooks": [command(kind)] });
        if let Some(matcher) = matcher {
            group["matcher"] = matcher.into();
        }
        group
    };
    serde_json::json!({
        "hooks": {
            "SessionStart": [group(None, AgentHookKind::SessionStarted)],
            "UserPromptSubmit": [group(None, AgentHookKind::PromptSubmitted)],
            // Right when Claude asks; its `permission_prompt` notification
            // comes only after a while.
            "PermissionRequest": [group(None, AgentHookKind::NeedsInput)],
            // Questions for the user and plan approval.
            "PreToolUse": [group(Some("AskUserQuestion|ExitPlanMode"), AgentHookKind::NeedsInput)],
            "Notification": [
                group(
                    Some("permission_prompt|elicitation_dialog|agent_needs_input"),
                    AgentHookKind::NeedsInput,
                ),
                group(Some("idle_prompt"), AgentHookKind::Idle),
            ],
            "Stop": [group(None, AgentHookKind::Stopped)],
            "StopFailure": [group(None, AgentHookKind::Stopped)],
        }
    })
}

/// SOLO: What an AI agent's hook reported it's doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentHookKind {
    /// A session started or resumed; the agent waits for a prompt.
    SessionStarted,
    /// The user submitted a prompt: a turn starts.
    PromptSubmitted,
    /// The agent asks for permission or an answer.
    NeedsInput,
    /// The agent finished its turn.
    Stopped,
    /// The agent finished a while ago and is still waiting for a prompt.
    Idle,
}

impl AgentHookKind {
    const ALL: [AgentHookKind; 5] = [
        AgentHookKind::SessionStarted,
        AgentHookKind::PromptSubmitted,
        AgentHookKind::NeedsInput,
        AgentHookKind::Stopped,
        AgentHookKind::Idle,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentHookKind::SessionStarted => "start",
            AgentHookKind::PromptSubmitted => "prompt",
            AgentHookKind::NeedsInput => "input",
            AgentHookKind::Stopped => "stop",
            AgentHookKind::Idle => "idle",
        }
    }
}

/// SOLO: A report from an AI agent's hook, sent as
/// `OSC 6973 ; agent ; <kind> [; <ms since the epoch> [; <session id>]]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentHook {
    pub kind: AgentHookKind,
    /// When the hook started. Hooks run in the background and can arrive out
    /// of order.
    pub sent_at_ms: Option<u64>,
    /// The agent's session (Claude Code's `session_id`), which tells which
    /// transcript is this terminal's.
    pub session_id: Option<AgentSessionId>,
}

impl AgentHook {
    /// Parses the parameters after `agent`.
    pub fn parse(params: &[String]) -> Option<Self> {
        let (kind, rest) = params.split_first()?;
        let kind = AgentHookKind::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == kind.trim())?;
        let sent_at_ms = rest.first().and_then(|sent_at| sent_at.trim().parse().ok());
        let session_id = rest.get(1).and_then(|id| AgentSessionId::parse(id));
        Some(Self {
            kind,
            sent_at_ms,
            session_id,
        })
    }
}

/// SOLO: An agent session id, a UUID such as
/// `0f6ed4b2-755b-4e18-8bd1-f60252548036`. Kept as a number so hooks stay `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AgentSessionId(pub u128);

impl AgentSessionId {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let groups: Vec<&str> = text.split('-').collect();
        let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
        if lengths != [8, 4, 4, 4, 12] {
            return None;
        }
        u128::from_str_radix(&groups.concat(), 16).ok().map(Self)
    }
}

impl std::fmt::Display for AgentSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = format!("{:032x}", self.0);
        write!(
            f,
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
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
                    .map(|(started_at, finished_at)| (finished_at - started_at).as_millis() as u64),
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
            integration.handle_mark(AlacShellMark::CommandFinished { exit_code: Some(0) }, end);
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
        assert!(matches!(
            shell,
            Shell::WithArguments { ref args, .. }
                if args[..3] == ["-NoLogo", "-NoExit", "-Command"]
                    && args[3].contains("solo-shell-integration/powershell-integration-")
                    && !args.iter().any(|arg| arg.contains("Encoded"))
        ));

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

    /// The load command runs the script whatever the execution policy, from a
    /// path with quotes and non-ASCII characters in it.
    #[cfg(windows)]
    #[test]
    #[allow(clippy::disallowed_methods, reason = "test")]
    fn powershell_loads_scripts_under_a_restricted_policy() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("it's 终端");
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("script.ps1");
        std::fs::write(
            &script,
            "$global:solo = '已加载'\nWrite-Output \"loaded $global:solo\"\n",
        )
        .unwrap();
        let path = script.to_string_lossy().replace('\\', "/");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive"])
            .args(["-ExecutionPolicy", "Restricted", "-Command"])
            .arg(format!(
                "{}; if ($global:solo -eq '已加载') {{ 'script-loaded' }}",
                powershell_load_command(&path)
            ))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("script-loaded"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_agent_hooks() {
        let params = |params: &[&str]| -> Vec<String> {
            params.iter().map(|param| param.to_string()).collect()
        };
        assert_eq!(
            AgentHook::parse(&params(&["prompt", "1793624689993"])),
            Some(AgentHook {
                kind: AgentHookKind::PromptSubmitted,
                sent_at_ms: Some(1793624689993),
                session_id: None,
            })
        );
        // `date` without %N leaves the time out.
        assert_eq!(
            AgentHook::parse(&params(&["stop", ""])),
            Some(AgentHook {
                kind: AgentHookKind::Stopped,
                sent_at_ms: None,
                session_id: None,
            })
        );
        // The session id, added at the end.
        let session_id = "0f6ed4b2-755b-4e18-8bd1-f60252548036";
        let hook = AgentHook::parse(&params(&["start", "1793624689993", session_id])).unwrap();
        assert_eq!(hook.session_id.unwrap().to_string(), session_id);
        assert_eq!(
            AgentHook::parse(&params(&["start", "1", "not-a-uuid"]))
                .and_then(|hook| hook.session_id),
            None
        );
        assert_eq!(
            AgentSessionId::parse("00000000-0000-0000-0000-00000000000a")
                .unwrap()
                .to_string(),
            "00000000-0000-0000-0000-00000000000a"
        );
        assert_eq!(
            AgentHook::parse(&params(&["input"])).map(|hook| hook.kind),
            Some(AgentHookKind::NeedsInput)
        );
        assert_eq!(AgentHook::parse(&params(&["dance", "1"])), None);
        assert_eq!(AgentHook::parse(&[]), None);
        for kind in AgentHookKind::ALL {
            assert_eq!(
                AgentHook::parse(&params(&[kind.as_str()])).map(|hook| hook.kind),
                Some(kind)
            );
        }
    }

    #[test]
    fn claude_code_settings_run_the_hook_for_each_event() {
        let settings =
            claude_code_settings(&ClaudeHook::PowerShell("C:/temp/claude-hook.ps1".into()));
        let hooks = &settings["hooks"];
        let kinds = |event: &str| -> Vec<(Option<String>, String)> {
            hooks[event]
                .as_array()
                .into_iter()
                .flatten()
                .map(|group| {
                    let hook = &group["hooks"][0];
                    assert_eq!(hook["command"], "powershell.exe");
                    assert_eq!(hook["async"], true);
                    let args: Vec<&str> = hook["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|arg| arg.as_str())
                        .collect();
                    assert_eq!(args[args.len() - 2], "C:/temp/claude-hook.ps1");
                    (
                        group["matcher"].as_str().map(str::to_string),
                        args[args.len() - 1].to_string(),
                    )
                })
                .collect()
        };
        assert_eq!(kinds("SessionStart"), vec![(None, "start".to_string())]);
        assert_eq!(
            kinds("UserPromptSubmit"),
            vec![(None, "prompt".to_string())]
        );
        assert_eq!(
            kinds("PermissionRequest"),
            vec![(None, "input".to_string())]
        );
        assert_eq!(kinds("Stop"), vec![(None, "stop".to_string())]);
        assert_eq!(
            kinds("Notification"),
            vec![
                (
                    Some("permission_prompt|elicitation_dialog|agent_needs_input".to_string()),
                    "input".to_string()
                ),
                (Some("idle_prompt".to_string()), "idle".to_string()),
            ]
        );

        let settings = claude_code_settings(&ClaudeHook::Executable(
            r"C:\Solo\solo-agent-hook.exe".into(),
        ));
        let hook = &settings["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(hook["command"], r"C:\Solo\solo-agent-hook.exe");
        assert_eq!(hook["args"], serde_json::json!(["stop"]));

        let settings = claude_code_settings(&ClaudeHook::Sh("/tmp/claude-hook.sh".into()));
        let hook = &settings["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(hook["command"], "sh");
        assert_eq!(
            hook["args"],
            serde_json::json!(["/tmp/claude-hook.sh", "stop"])
        );

        let mut env = HashMap::default();
        integrate_claude_code(cfg!(windows), &mut env);
        let written = std::fs::read_to_string(&env["SOLO_CLAUDE_SETTINGS"]).unwrap();
        let written: serde_json::Value = serde_json::from_str(&written).unwrap();
        let args = written["hooks"]["Stop"][0]["hooks"][0]["args"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The test binary has no hook program next to it: a script runs.
        let hook_path = args[args.len() - 2].as_str().unwrap_or_default();
        assert!(Path::new(hook_path).is_file());
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
