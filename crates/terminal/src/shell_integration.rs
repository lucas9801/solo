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

/// The environment variable that gives an integrated shell its terminal's
/// nonce, which it adds to `OSC 633` sequences. Anything else the terminal
/// shows (`cat`ed files, web pages, remote output) can print those sequences
/// too, but can't know the nonce.
pub const SHELL_NONCE_ENV: &str = "SOLO_SHELL_NONCE";

/// The environment variable with the terminal's id, which `solo-cli agent`
/// names terminals by.
pub const TERMINAL_ID_ENV: &str = "SOLO_TERMINAL_ID";
/// The environment variable with the path of `solo-cli`, for agents to drive
/// Solo with (`solo-cli agent ...`).
pub const CLI_ENV: &str = "SOLO_CLI";

/// SOLO: Gives a local terminal its id and the path of `solo-cli`.
pub fn add_terminal_env(env: &mut HashMap<String, String>) {
    env.insert(TERMINAL_ID_ENV.to_string(), new_nonce()[..8].to_string());
    let cli = format!("solo-cli{}", std::env::consts::EXE_SUFFIX);
    if let Some(cli) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(cli)))
        .filter(|cli| cli.is_file())
    {
        env.insert(CLI_ENV.to_string(), cli.to_string_lossy().into_owned());
    }
}

/// The environment variable with the path of `solo-agent-hook`, which agents
/// and scripts run in Solo's terminals report their status with.
pub const AGENT_HOOK_ENV: &str = "SOLO_AGENT_HOOK";

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
    // Unused by shells that aren't integrated below.
    env.insert(SHELL_NONCE_ENV.to_string(), new_nonce());
    // How agents and scripts report their status (docs/agent-status-protocol.md).
    if let Some(hook) = agent_hook_executable() {
        env.insert(
            AGENT_HOOK_ENV.to_string(),
            hook.to_string_lossy().into_owned(),
        );
    }
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

/// 64 unpredictable bits: the hasher's keys are random.
pub(crate) fn new_nonce() -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(elapsed.as_nanos());
    }
    format!("{:016x}", hasher.finish())
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

/// Where the scripts go: a directory in the temp dir of this user's own.
fn scripts_dir() -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
        // /tmp is shared between users, who mustn't be able to swap the
        // scripts this user's shells and agents run.
        // SAFETY: getuid has no preconditions and can't fail.
        let uid = unsafe { libc::getuid() };
        let dir = std::env::temp_dir().join(format!("solo-shell-integration-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = std::fs::symlink_metadata(&dir)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::other(format!(
                "{} isn't a private directory of this user",
                dir.display()
            )));
        }
        Ok(dir)
    }
    #[cfg(not(unix))]
    {
        // Windows' temp dir is the user's own.
        Ok(std::env::temp_dir().join("solo-shell-integration"))
    }
}

