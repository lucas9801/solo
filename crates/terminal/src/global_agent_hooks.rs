//! SOLO: Status hooks for agents that read hooks only from the user's own
//! config: Kimi Code CLI (`[[hooks]]` in `$KIMI_CODE_HOME/config.toml`,
//! default `~/.kimi-code`) and MastraCode (`~/.mastracode/hooks.json`). Neither
//! takes hooks for one session (Kimi has no option for another config file;
//! MastraCode's folder is fixed), so these go into the user's file, and only
//! for the agents the user lists in the `global_agent_hooks` setting.
//!
//! - Before Solo first changes a file it copies it to `<file>.solo-backup`.
//! - Solo's entries are marked (a commented block in Kimi's TOML, a
//!   `description` in MastraCode's JSON); taking an agent off the list removes
//!   those entries and nothing else.
//! - The hooks run Solo's hook program, which does nothing outside Solo's
//!   terminals (no `SOLO_AGENT_TOKEN`). If Solo is uninstalled first the
//!   commands are left pointing at a missing program: take the agents off the
//!   list before uninstalling.
//!
//! Which events to report follows Herdr's Kimi and MastraCode integrations
//! (https://github.com/herdrdev/herdr, `src/integration`, Copyright Herdr
//! contributors, Apache License 2.0). SOLO: Solo's hook program instead of
//! Herdr's scripts, and the user turns each agent on.

use std::io;
use std::path::{Path, PathBuf};

use crate::agent_integrations::hook_command_line;
use crate::shell_integration::AgentHookKind;

/// An agent whose hooks go into the user's config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalHookAgent {
    Kimi,
    MastraCode,
}

/// What changed in the agent's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookChange {
    /// Nothing to do: already as asked.
    Unchanged,
    /// The file was written (and backed up to the path, the first time).
    Written {
        file: PathBuf,
        backup: Option<PathBuf>,
    },
}

const KIMI_BLOCK_BEGIN: &str = "# >>> solo agent status hooks (added by Solo; remove \"kimi\" from Solo's global_agent_hooks setting to take them out)";
const KIMI_BLOCK_END: &str = "# <<< solo agent status hooks";
/// No quotes or backslashes: it's found in the file's text, where JSON would
/// escape them.
const MASTRACODE_MARK: &str = "solo-agent-status (added by Solo; take mastracode off Solo's global_agent_hooks setting to remove it)";

impl GlobalHookAgent {
    pub const ALL: [GlobalHookAgent; 2] = [GlobalHookAgent::Kimi, GlobalHookAgent::MastraCode];

    /// Its name in the setting and in reports.
    pub fn name(self) -> &'static str {
        match self {
            GlobalHookAgent::Kimi => "kimi",
            GlobalHookAgent::MastraCode => "mastracode",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|agent| agent.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The config file, for the user whose home is `home`, with the
    /// environment variable `var` read through `var`.
    pub fn config_file(self, home: &Path, var: impl Fn(&str) -> Option<String>) -> PathBuf {
        match self {
            GlobalHookAgent::Kimi => var("KIMI_CODE_HOME")
                .filter(|dir| !dir.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".kimi-code"))
                .join("config.toml"),
            GlobalHookAgent::MastraCode => home.join(".mastracode").join("hooks.json"),
        }
    }

