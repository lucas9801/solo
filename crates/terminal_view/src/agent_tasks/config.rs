//! SOLO: a repository's settings for its agent tasks, in `.solo/tasks.json`:
//! where their worktrees go, the ignored files each gets a copy of, a setup
//! command run in it, a port of its own, environment variables, and a
//! teardown command run when it's deleted.
#![allow(
    clippy::disallowed_methods,
    reason = "runs the teardown command synchronously, only from background tasks"
)]

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use serde::Deserialize;

/// The file, relative to the repository's main worktree.
pub const CONFIG_PATH: &str = ".solo/tasks.json";
/// The environment variable with a task's port.
pub const PORT_ENV: &str = "SOLO_TASK_PORT";
const MAX_COPIED_FILES: usize = 1000;
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// Where task worktrees go, relative to the repository's main worktree;
    /// `{repo}` is the repository's folder name.
    pub worktree_directory: Option<String>,
    /// Files to copy from the main worktree into each new task's, as paths
    /// relative to it whose file names may have `*` and `?` wildcards.
    pub copy: Vec<String>,
    /// Run in a new task's terminal before its agent starts.
    pub setup: Option<String>,
    /// Run in a task's worktree before it's deleted.
    pub teardown: Option<String>,
    /// The ports tasks get one of each, in `SOLO_TASK_PORT`.
    pub port_range: Option<[u16; 2]>,
    /// Environment variables for tasks' terminals.
    pub env: BTreeMap<String, String>,
}

impl TasksConfig {
    pub fn parse(text: &str) -> Result<Self> {
        let config: TasksConfig =
            serde_json_lenient::from_str(text).context("reading .solo/tasks.json")?;
        if let Some([first, last]) = config.port_range {
            ensure!(
                first > 0 && first <= last,
                "port_range [{first}, {last}] is empty"
            );
        }
        for name in config.env.keys() {
            ensure!(
                !name.is_empty() && !name.contains(['=', '\0']),
                "{name:?} isn't an environment variable's name"
            );
        }
        Ok(config)
    }

    /// The repository's config, `None` if it has none.
    pub fn load(main_root: &Path) -> Result<Option<Self>> {
        let path = main_root.join(CONFIG_PATH);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Where task worktrees go: next to the main worktree at most, so a
    /// repository can't have them made anywhere on the machine.
    pub fn worktrees_directory(&self, main_root: &Path) -> Result<Option<PathBuf>> {
        let Some(directory) = &self.worktree_directory else {
            return Ok(None);
        };
        let repo = main_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = PathBuf::from(directory.replace("{repo}", &repo));
        ensure!(
            relative.is_relative(),
            "worktree_directory {directory:?} must be relative to the repository"
        );
        let mut resolved = main_root.to_path_buf();
        for component in relative.components() {
            match component {
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(part) => resolved.push(part),
                Component::CurDir => {}
                _ => bail!("worktree_directory {directory:?} must be relative"),
            }
        }
        let allowed = main_root.parent().unwrap_or(main_root);
        ensure!(
            resolved.starts_with(allowed) && resolved != allowed,
            "worktree_directory {directory:?} goes outside the repository's parent folder"
        );
        Ok(Some(resolved))
    }

    /// The environment for a task's terminal.
    pub fn task_env(&self, port: Option<u16>) -> collections::HashMap<String, String> {
        let mut env: collections::HashMap<String, String> = self
            .env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if let Some(port) = port {
            env.insert(PORT_ENV.to_string(), port.to_string());
        }
        env
    }
}

/// The lowest port of `range` no other task uses.
pub fn free_port(range: [u16; 2], used: &[u16]) -> Option<u16> {
    (range[0]..=range[1]).find(|port| !used.contains(port))
}

/// Whether `name` matches `pattern`, whose `*` stands for any characters and
/// `?` for one.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            backtrack = Some((p, n));
            p += 1;
        } else if let Some((star, matched)) = backtrack {
            p = star + 1;
            n = matched + 1;
            backtrack = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Copies the files `patterns` name from `from` into `to`, keeping their
/// paths, without overwriting files already there. Returns what it copied.
pub fn copy_files(patterns: &[String], from: &Path, to: &Path) -> Result<Vec<PathBuf>> {
    let mut copied = Vec::new();
    for pattern in patterns {
        let relative = Path::new(pattern);
        ensure!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
            "copy pattern {pattern:?} must stay inside the repository"
        );
        let Some(file_pattern) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let directory = relative.parent().unwrap_or(Path::new(""));
        ensure!(
            !directory.to_string_lossy().contains(['*', '?']),
            "copy pattern {pattern:?}: wildcards only go in the file name"
        );
        let Ok(entries) = std::fs::read_dir(from.join(directory)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !entry.file_type().is_ok_and(|kind| kind.is_file())
                || !wildcard_match(file_pattern, name)
            {
                continue;
            }
            let relative = directory.join(name);
            let target = to.join(&relative);
            if target.exists() {
                continue;
            }
            ensure!(
                copied.len() < MAX_COPIED_FILES,
                "copy patterns match more than {MAX_COPIED_FILES} files"
            );
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", relative.display()))?;
            copied.push(relative);
        }
    }
    Ok(copied)
}