/// Shells only load extra scripts from disk, so they go to the temp dir.
fn write_script(name: &str, script: &str) -> std::io::Result<String> {
    let path = scripts_dir()?.join(name);
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

/// SOLO: Lets Codex started from the integrated shell report what it's doing
/// through hooks, so its turns are tracked like Claude Code's.
///
/// The shell scripts wrap `codex` to pass the hooks as `-c hooks.<event>=...`
/// config overrides, one per line in `SOLO_CODEX_ARGS`: Codex adds them to
/// the user's own config and hooks for that session, without changing
/// `config.toml`. Codex runs hooks it doesn't trust yet only after asking, so
/// the first session shows its hook review once.
///
/// The hooks run `solo-agent-hook`, which reads the event from Codex's input
/// (the same events as Claude Code's). Codex runs a hook command with the
/// session's shell: `sh -c` elsewhere, but on Windows (since Codex 0.114)
/// `powershell.exe -NoProfile -Command`, or `cmd /C` only if it finds no
/// PowerShell. So on Windows the command names the program by its full path
/// rather than through an environment variable: PowerShell doesn't expand
/// `%VAR%`, and looking up the unknown command made it fail after seconds.
pub fn integrate_codex(is_windows: bool, env: &mut HashMap<String, String>) {
    let Some(hook) = agent_hook_executable() else {
        return;
    };
    match codex_hook_args(is_windows, &hook.to_string_lossy()) {
        Some(args) => {
            env.insert("SOLO_CODEX_ARGS".to_string(), args.join("\n"));
        }
        None => log::warn!(
            "Codex can't be given a hook command for {}: it won't report its status",
            hook.display()
        ),
    }
}

/// SOLO: Lets GitHub Copilot CLI started from the integrated shell report
/// what it's doing through hooks, so its turns are tracked like Claude Code's.
///
/// The hooks are in a plugin Solo writes (`copilot-plugin` beside the shell
/// scripts), whose folder the shell scripts get in `SOLO_COPILOT_PLUGIN`;
/// they wrap `copilot` to load it with `--plugin-dir`, for that session only.
/// Nothing is installed into Copilot's own folder.
///
/// Each hook runs `solo-agent-hook` with the kind of report as its argument,
/// so it works whether or not Copilot passes the event on stdin; Copilot runs
/// hooks with PowerShell on Windows and bash elsewhere.
pub fn integrate_copilot(is_windows: bool, env: &mut HashMap<String, String>) {
    let Some(hook) = agent_hook_executable() else {
        return;
    };
    match write_copilot_plugin(&hook.to_string_lossy(), is_windows) {
        Ok(dir) => {
            env.insert("SOLO_COPILOT_PLUGIN".to_string(), dir);
        }
        Err(error) => log::warn!("failed to write the Copilot hook plugin: {error}"),
    }
}

fn write_copilot_plugin(hook: &str, is_windows: bool) -> std::io::Result<String> {
    let manifest = serde_json::json!({
        "name": "solo",
        "version": "1.0.0",
        "description": "Reports what Copilot is doing to the Solo terminal it runs in.",
    });
    let manifest = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
    write_script("copilot-plugin/.claude-plugin/plugin.json", &manifest)?;
    let hooks =
        serde_json::to_string_pretty(&copilot_hooks(hook)).map_err(std::io::Error::other)?;
    let hooks_path = write_script("copilot-plugin/hooks/hooks.json", &hooks)?;
    let dir = Path::new(&hooks_path)
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(if is_windows {
        dir.replace('/', "\\")
    } else {
        dir
    })
}

/// Copilot hooks in Claude Code's format (PascalCase events), for each event
/// Solo reports, with the kind of report each one is.
fn copilot_hooks(hook: &str) -> serde_json::Value {
    let powershell_path = hook.replace('\'', "''");
    let sh_path = hook.replace('\'', r"'\''");
    let group = |kind: AgentHookKind| {
        let kind = kind.as_str();
        serde_json::json!([{
            "hooks": [{
                "type": "command",
                "powershell": format!("& '{powershell_path}' {kind} --agent copilot"),
                "bash": format!("'{sh_path}' {kind} --agent copilot"),
                "timeout": 5,
            }]
        }])
    };
    serde_json::json!({
        "hooks": {
            "SessionStart": group(AgentHookKind::SessionStarted),
            "UserPromptSubmit": group(AgentHookKind::PromptSubmitted),
            "PermissionRequest": group(AgentHookKind::NeedsInput),
            "Stop": group(AgentHookKind::Stopped),
        }
    })
}

/// SOLO: Lets OpenCode started from the integrated shell report what it's
/// doing, so its turns are tracked like Claude Code's.
///
/// OpenCode has no command hooks, only JavaScript plugins, so Solo writes
/// plugins (beside the shell scripts) that run `solo-agent-hook report` on
/// OpenCode's events. The shell scripts wrap `opencode` to load them for that
/// session only, unless the user set the variable themselves; nothing is
/// written to OpenCode's own config:
/// - OpenCode 2 runs a shared server for all its clients, so the plugin is a
///   TUI plugin (it runs in the terminal) listed in
///   `OPENCODE_CLI_CONFIG_CONTENT`, from `SOLO_OPENCODE_CLI_CONFIG`
///   (<https://opencode.ai/docs/cli/config/#inline-config>). That list
///   replaces the one in the user's `cli.json`, so it's theirs plus Solo's.
/// - OpenCode 1 loads plugins from the extra config folder in
///   `OPENCODE_CONFIG_DIR`, from `SOLO_OPENCODE_CONFIG_DIR`
///   (<https://docs.opencode.ai/docs/config/#custom-directory>).
pub fn integrate_opencode(env: &mut HashMap<String, String>) {
    if agent_hook_executable().is_none() {
        return;
    }
    let (plugin, config_dir) = match write_opencode_plugins() {
        Ok(paths) => paths,
        Err(error) => {
            log::warn!("failed to write the OpenCode plugins: {error}");
            return;
        }
    };
    env.insert("SOLO_OPENCODE_CONFIG_DIR".to_string(), config_dir);
    let path = opencode_cli_config_path(env);
    let user_settings = std::fs::read_to_string(&path).ok();
    let user_settings = user_settings
        .as_deref()
        .map(|contents| (contents, path.parent().unwrap_or(Path::new("/"))));
    match opencode_cli_config(&plugin, user_settings) {
        Some(config) => {
            env.insert("SOLO_OPENCODE_CLI_CONFIG".to_string(), config);
        }
        None => log::warn!(
            "{} isn't JSON Solo can read: OpenCode 2 won't report its status",
            path.display()
        ),
    }
}

const OPENCODE_SHARED: &str = include_str!("shell_integration/opencode/solo-opencode.js");
const OPENCODE_TUI_PLUGIN: &str = include_str!("shell_integration/opencode/tui.js");
const OPENCODE_V1_PLUGIN: &str = include_str!("shell_integration/opencode/solo-v1.js");

/// Writes the OpenCode 2 plugin folder and OpenCode 1's config folder (whose
/// plugin imports from the former); returns the plugin's file URL and the
/// config folder.
fn write_opencode_plugins() -> std::io::Result<(String, String)> {
    write_script("opencode-plugin/solo-opencode.js", OPENCODE_SHARED)?;
    let tui = write_script("opencode-plugin/tui.js", OPENCODE_TUI_PLUGIN)?;
    let v1 = write_script("opencode-config/plugins/solo.js", OPENCODE_V1_PLUGIN)?;
    let plugin_dir = native_path(Path::new(&tui).parent().unwrap_or(Path::new("")));
    let plugin = url::Url::from_file_path(&plugin_dir).map_err(|()| {
        std::io::Error::other(format!("{} isn't an absolute path", plugin_dir.display()))
    })?;
    let config_dir = native_path(
        Path::new(&v1)
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("")),
    );
    Ok((
        plugin.to_string(),
        config_dir.to_string_lossy().into_owned(),
    ))
}

