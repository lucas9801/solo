//! SOLO: Status reporting for more agents, beyond Claude Code, Codex, Copilot
//! CLI and OpenCode (`shell_integration`).
//!
//! Per session, without touching the user's config (the `more_agent_hooks`
//! setting):
//! - Pi and oh-my-pi: an extension the shell scripts load with `pi -e` /
//!   `omp -e` (`SOLO_PI_EXTENSION`);
//! - Kilo Code CLI: a plugin in a folder of Solo's, given as the extra config
//!   folder `KILO_CONFIG_DIR`, whose plugins Kilo adds to the user's;
//! - Gemini CLI and Qwen Code: hooks in a "system defaults" settings file of
//!   Solo's (`GEMINI_CLI_SYSTEM_DEFAULTS_PATH`,
//!   `QWEN_CODE_SYSTEM_DEFAULTS_PATH`), whose hooks they add to the user's
//!   (hook lists are concatenated across settings scopes). An administrator's
//!   system defaults file is copied into Solo's, so it still applies.
//!
//! Only with the user's config (the `global_agent_hooks` setting, one agent at
//! a time; see `global_agent_hooks`): Kimi Code CLI and MastraCode, which read
//! hooks from the user's config file alone.
//!
//! Which events to report follows Herdr's integrations
//! (https://github.com/herdrdev/herdr, `src/integration`, Copyright Herdr
//! contributors, Apache License 2.0). SOLO: every hook runs Solo's hook
//! program (`solo-agent-hook`) rather than Herdr's PowerShell and shell
//! scripts, and nothing is installed into the agents' folders.

use collections::HashMap;
use std::path::{Path, PathBuf};

use crate::shell_integration::{AgentHookKind, agent_hook_executable, native_path, write_script};

const PI_EXTENSION: &str = include_str!("shell_integration/pi/solo-pi.ts");
const KILO_PLUGIN: &str = include_str!("shell_integration/opencode/solo-kilo.js");
const OPENCODE_SHARED: &str = include_str!("shell_integration/opencode/solo-opencode.js");

/// The environment variable with the Pi / oh-my-pi extension's path, for the
/// shell scripts' `pi` and `omp` wrappers.
pub const PI_EXTENSION_ENV: &str = "SOLO_PI_EXTENSION";
const KILO_CONFIG_DIR_ENV: &str = "KILO_CONFIG_DIR";
const GEMINI_SYSTEM_DEFAULTS_ENV: &str = "GEMINI_CLI_SYSTEM_DEFAULTS_PATH";
const QWEN_SYSTEM_DEFAULTS_ENV: &str = "QWEN_CODE_SYSTEM_DEFAULTS_PATH";

/// SOLO: Lets Pi, oh-my-pi, Kilo Code CLI, Gemini CLI and Qwen Code started
/// in this terminal report what they're doing, for this terminal's sessions
/// only (see the module docs).
pub fn integrate_more_agents(is_windows: bool, env: &mut HashMap<String, String>) {
    let Some(hook) = agent_hook_executable() else {
        return;
    };
    let hook = hook.to_string_lossy().into_owned();
    match write_script("pi-extension/solo-pi.ts", PI_EXTENSION) {
        Ok(path) => {
            env.insert(
                PI_EXTENSION_ENV.to_string(),
                native_path(Path::new(&path)).to_string_lossy().into_owned(),
            );
        }
        Err(error) => log::warn!("failed to write the Pi extension: {error}"),
    }
    if user_variable(env, KILO_CONFIG_DIR_ENV).is_none() {
        match write_kilo_plugin() {
            Ok(dir) => {
                env.insert(KILO_CONFIG_DIR_ENV.to_string(), dir);
            }
            Err(error) => log::warn!("failed to write the Kilo plugin: {error}"),
        }
    } else {
        log::info!("{KILO_CONFIG_DIR_ENV} is set: Kilo won't report its status");
    }
    for agent in [SettingsAgent::Gemini, SettingsAgent::Qwen] {
        let variable = agent.variable();
        let current = user_variable(env, variable)
            .map(PathBuf::from)
            .or_else(|| agent.default_system_defaults());
        let theirs = current.and_then(|path| std::fs::read_to_string(path).ok());
        let Some(settings) = system_defaults(agent, theirs.as_deref(), &hook, is_windows) else {
            log::warn!("{variable} names a file Solo can't read as JSON: no status hooks");
            continue;
        };
        match write_script(&agent.file_name(&settings), &settings) {
            Ok(path) => {
                env.insert(
                    variable.to_string(),
                    native_path(Path::new(&path)).to_string_lossy().into_owned(),
                );
            }
            Err(error) => log::warn!("failed to write the {variable} file: {error}"),
        }
    }
}