/// Runs `command` in `directory` with the system shell, failing on a
/// non-zero exit or after two minutes.
pub fn run_teardown(command: &str, directory: &Path) -> Result<()> {
    let mut process = if cfg!(windows) {
        let mut process = util::command::new_std_command("cmd.exe");
        process.args(["/D", "/S", "/C", command]);
        process
    } else {
        let mut process = util::command::new_std_command("sh");
        process.args(["-c", command]);
        process
    };
    let mut child = process
        .current_dir(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("starting the teardown {command:?}"))?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(anyhow!("the teardown {command:?} failed ({status})"));
        }
        if started.elapsed() > TEARDOWN_TIMEOUT {
            child.kill().ok();
            bail!("the teardown {command:?} didn't finish in two minutes");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_configs() {
        let config = TasksConfig::parse(
            r#"{
                // Comments are fine.
                "worktree_directory": "../{repo}-worktrees",
                "copy": [".env", ".env.*"],
                "setup": "npm install",
                "teardown": "docker compose down",
                "port_range": [4100, 4199],
                "env": { "NODE_ENV": "development" },
            }"#,
        )
        .unwrap();
        assert_eq!(config.setup.as_deref(), Some("npm install"));
        assert_eq!(config.port_range, Some([4100, 4199]));
        let env = config.task_env(Some(4100));
        assert_eq!(env.get(PORT_ENV).map(String::as_str), Some("4100"));
        assert_eq!(env.get("NODE_ENV").map(String::as_str), Some("development"));

        assert_eq!(TasksConfig::parse("{}").unwrap(), TasksConfig::default());
        for bad in [
            r#"{ "setpu": "x" }"#,
            r#"{ "port_range": [5, 4] }"#,
            r#"{ "port_range": [0, 4] }"#,
            r#"{ "env": { "A=B": "x" } }"#,
            "not json",
        ] {
            assert!(TasksConfig::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn worktree_directories_stay_near_the_repository() {
        let root = if cfg!(windows) {
            Path::new(r"C:\code\app")
        } else {
            Path::new("/code/app")
        };
        let directory = |value: &str| {
            TasksConfig {
                worktree_directory: Some(value.to_string()),
                ..TasksConfig::default()
            }
            .worktrees_directory(root)
        };
        assert_eq!(
            directory("../{repo}-worktrees").unwrap(),
            Some(root.parent().unwrap().join("app-worktrees"))
        );
        assert_eq!(
            directory(".solo/worktrees").unwrap(),
            Some(root.join(".solo").join("worktrees"))
        );
        for bad in ["../../elsewhere", "..", "/tmp/x", r"C:\x"] {
            assert!(directory(bad).is_err(), "{bad}");
        }
        assert_eq!(
            TasksConfig::default().worktrees_directory(root).unwrap(),
            None
        );
    }

    #[test]
    fn ports_are_given_out_once() {
        assert_eq!(free_port([4100, 4102], &[]), Some(4100));
        assert_eq!(free_port([4100, 4102], &[4100, 4101]), Some(4102));
        assert_eq!(free_port([4100, 4101], &[4100, 4101]), None);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match(".env", ".env"));
        assert!(wildcard_match(".env.*", ".env.local"));
        assert!(!wildcard_match(".env.*", ".env"));
        assert!(wildcard_match("local.?", "local.a"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn copies_ignored_files_into_tasks() {
        let temp = tempfile::tempdir().unwrap();
        let (from, to) = (temp.path().join("main"), temp.path().join("task"));
        std::fs::create_dir_all(from.join("config")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join(".env"), "SECRET=1\n").unwrap();
        std::fs::write(from.join(".env.local"), "LOCAL=1\n").unwrap();
        std::fs::write(from.join("config/local.json"), "{}").unwrap();
        std::fs::write(from.join("config/shared.json"), "{}").unwrap();
        // Already in the task: kept.
        std::fs::write(to.join(".env.local"), "MINE=1\n").unwrap();

        let mut copied = copy_files(
            &[
                ".env".into(),
                ".env.*".into(),
                "config/local.*".into(),
                "missing/*".into(),
            ],
            &from,
            &to,
        )
        .unwrap();
        copied.sort();
        assert_eq!(
            copied,
            vec![
                PathBuf::from(".env"),
                Path::new("config").join("local.json")
            ]
        );
        assert_eq!(
            std::fs::read_to_string(to.join(".env.local")).unwrap(),
            "MINE=1\n"
        );
        assert!(!to.join("config/shared.json").exists());

        for bad in ["../outside", "/etc/passwd", "*/x"] {
            assert!(copy_files(&[bad.into()], &from, &to).is_err(), "{bad}");
        }
    }

    #[test]
    fn teardowns_run_in_the_worktree() {
        let temp = tempfile::tempdir().unwrap();
        run_teardown("echo done > teardown.txt", temp.path()).unwrap();
        assert!(temp.path().join("teardown.txt").exists());
        assert!(run_teardown("exit 3", temp.path()).is_err());
    }
}