/// `path` with the platform's separators (the scripts' paths use `/`).
fn native_path(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    } else {
        path.to_path_buf()
    }
}

/// Where OpenCode 2 keeps the user's CLI settings
/// (<https://opencode.ai/docs/cli/config/#configuration-file>).
fn opencode_cli_config_path(env: &HashMap<String, String>) -> PathBuf {
    env.get("XDG_CONFIG_HOME")
        .cloned()
        .or_else(|| std::env::var("XDG_CONFIG_HOME").ok())
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().join(".config"))
        .join("opencode")
        .join("cli.json")
}

/// OpenCode 2 CLI settings that load the plugin at `plugin` (a file URL) as
/// well as the user's own plugins, from their `cli.json` (its contents and
/// folder) if they have one, since an inline `plugins` list replaces theirs.
/// `None` if their `cli.json` can't be read as JSON: their plugins would be
/// lost.
fn opencode_cli_config(plugin: &str, user: Option<(&str, &Path)>) -> Option<String> {
    let mut plugins = Vec::new();
    if let Some((contents, dir)) = user.filter(|(contents, _)| !contents.trim().is_empty()) {
        let settings: serde_json::Value = serde_json::from_str(contents).ok()?;
        let user_plugins = settings
            .get("plugins")
            .and_then(|plugins| plugins.as_array())
            .cloned()
            .unwrap_or_default();
        // Relative paths are relative to cli.json's folder.
        let absolute = |package: &str| -> serde_json::Value {
            if ["./", "../", ".\\", "..\\"]
                .iter()
                .any(|prefix| package.starts_with(prefix))
            {
                dir.join(package).to_string_lossy().into_owned().into()
            } else {
                package.into()
            }
        };
        for entry in user_plugins {
            plugins.push(match entry {
                serde_json::Value::String(package) => absolute(&package),
                serde_json::Value::Object(mut entry) => {
                    let package = entry.get("package").and_then(|package| package.as_str());
                    if let Some(package) = package.map(absolute) {
                        entry.insert("package".to_string(), package);
                    }
                    serde_json::Value::Object(entry)
                }
                other => other,
            });
        }
    }
    plugins.push(serde_json::json!({ "package": plugin }));
    Some(serde_json::json!({ "plugins": plugins }).to_string())
}