/// The value of `name` in the terminal's environment, or else Solo's.
fn user_variable(env: &HashMap<String, String>, name: &str) -> Option<String> {
    env.get(name)
        .cloned()
        .or_else(|| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

/// Writes Kilo's extra config folder with Solo's plugin (and the OpenCode
/// code it shares); returns the folder.
fn write_kilo_plugin() -> std::io::Result<String> {
    // The plugin imports `../../opencode-plugin/solo-opencode.js`.
    write_script("opencode-plugin/solo-opencode.js", OPENCODE_SHARED)?;
    let plugin = write_script("kilo-config/plugin/solo.js", KILO_PLUGIN)?;
    let dir = Path::new(&plugin)
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new(""));
    Ok(native_path(dir).to_string_lossy().into_owned())
}

/// Agents that take hooks from a system defaults settings file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsAgent {
    Gemini,
    Qwen,
}

impl SettingsAgent {
    fn name(self) -> &'static str {
        match self {
            SettingsAgent::Gemini => "gemini",
            SettingsAgent::Qwen => "qwen",
        }
    }

    fn variable(self) -> &'static str {
        match self {
            SettingsAgent::Gemini => GEMINI_SYSTEM_DEFAULTS_ENV,
            SettingsAgent::Qwen => QWEN_SYSTEM_DEFAULTS_ENV,
        }
    }

    /// Where the agent looks for system defaults when the variable isn't set
    /// (Gemini CLI's docs, "System defaults file"; Qwen Code keeps Gemini
    /// CLI's layout under its own name).
    fn default_system_defaults(self) -> Option<PathBuf> {
        let dir = match (self, std::env::consts::OS) {
            (SettingsAgent::Gemini, "windows") => {
                PathBuf::from(std::env::var("ProgramData").ok()?).join("gemini-cli")
            }
            (SettingsAgent::Gemini, "macos") => {
                PathBuf::from("/Library/Application Support/GeminiCli")
            }
            (SettingsAgent::Gemini, _) => PathBuf::from("/etc/gemini-cli"),
            (SettingsAgent::Qwen, "windows") => {
                PathBuf::from(std::env::var("ProgramData").ok()?).join("qwen-code")
            }
            (SettingsAgent::Qwen, "macos") => {
                PathBuf::from("/Library/Application Support/QwenCode")
            }
            (SettingsAgent::Qwen, _) => PathBuf::from("/etc/qwen-code"),
        };
        Some(dir.join("system-defaults.json"))
    }

    /// The file's name, after its contents: a running agent keeps reading the
    /// file it started with.
    fn file_name(self, settings: &str) -> String {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(settings.as_bytes());
        let short: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("{}-system-defaults-{short}.json", self.name())
    }

    /// Solo's hooks: (event, matcher, kind of report).
    fn hooks(self) -> &'static [(&'static str, Option<&'static str>, AgentHookKind)] {
        match self {
            // Gemini CLI's hook events (docs/hooks/reference.md): a turn is
            // BeforeAgent .. AfterAgent; a tool waiting for approval sends a
            // ToolPermission notification.
            SettingsAgent::Gemini => &[
                ("SessionStart", None, AgentHookKind::SessionStarted),
                ("BeforeAgent", None, AgentHookKind::PromptSubmitted),
                ("Notification", None, AgentHookKind::NeedsInput),
                ("AfterAgent", None, AgentHookKind::Stopped),
            ],
            // Qwen Code's are Claude Code's.
            SettingsAgent::Qwen => &[
                ("SessionStart", None, AgentHookKind::SessionStarted),
                ("UserPromptSubmit", None, AgentHookKind::PromptSubmitted),
                ("PermissionRequest", None, AgentHookKind::NeedsInput),
                ("Notification", None, AgentHookKind::NeedsInput),
                ("Stop", None, AgentHookKind::Stopped),
            ],
        }
    }

    /// Hook timeouts: Gemini CLI's are in milliseconds, Qwen Code's in
    /// seconds.
    fn timeout(self) -> u64 {
        match self {
            SettingsAgent::Gemini => 10_000,
            SettingsAgent::Qwen => 10,
        }
    }
}