    /// The hooks: (event, tool matcher, kind of report). Kimi's events
    /// (customization/hooks in its docs) and MastraCode's
    /// (`mastracode/sdk/src/hooks/types.ts`) are Claude Code's, with
    /// Interrupt, and MastraCode's AgentStart/AgentEnd.
    fn hooks(self) -> &'static [(&'static str, Option<&'static str>, AgentHookKind)] {
        match self {
            GlobalHookAgent::Kimi => &[
                ("SessionStart", None, AgentHookKind::SessionStarted),
                ("UserPromptSubmit", None, AgentHookKind::PromptSubmitted),
                (
                    "PreToolUse",
                    Some("^AskUserQuestion$"),
                    AgentHookKind::NeedsInput,
                ),
                ("PermissionRequest", None, AgentHookKind::NeedsInput),
                ("PermissionResult", None, AgentHookKind::PromptSubmitted),
                ("Stop", None, AgentHookKind::Stopped),
                ("StopFailure", None, AgentHookKind::Stopped),
                ("Interrupt", None, AgentHookKind::Stopped),
            ],
            GlobalHookAgent::MastraCode => &[
                ("SessionStart", None, AgentHookKind::SessionStarted),
                ("UserPromptSubmit", None, AgentHookKind::PromptSubmitted),
                ("AgentStart", None, AgentHookKind::PromptSubmitted),
                ("PermissionRequest", None, AgentHookKind::NeedsInput),
                ("PermissionResult", None, AgentHookKind::PromptSubmitted),
                ("Interrupt", None, AgentHookKind::Stopped),
                ("AgentEnd", None, AgentHookKind::Stopped),
                ("Stop", None, AgentHookKind::Stopped),
            ],
        }
    }

    /// The file's contents with Solo's hooks running `hook`, from its
    /// contents now (empty if there's no file). `None` if Solo can't read it
    /// (MastraCode's isn't a JSON object).
    pub fn with_hooks(self, contents: &str, hook: &str, is_windows: bool) -> Option<String> {
        let contents = self.without_hooks(contents)?;
        match self {
            GlobalHookAgent::Kimi => {
                let mut result = contents.trim_end_matches(['\n', '\r']).to_string();
                if !result.is_empty() {
                    result.push_str("\n\n");
                }
                result.push_str(KIMI_BLOCK_BEGIN);
                result.push('\n');
                for (event, matcher, kind) in self.hooks() {
                    result.push_str("[[hooks]]\n");
                    result.push_str(&format!("event = {}\n", toml_string(event)));
                    if let Some(matcher) = matcher {
                        result.push_str(&format!("matcher = {}\n", toml_string(matcher)));
                    }
                    let command = hook_command_line(hook, *kind, self.name(), is_windows);
                    result.push_str(&format!(
                        "command = {}\ntimeout = 10\n\n",
                        toml_string(&command)
                    ));
                }
                result.push_str(KIMI_BLOCK_END);
                result.push('\n');
                Some(result)
            }
            GlobalHookAgent::MastraCode => {
                let mut settings = if contents.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str::<serde_json::Value>(&contents).ok()?
                };
                let root = settings.as_object_mut()?;
                for (event, _, kind) in self.hooks() {
                    let entries = root
                        .entry(event.to_string())
                        .or_insert_with(|| serde_json::json!([]))
                        .as_array_mut()?;
                    entries.push(serde_json::json!({
                        "type": "command",
                        "command": hook_command_line(hook, *kind, self.name(), is_windows),
                        "timeout": 10_000,
                        "description": MASTRACODE_MARK,
                    }));
                }
                serde_json::to_string_pretty(&settings)
                    .ok()
                    .map(|text| text + "\n")
            }
        }
    }

    /// The file's contents without Solo's hooks. `None` if Solo can't read
    /// it.
    pub fn without_hooks(self, contents: &str) -> Option<String> {
        match self {
            GlobalHookAgent::Kimi => {
                if !contents.contains(KIMI_BLOCK_BEGIN) {
                    return Some(contents.to_string());
                }
                let mut kept = Vec::new();
                let mut inside = false;
                for line in contents.lines() {
                    if line.trim() == KIMI_BLOCK_BEGIN {
                        inside = true;
                    } else if inside && line.trim() == KIMI_BLOCK_END {
                        inside = false;
                    } else if !inside {
                        kept.push(line);
                    }
                }
                let mut result = kept.join("\n").trim_end().to_string();
                if !result.is_empty() {
                    result.push('\n');
                }
                Some(result)
            }
            GlobalHookAgent::MastraCode => {
                if contents.trim().is_empty() || !contents.contains(MASTRACODE_MARK) {
                    return Some(contents.to_string());
                }
                let mut settings = serde_json::from_str::<serde_json::Value>(contents).ok()?;
                let root = settings.as_object_mut()?;
                let mut emptied = Vec::new();
                for (event, entries) in root.iter_mut() {
                    if let Some(entries) = entries.as_array_mut() {
                        let before = entries.len();
                        entries.retain(|entry| {
                            entry.get("description").and_then(|d| d.as_str())
                                != Some(MASTRACODE_MARK)
                        });
                        if entries.is_empty() && before > 0 {
                            emptied.push(event.clone());
                        }
                    }
                }
                for event in emptied {
                    root.remove(&event);
                }
                serde_json::to_string_pretty(&settings)
                    .ok()
                    .map(|text| text + "\n")
            }
        }
    }

    /// Whether `contents` has Solo's hooks.
    pub fn has_hooks(self, contents: &str) -> bool {
        contents.contains(match self {
            GlobalHookAgent::Kimi => KIMI_BLOCK_BEGIN,
            GlobalHookAgent::MastraCode => MASTRACODE_MARK,
        })
    }
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut result = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => result.push_str(&format!("\\u{:04X}", c as u32)),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}