/// The Codex events Solo reports, as in its `hooks` config.
const CODEX_HOOK_EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "Stop",
    // Pressing Esc ends the turn without a Stop.
    "Interrupt",
];

/// The `-c` arguments that add Solo's hooks, running the hook program at
/// `hook`, to a Codex session; `None` if there's no command for that path.
/// The values are TOML with literal (single-quoted) strings only: Codex reads
/// a value that isn't valid TOML as a plain string, silently, and Windows
/// PowerShell can't pass double quotes on to Codex.
///
/// Codex trusts a hook by a hash of its command and timeout, so changing
/// either (or Solo's folder, on Windows) makes it ask to trust them again.
fn codex_hook_args(is_windows: bool, hook: &str) -> Option<Vec<String>> {
    let command = if is_windows {
        codex_windows_hook_command(hook)?
    } else {
        "'\"$SOLO_AGENT_HOOK\" --agent codex'".to_string()
    };
    Some(
        CODEX_HOOK_EVENTS
            .iter()
            .flat_map(|event| {
                // Codex allows Interrupt hooks 3 seconds at most. The hook
                // takes a few milliseconds, but on Windows it's started by
                // PowerShell, which can take a second or more to start.
                let timeout = if *event == "Interrupt" { 3 } else { 10 };
                [
                    "-c".to_string(),
                    format!(
                        "hooks.{event}=[{{hooks=[{{type='command',command={command},timeout={timeout}}}]}}]"
                    ),
                ]
            })
            .collect(),
    )
}

/// The hook command for Codex on Windows, as a TOML literal string: one that
/// both PowerShell and `cmd /C` run (a bare path) if the path needs no
/// quoting, otherwise PowerShell's call of a quoted path, in a TOML multi-line
/// literal string since it has single quotes. `None` if TOML can't hold it.
fn codex_windows_hook_command(hook: &str) -> Option<String> {
    let plain = hook
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '\\' | ':' | '.' | '_' | '-'));
    if plain && !hook.is_empty() {
        return Some(format!("'{hook} --agent codex'"));
    }
    // PowerShell takes these as single quotes too.
    let mut quoted = String::new();
    for c in hook.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    let command = format!("& '{quoted}' --agent codex");
    (!command.contains("'''") && !command.contains(['\n', '\r']))
        .then(|| format!("'''{command}'''"))
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

    /// A kind by its name, or by the state it leads to (`working`,
    /// `blocked`), as other tools name them.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "working" => Some(AgentHookKind::PromptSubmitted),
            "blocked" => Some(AgentHookKind::NeedsInput),
            text => AgentHookKind::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == text),
        }
    }
}

/// SOLO: A report from an AI agent's hook, sent as
/// `OSC 6973 ; agent ; <kind> [; <order> [; <session id> [; <agent>]]]`
/// (documented in docs/agent-status-protocol.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHook {
    pub kind: AgentHookKind,
    /// When the hook started (ms since the epoch), or any number that grows
    /// with each report. Hooks run in the background and can arrive out of
    /// order.
    pub sent_at_ms: Option<u64>,
    /// The agent's session (Claude Code's `session_id`), which tells which
    /// transcript is this terminal's.
    pub session_id: Option<AgentSessionId>,
    /// SOLO: The session id as reported, for agents whose ids aren't UUIDs
    /// (OpenCode's `ses_...`): up to 128 letters, digits, `-` or `_`.
    pub raw_session_id: Option<String>,
    /// What the agent calls itself, for programs Solo doesn't know as agents.
    pub agent: Option<String>,
    /// Where the agent works, which can differ from where the shell is (a
    /// session resumed from another folder, say).
    pub working_directory: Option<PathBuf>,
}