/// The command line that runs the hook program at `hook` for a hook of
/// `agent`, with `kind` as the report to fall back on (the program reads the
/// event from its input first). The agents run it with a shell (`sh -c`, or
/// `cmd /c` on Windows), so the path is quoted when it has to be.
pub(crate) fn hook_command_line(
    hook: &str,
    kind: AgentHookKind,
    agent: &str,
    is_windows: bool,
) -> String {
    let plain = !hook.is_empty()
        && hook
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '\\' | '/' | ':' | '.' | '_' | '-'));
    let program = if plain {
        hook.to_string()
    } else if is_windows {
        format!("\"{hook}\"")
    } else {
        format!("'{}'", hook.replace('\'', r"'\''"))
    };
    format!("{program} {} --agent {agent}", kind.as_str())
}

/// The system defaults settings for `agent`: theirs (the file that applied
/// before, if any) with Solo's hooks added. `None` if theirs isn't a JSON
/// object.
fn system_defaults(
    agent: SettingsAgent,
    theirs: Option<&str>,
    hook: &str,
    is_windows: bool,
) -> Option<String> {
    let mut settings = match theirs.filter(|text| !text.trim().is_empty()) {
        Some(text) => serde_json::from_str::<serde_json::Value>(text).ok()?,
        None => serde_json::json!({}),
    };
    let root = settings.as_object_mut()?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()?;
    for (event, matcher, kind) in agent.hooks() {
        let mut group = serde_json::json!({
            "hooks": [{
                "type": "command",
                "name": "solo-agent-status",
                "command": hook_command_line(hook, *kind, agent.name(), is_windows),
                "timeout": agent.timeout(),
            }]
        });
        if let Some(matcher) = matcher {
            group["matcher"] = (*matcher).into();
        }
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));
        groups.as_array_mut()?.push(group);
    }
    serde_json::to_string_pretty(&settings).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_commands_quote_paths_that_need_it() {
        assert_eq!(
            hook_command_line(
                r"C:\Solo\solo-agent-hook.exe",
                AgentHookKind::NeedsInput,
                "gemini",
                true
            ),
            r"C:\Solo\solo-agent-hook.exe input --agent gemini"
        );
        assert_eq!(
            hook_command_line(
                r"C:\Program Files\Solo\solo-agent-hook.exe",
                AgentHookKind::Stopped,
                "kimi",
                true
            ),
            r#""C:\Program Files\Solo\solo-agent-hook.exe" stop --agent kimi"#
        );
        assert_eq!(
            hook_command_line(
                "/opt/my solo/hook",
                AgentHookKind::SessionStarted,
                "qwen",
                false
            ),
            "'/opt/my solo/hook' start --agent qwen"
        );
    }

    #[test]
    fn system_defaults_add_solos_hooks_to_theirs() {
        let theirs = r#"{"general": {"vimMode": true}, "hooks": {"AfterAgent": [{"hooks": [{"type": "command", "command": "notify"}]}]}}"#;
        let settings: serde_json::Value = serde_json::from_str(
            &system_defaults(SettingsAgent::Gemini, Some(theirs), "/s/hook", false).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["general"]["vimMode"], true);
        let after = settings["hooks"]["AfterAgent"].as_array().unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0]["hooks"][0]["command"], "notify");
        assert_eq!(
            after[1]["hooks"][0]["command"],
            "/s/hook stop --agent gemini"
        );
        assert_eq!(after[1]["hooks"][0]["timeout"], 10_000);
        assert_eq!(
            settings["hooks"]["BeforeAgent"][0]["hooks"][0]["command"],
            "/s/hook prompt --agent gemini"
        );

        let qwen: serde_json::Value = serde_json::from_str(
            &system_defaults(SettingsAgent::Qwen, None, "/s/hook", false).unwrap(),
        )
        .unwrap();
        assert_eq!(
            qwen["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            "/s/hook prompt --agent qwen"
        );
        assert_eq!(qwen["hooks"]["Stop"][0]["hooks"][0]["timeout"], 10);

        // Theirs isn't something Solo can add to: no hooks, rather than losing theirs.
        assert!(system_defaults(SettingsAgent::Gemini, Some("[1]"), "/s/hook", false).is_none());
        assert!(system_defaults(SettingsAgent::Gemini, Some("{oops"), "/s/hook", false).is_none());
    }

    #[test]
    fn settings_files_are_named_after_their_contents() {
        let a = SettingsAgent::Gemini.file_name("{}");
        assert!(a.starts_with("gemini-system-defaults-") && a.ends_with(".json"));
        assert_ne!(a, SettingsAgent::Gemini.file_name("{\"x\":1}"));
    }
}