/// Adds Solo's hooks running `hook` to `agent`'s config file `file` (making
/// it if there's none and the agent's folder exists), backing the file up
/// first. Writes nothing if they're there already, as they would be written.
pub fn install(
    agent: GlobalHookAgent,
    file: &Path,
    hook: &str,
    is_windows: bool,
) -> io::Result<HookChange> {
    let current = match std::fs::read_to_string(file) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if current.is_none() && !file.parent().is_some_and(Path::is_dir) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} isn't there: is {} installed?",
                file.display(),
                agent.name()
            ),
        ));
    }
    let contents = current.as_deref().unwrap_or_default();
    let updated = agent
        .with_hooks(contents, hook, is_windows)
        .ok_or_else(|| {
            io::Error::other(format!("{} isn't a file Solo can add to", file.display()))
        })?;
    if current.as_deref() == Some(updated.as_str()) {
        return Ok(HookChange::Unchanged);
    }
    let backup = match &current {
        Some(contents) => backup_once(file, contents)?,
        None => None,
    };
    write_atomically(file, &updated)?;
    Ok(HookChange::Written {
        file: file.to_path_buf(),
        backup,
    })
}

/// Takes Solo's hooks out of `agent`'s config file `file`, leaving the rest.
pub fn remove(agent: GlobalHookAgent, file: &Path) -> io::Result<HookChange> {
    let contents = match std::fs::read_to_string(file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HookChange::Unchanged),
        Err(error) => return Err(error),
    };
    if !agent.has_hooks(&contents) {
        return Ok(HookChange::Unchanged);
    }
    let updated = agent.without_hooks(&contents).ok_or_else(|| {
        io::Error::other(format!("{} isn't a file Solo can read", file.display()))
    })?;
    write_atomically(file, &updated)?;
    Ok(HookChange::Written {
        file: file.to_path_buf(),
        backup: None,
    })
}

/// Copies `file` to `<file>.solo-backup` unless there's a backup already.
fn backup_once(file: &Path, contents: &str) -> io::Result<Option<PathBuf>> {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".solo-backup");
    let backup = file.with_file_name(name);
    if backup.exists() {
        return Ok(None);
    }
    std::fs::write(&backup, contents)?;
    Ok(Some(backup))
}

fn write_atomically(file: &Path, contents: &str) -> io::Result<()> {
    let partial = file.with_extension(format!("solo-{}.partial", std::process::id()));
    std::fs::write(&partial, contents)?;
    std::fs::rename(&partial, file).inspect_err(|_| {
        std::fs::remove_file(&partial).ok();
    })
}