/// Longest agent name a report can give.
const MAX_AGENT_NAME_CHARS: usize = 40;

impl AgentHook {
    /// Parses the parameters after `agent`.
    pub fn parse(params: &[String]) -> Option<Self> {
        let (kind, rest) = params.split_first()?;
        let kind = AgentHookKind::parse(kind)?;
        let sent_at_ms = rest.first().and_then(|sent_at| sent_at.trim().parse().ok());
        let raw_session_id = rest
            .get(1)
            .map(|id| id.trim())
            .filter(|id| is_session_id(id))
            .map(str::to_string);
        let session_id = raw_session_id.as_deref().and_then(AgentSessionId::parse);
        let agent = rest
            .get(2)
            .map(|name| name.trim())
            .filter(|name| {
                !name.is_empty()
                    && name.chars().count() <= MAX_AGENT_NAME_CHARS
                    && name.chars().all(|character| {
                        character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ' ')
                    })
            })
            .map(str::to_string);
        // Percent-encoded, and last, so a `;` in it can't move the others.
        let working_directory = (rest.len() > 3)
            .then(|| rest[3..].join(";"))
            .and_then(|encoded| percent_decode(&encoded))
            .map(PathBuf::from)
            .filter(|directory| directory.is_absolute());
        Some(Self {
            kind,
            sent_at_ms,
            session_id,
            raw_session_id,
            agent,
            working_directory,
        })
    }
}

/// SOLO: Whether `id` can be an agent's session id: up to 128 letters,
/// digits, `-` or `_`.
pub fn is_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// SOLO: `%XX` bytes back, as UTF-8; `None` if it isn't valid.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%' {
            let hex = std::str::from_utf8(bytes.get(ix + 1..ix + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            ix += 3;
        } else {
            decoded.push(bytes[ix]);
            ix += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.trim().is_empty() && !decoded.chars().any(char::is_control)).then_some(decoded)
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
    /// The nonce the shell was started with ([`SHELL_NONCE_ENV`]), `None` if
    /// Solo didn't integrate it.
    nonce: Option<String>,
    /// The terminal's id ([`TERMINAL_ID_ENV`]).
    terminal_id: Option<String>,
}

impl ShellIntegration {
    pub fn new(nonce: Option<String>) -> Self {
        Self {
            nonce,
            ..Self::default()
        }
    }

    pub fn with_terminal_id(mut self, terminal_id: Option<String>) -> Self {
        self.terminal_id = terminal_id;
        self
    }

    /// The id the terminal's shell got in [`TERMINAL_ID_ENV`].
    pub fn terminal_id(&self) -> Option<&str> {
        self.terminal_id.as_deref()
    }

    /// Whether a sequence came from the shell Solo integrated.
    fn from_integration(&self, nonce: Option<&str>) -> bool {
        self.nonce.is_some() && self.nonce.as_deref() == nonce
    }

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
            AlacShellMark::CommandLine { command, nonce } => {
                // What re-running a block runs: only from Solo's integration
                // when it has one, not from output that looks like it.
                if self.nonce.is_some() && !self.from_integration(nonce.as_deref()) {
                    return;
                }
                if let Some(block) = self.blocks.last_mut() {
                    block.command = Some(command);
                }
            }
            AlacShellMark::Property { key, value, nonce } => {
                // Completions run this program: only from Solo's integration.
                if key == "Shell" && self.from_integration(nonce.as_deref()) {
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
            integration.handle_mark(
                AlacShellMark::CommandLine {
                    command: "ls".into(),
                    nonce: None,
                },
                prompt,
            );
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
                    && args[3].contains("/powershell-integration-")
                    && !args.iter().any(|arg| arg.contains("Encoded"))
        ));

        let shell = integrate_shell(Shell::Program("bash".into()), "bash", false, env);
        assert!(matches!(shell, Shell::WithArguments { ref args, .. } if args[0] == "--rcfile"));

        let shell = integrate_shell(Shell::Program("/usr/bin/fish".into()), "fish", false, env);
        assert!(
            matches!(shell, Shell::WithArguments { ref args, .. } if args[0] == "--init-command")
        );
        // Only the nonce (and the hook program, when it's next to Solo):
        // these shells take their scripts as arguments.
        assert!(
            env.keys()
                .all(|key| key == SHELL_NONCE_ENV || key == AGENT_HOOK_ENV),
            "{env:?}"
        );

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
    fn only_trusts_sequences_with_the_nonce() {
        let shell = |value: &str, nonce: Option<&str>| AlacShellMark::Property {
            key: "Shell".into(),
            value: value.into(),
            nonce: nonce.map(str::to_string),
        };
        let command = |command: &str, nonce: Option<&str>| AlacShellMark::CommandLine {
            command: command.into(),
            nonce: nonce.map(str::to_string),
        };

        let mut integration = ShellIntegration::new(Some("abc123".into()));
        integration.handle_mark(AlacShellMark::PromptStart, 0);
        integration.handle_mark(shell(r"C:\evil.exe", None), 0);
        integration.handle_mark(shell(r"C:\evil.exe", Some("guess")), 0);
        assert_eq!(integration.shell(), None);
        integration.handle_mark(shell("pwsh.exe", Some("abc123")), 0);
        assert_eq!(integration.shell(), Some("pwsh.exe"));
        integration.handle_mark(command("rm -rf ~", None), 1);
        assert_eq!(integration.blocks()[0].command, None);
        integration.handle_mark(command("ls", Some("abc123")), 1);
        assert_eq!(integration.blocks()[0].command.as_deref(), Some("ls"));

        // Without Solo's integration, no shell to run is trusted.
        let mut integration = ShellIntegration::new(None);
        integration.handle_mark(shell("pwsh.exe", Some("abc123")), 0);
        assert_eq!(integration.shell(), None);
    }