/// Brings each agent's config in line with `enabled` (the names in the
/// `global_agent_hooks` setting): Solo's hooks added for the ones listed,
/// taken out of the others. Returns what it did, per agent.
pub fn reconcile(
    enabled: &[String],
    home: &Path,
    var: impl Fn(&str) -> Option<String>,
    hook: Option<&str>,
    is_windows: bool,
) -> Vec<(GlobalHookAgent, io::Result<HookChange>)> {
    GlobalHookAgent::ALL
        .into_iter()
        .map(|agent| {
            let file = agent.config_file(home, &var);
            let wanted = enabled
                .iter()
                .any(|name| GlobalHookAgent::from_name(name) == Some(agent));
            let result = match (wanted, hook) {
                (true, Some(hook)) => install(agent, &file, hook, is_windows),
                (true, None) => Err(io::Error::other("Solo's hook program is missing")),
                (false, _) => remove(agent, &file),
            };
            (agent, result)
        })
        .collect()
}

/// Brings the agents' config in line with `enabled` for this user, with
/// Solo's hook program, and logs what changed.
pub fn apply(enabled: &[String]) {
    let hook = crate::shell_integration::agent_hook_executable()
        .map(|path| path.to_string_lossy().into_owned());
    let home = util::paths::home_dir();
    let results = reconcile(
        enabled,
        home,
        |name| std::env::var(name).ok(),
        hook.as_deref(),
        cfg!(windows),
    );
    for (agent, result) in results {
        match result {
            Ok(HookChange::Unchanged) => {}
            Ok(HookChange::Written { file, backup }) => log::info!(
                "{} status hooks: wrote {}{}",
                agent.name(),
                file.display(),
                backup
                    .map(|backup| format!(" (backed up to {})", backup.display()))
                    .unwrap_or_default()
            ),
            Err(error) => log::warn!("{} status hooks: {error}", agent.name()),
        }
    }
    for name in enabled {
        if GlobalHookAgent::from_name(name).is_none() {
            log::warn!("global_agent_hooks: unknown agent {name:?} (known: kimi, mastracode)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: &str = r"C:\Program Files\Solo\solo-agent-hook.exe";

    fn temp_home(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("solo-global-hooks-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn kimi_hooks_are_added_and_removed_cleanly() {
        let home = temp_home("kimi");
        let dir = home.join(".kimi-code");
        std::fs::create_dir_all(&dir).unwrap();
        let file = GlobalHookAgent::Kimi.config_file(&home, |_| None);
        assert_eq!(file, dir.join("config.toml"));
        let original = "default_model = \"kimi-k2\"\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n";
        std::fs::write(&file, original).unwrap();

        let change = install(GlobalHookAgent::Kimi, &file, HOOK, true).unwrap();
        let backup = dir.join("config.toml.solo-backup");
        assert_eq!(
            change,
            HookChange::Written {
                file: file.clone(),
                backup: Some(backup.clone())
            }
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        let installed = std::fs::read_to_string(&file).unwrap();
        assert!(installed.starts_with(original.trim_end()));
        let parsed: toml::Value = toml::from_str(&installed).unwrap();
        let hooks = parsed["hooks"].as_array().unwrap();
        // Theirs, then Solo's.
        assert_eq!(hooks[0]["command"].as_str(), Some("notify-send done"));
        let solo: Vec<_> = hooks[1..].iter().collect();
        assert_eq!(solo.len(), GlobalHookAgent::Kimi.hooks().len());
        assert_eq!(
            solo[0]["command"].as_str(),
            Some(r#""C:\Program Files\Solo\solo-agent-hook.exe" start --agent kimi"#)
        );
        assert_eq!(solo[2]["matcher"].as_str(), Some("^AskUserQuestion$"));
        // Again: nothing to do.
        assert_eq!(
            install(GlobalHookAgent::Kimi, &file, HOOK, true).unwrap(),
            HookChange::Unchanged
        );

        remove(GlobalHookAgent::Kimi, &file).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert_eq!(
            remove(GlobalHookAgent::Kimi, &file).unwrap(),
            HookChange::Unchanged
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn kimi_home_follows_its_variable() {
        let home = Path::new("/home/me");
        let file = GlobalHookAgent::Kimi.config_file(home, |name| {
            (name == "KIMI_CODE_HOME").then(|| "/data/kimi".to_string())
        });
        assert_eq!(file, Path::new("/data/kimi").join("config.toml"));
    }

    #[test]
    fn mastracode_hooks_are_added_and_removed_cleanly() {
        let home = temp_home("mastracode");
        std::fs::create_dir_all(home.join(".mastracode")).unwrap();
        let file = GlobalHookAgent::MastraCode.config_file(&home, |_| None);
        let original = "{\n  \"Stop\": [\n    {\n      \"type\": \"command\",\n      \"command\": \"say done\"\n    }\n  ]\n}\n";
        std::fs::write(&file, original).unwrap();

        install(
            GlobalHookAgent::MastraCode,
            &file,
            "/opt/solo/solo-agent-hook",
            false,
        )
        .unwrap();
        let installed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(installed["Stop"][0]["command"], "say done");
        assert_eq!(
            installed["Stop"][1]["command"],
            "/opt/solo/solo-agent-hook stop --agent mastracode"
        );
        assert_eq!(
            installed["PermissionRequest"][0]["command"],
            "/opt/solo/solo-agent-hook input --agent mastracode"
        );
        assert_eq!(installed["AgentStart"][0]["timeout"], 10_000);

        remove(GlobalHookAgent::MastraCode, &file).unwrap();
        let removed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            removed,
            serde_json::from_str::<serde_json::Value>(original).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(home.join(".mastracode").join("hooks.json.solo-backup"))
                .unwrap(),
            original
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn reconcile_only_touches_listed_agents_and_removes_the_rest() {
        let home = temp_home("reconcile");
        // Neither agent installed: listing them fails without making folders,
        // and not listing them writes nothing.
        let results = reconcile(&[], &home, |_| None, Some(HOOK), true);
        assert!(
            results
                .iter()
                .all(|(_, result)| matches!(result, Ok(HookChange::Unchanged)))
        );
        let results = reconcile(&["kimi".into()], &home, |_| None, Some(HOOK), true);
        assert!(results[0].1.is_err());
        assert!(!home.join(".kimi-code").exists());

        // Kimi installed with no config yet: one is made; MastraCode left alone.
        std::fs::create_dir_all(home.join(".kimi-code")).unwrap();
        std::fs::create_dir_all(home.join(".mastracode")).unwrap();
        let results = reconcile(&["Kimi".into()], &home, |_| None, Some(HOOK), true);
        assert!(matches!(
            results[0].1,
            Ok(HookChange::Written { backup: None, .. })
        ));
        assert!(matches!(results[1].1, Ok(HookChange::Unchanged)));
        assert!(!home.join(".mastracode").join("hooks.json").exists());
        let config = home.join(".kimi-code").join("config.toml");
        assert!(GlobalHookAgent::Kimi.has_hooks(&std::fs::read_to_string(&config).unwrap()));

        // Off the list: taken out again.
        reconcile(&[], &home, |_| None, Some(HOOK), true);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn unreadable_files_are_left_alone() {
        let home = temp_home("broken");
        std::fs::create_dir_all(home.join(".mastracode")).unwrap();
        let file = home.join(".mastracode").join("hooks.json");
        std::fs::write(&file, "{ not json").unwrap();
        assert!(install(GlobalHookAgent::MastraCode, &file, HOOK, true).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
        std::fs::remove_dir_all(&home).ok();
    }
}