    #[test]
    fn integrated_shells_get_a_nonce() {
        let env = &mut HashMap::default();
        integrate_shell(Shell::Program("bash".into()), "bash", false, env);
        let nonce = env.get(SHELL_NONCE_ENV).cloned().unwrap();
        assert_eq!(nonce.len(), 16);
        integrate_shell(Shell::Program("bash".into()), "bash", false, env);
        assert_ne!(env.get(SHELL_NONCE_ENV), Some(&nonce));
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
                raw_session_id: None,
                agent: None,
                working_directory: None,
            })
        );
        // `date` without %N leaves the time out.
        assert_eq!(
            AgentHook::parse(&params(&["stop", ""])),
            Some(AgentHook {
                kind: AgentHookKind::Stopped,
                sent_at_ms: None,
                session_id: None,
                raw_session_id: None,
                agent: None,
                working_directory: None,
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
        // The public protocol: states by name, a sequence number, an agent's
        // name for programs Solo doesn't know.
        let hook = AgentHook::parse(&params(&["working", "3", "", "my agent"])).unwrap();
        assert_eq!(hook.kind, AgentHookKind::PromptSubmitted);
        assert_eq!(hook.sent_at_ms, Some(3));
        assert_eq!(hook.session_id, None);
        assert_eq!(hook.agent.as_deref(), Some("my agent"));
        assert_eq!(
            AgentHook::parse(&params(&["blocked"])).map(|hook| hook.kind),
            Some(AgentHookKind::NeedsInput)
        );
        for name in ["", "a\u{7}b", "evil;x", &"x".repeat(41)] {
            assert_eq!(
                AgentHook::parse(&params(&["idle", "1", "", name])).and_then(|hook| hook.agent),
                None,
                "{name:?}"
            );
        }
        assert_eq!(AgentHook::parse(&[]), None);

        // Where the agent works, percent-encoded, after the agent's name.
        let hook = AgentHook::parse(&params(&[
            "prompt",
            "1",
            "",
            "",
            r"C:%5CGit%5C%E9%BE%99%3Bx",
        ]))
        .unwrap();
        assert_eq!(hook.agent, None);
        assert_eq!(
            hook.working_directory,
            Some(PathBuf::from("C:\\Git\\\u{9f99};x"))
        );
        // Split at a `;` the encoding missed: joined back.
        let hook =
            AgentHook::parse(&params(&["prompt", "1", "", "codex", r"C:%5Ca", "b"])).unwrap();
        assert_eq!(hook.working_directory, Some(PathBuf::from("C:\\a;b")));
        // Not a path, or not valid: none.
        for bad in ["relative", "%ZZ", "%C3", "C:%5C%07"] {
            assert_eq!(
                AgentHook::parse(&params(&["prompt", "1", "", "", bad]))
                    .unwrap()
                    .working_directory,
                None,
                "{bad}"
            );
        }
        for kind in AgentHookKind::ALL {
            assert_eq!(
                AgentHook::parse(&params(&[kind.as_str()])).map(|hook| hook.kind),
                Some(kind)
            );
        }
    }

    #[test]
    fn copilot_hooks_report_each_event() {
        let hooks = copilot_hooks(r"C:\Program Files\it's\solo-agent-hook.exe");
        let command = |event: &str, shell: &str| {
            hooks["hooks"][event][0]["hooks"][0][shell]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            command("UserPromptSubmit", "powershell"),
            r"& 'C:\Program Files\it''s\solo-agent-hook.exe' prompt --agent copilot"
        );
        assert_eq!(
            command("Stop", "bash"),
            r"'C:\Program Files\it'\''s\solo-agent-hook.exe' stop --agent copilot"
        );
        assert_eq!(
            command("PermissionRequest", "powershell"),
            r"& 'C:\Program Files\it''s\solo-agent-hook.exe' input --agent copilot"
        );
        assert_eq!(
            command("SessionStart", "bash"),
            r"'C:\Program Files\it'\''s\solo-agent-hook.exe' start --agent copilot"
        );
    }

    #[test]
    fn opencode_cli_config_keeps_the_users_plugins() {
        let plugin =
            "file:///C:/Users/me/AppData/Local/Temp/solo-shell-integration/opencode-plugin";
        let parse = |config: Option<String>| -> serde_json::Value {
            serde_json::from_str(&config.expect("a config")).expect("JSON")
        };
        let solo = serde_json::json!({ "package": plugin });
        assert_eq!(
            parse(opencode_cli_config(plugin, None)),
            serde_json::json!({ "plugins": [solo] })
        );
        assert_eq!(
            parse(opencode_cli_config(
                plugin,
                Some((
                    r#"{"theme":{"name":"x"}}"#,
                    Path::new("/home/me/.config/opencode")
                ))
            )),
            serde_json::json!({ "plugins": [solo] })
        );
        // Their list, in order, with relative paths made absolute, then Solo's.
        let dir = Path::new("/home/me/.config/opencode");
        let user = r#"{"plugins": ["-opencode.notifications", "./plugins/status",
            {"package": "../shared", "options": {"compact": true}}, "@acme/tui@1.0.0"]}"#;
        assert_eq!(
            parse(opencode_cli_config(plugin, Some((user, dir)))),
            serde_json::json!({ "plugins": [
                "-opencode.notifications",
                dir.join("./plugins/status").to_string_lossy(),
                { "package": dir.join("../shared").to_string_lossy(), "options": { "compact": true } },
                "@acme/tui@1.0.0",
                solo,
            ] })
        );
        // Their plugins can't be kept: Solo's isn't added.
        assert_eq!(
            opencode_cli_config(plugin, Some(("{ // comment", dir))),
            None
        );
    }

    #[test]
    fn opencode_plugins_are_written() {
        let (plugin, config_dir) = write_opencode_plugins().expect("writes the plugins");
        let plugin_dir = url::Url::parse(&plugin)
            .expect("a URL")
            .to_file_path()
            .expect("a file URL");
        assert_eq!(
            std::fs::read_to_string(plugin_dir.join("tui.js")).unwrap(),
            OPENCODE_TUI_PLUGIN
        );
        let v1 = Path::new(&config_dir).join("plugins").join("solo.js");
        assert_eq!(std::fs::read_to_string(&v1).unwrap(), OPENCODE_V1_PLUGIN);
        // What the OpenCode 1 plugin imports.
        let import = "../../opencode-plugin/solo-opencode.js";
        assert!(OPENCODE_V1_PLUGIN.contains(import));
        let shared = v1.parent().unwrap().join(import);
        assert_eq!(std::fs::read_to_string(shared).unwrap(), OPENCODE_SHARED);
        assert!(OPENCODE_TUI_PLUGIN.contains("\"./solo-opencode.js\""));
    }

    #[test]
    fn opencode_session_ids_are_kept_raw() {
        let params = |params: &[&str]| -> Vec<String> {
            params.iter().map(|param| param.to_string()).collect()
        };
        let hook = AgentHook::parse(&params(&["prompt", "1", "ses_4f2a1cB9", "opencode"])).unwrap();
        assert_eq!(hook.raw_session_id.as_deref(), Some("ses_4f2a1cB9"));
        assert_eq!(hook.session_id, None);
        assert_eq!(hook.agent.as_deref(), Some("opencode"));
        let uuid = "0f6ed4b2-755b-4e18-8bd1-f60252548036";
        let hook = AgentHook::parse(&params(&["start", "1", uuid])).unwrap();
        assert_eq!(hook.raw_session_id.as_deref(), Some(uuid));
        assert!(hook.session_id.is_some());
        let too_long = "x".repeat(129);
        for bad in ["a b", "a/b", "", too_long.as_str()] {
            assert_eq!(
                AgentHook::parse(&params(&["stop", "1", bad]))
                    .unwrap()
                    .raw_session_id,
                None,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn codex_hook_args_are_valid_toml() {
        let cases = [
            (false, "/opt/solo/solo-agent-hook", r#""$SOLO_AGENT_HOOK" --agent codex"#),
            // Runs in both PowerShell and cmd as is.
            (
                true,
                r"C:\Users\Admin\AppData\Local\Programs\Solo\solo-agent-hook.exe",
                r"C:\Users\Admin\AppData\Local\Programs\Solo\solo-agent-hook.exe --agent codex",
            ),
            (
                true,
                r"D:\工具\Solo_2.2-dev\solo-agent-hook.exe",
                r"D:\工具\Solo_2.2-dev\solo-agent-hook.exe --agent codex",
            ),
            (
                true,
                r"C:\Program Files\Solo 测试\solo-agent-hook.exe",
                r"& 'C:\Program Files\Solo 测试\solo-agent-hook.exe' --agent codex",
            ),
            (
                true,
                r"C:\Users\O'Brien\Solo\solo-agent-hook.exe",
                r"& 'C:\Users\O''Brien\Solo\solo-agent-hook.exe' --agent codex",
            ),
            (
                true,
                "C:\\Users\\\u{2019}x\\100% $(x) & y\\solo-agent-hook.exe",
                "& 'C:\\Users\\\u{2019}\u{2019}x\\100% $(x) & y\\solo-agent-hook.exe' --agent codex",
            ),
        ];
        for (is_windows, hook, expected) in cases {
            let args = codex_hook_args(is_windows, hook).unwrap();
            assert_eq!(args.len(), 2 * CODEX_HOOK_EVENTS.len());
            for (pair, event) in args.chunks(2).zip(CODEX_HOOK_EVENTS) {
                assert_eq!(pair[0], "-c");
                let (key, value) = pair[1].split_once('=').unwrap();
                assert_eq!(key, format!("hooks.{event}"));
                // What Codex does with a `-c` value.
                let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
                let hook = &parsed["value"][0]["hooks"][0];
                assert_eq!(hook["type"].as_str(), Some("command"));
                let timeout = if event == "Interrupt" { 3 } else { 10 };
                assert_eq!(hook["timeout"].as_integer(), Some(timeout));
                assert_eq!(hook["command"].as_str(), Some(expected));
                // Windows PowerShell drops double quotes from arguments, and
                // cmd.exe (running a codex.cmd) expands `%VAR%`.
                assert_eq!(pair[1].contains('"'), !is_windows, "{}", pair[1]);
                if is_windows {
                    assert!(!expected.contains("%SOLO"), "{expected}");
                }
            }
        }
        // A path TOML can't hold in a literal string.
        assert_eq!(codex_hook_args(true, r"C:\a''b\solo-agent-hook.exe"), None);
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
