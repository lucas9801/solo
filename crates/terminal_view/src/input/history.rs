//! Command history for the terminal input: the shell's history file plus the
//! commands submitted in this session.

use std::path::{Path, PathBuf};

/// Entries kept in memory; older ones are dropped when loading.
const MAX_ENTRIES: usize = 10_000;

#[derive(Default)]
pub struct CommandHistory {
    /// Oldest first, without duplicates (a repeated command moves to the end).
    entries: Vec<String>,
    navigation: Option<Navigation>,
}

/// An in-progress walk through the history with the up and down keys.
struct Navigation {
    /// What was typed before navigating; only entries starting with it match.
    prefix: String,
    /// Index into `entries` of the entry shown, `None` back at the typed text.
    index: Option<usize>,
}

impl CommandHistory {
    pub fn new(entries: Vec<String>) -> Self {
        let mut history = Self::default();
        for entry in entries {
            history.push(entry);
        }
        history
    }

    pub fn push(&mut self, entry: String) {
        let entry = entry.trim_end().to_string();
        if entry.trim().is_empty() {
            return;
        }
        self.entries.retain(|existing| *existing != entry);
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }

    /// All entries, newest first.
    pub fn newest_first(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().rev().map(String::as_str)
    }

    pub fn into_entries(self) -> Vec<String> {
        self.entries
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The most recent entry that extends `prefix`, for showing as a ghost
    /// suggestion while typing.
    pub fn suggestion(&self, prefix: &str) -> Option<&str> {
        if prefix.trim().is_empty() {
            return None;
        }
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.len() > prefix.len() && entry.starts_with(prefix))
            .map(String::as_str)
    }

    /// Steps to an older entry matching what was typed (`current` when the walk
    /// starts). Returns the text to show, or `None` when there is nothing older.
    pub fn previous(&mut self, current: &str) -> Option<String> {
        let navigation = self.navigation.get_or_insert_with(|| Navigation {
            prefix: current.to_string(),
            index: None,
        });
        let end = navigation.index.unwrap_or(self.entries.len());
        let index = self.entries[..end]
            .iter()
            .rposition(|entry| entry.starts_with(&navigation.prefix) && entry != current)?;
        navigation.index = Some(index);
        Some(self.entries[index].clone())
    }

    /// Steps to a newer entry, ending back at the text typed before the walk.
    pub fn next(&mut self) -> Option<String> {
        let navigation = self.navigation.as_mut()?;
        let start = navigation.index? + 1;
        match self.entries[start..]
            .iter()
            .position(|entry| entry.starts_with(&navigation.prefix))
        {
            Some(offset) => {
                navigation.index = Some(start + offset);
                Some(self.entries[start + offset].clone())
            }
            None => {
                let prefix = navigation.prefix.clone();
                self.navigation = None;
                Some(prefix)
            }
        }
    }

    /// Ends a walk, e.g. because the user edited the text.
    pub fn reset_navigation(&mut self) {
        self.navigation = None;
    }

    pub fn is_navigating(&self) -> bool {
        self.navigation.is_some()
    }
}

/// Reads the history file of the given shell, oldest entry first.
pub fn load_shell_history(shell: Option<&str>) -> Vec<String> {
    let Some(shell) = shell else {
        return Vec::new();
    };
    let name = Path::new(shell)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (path, format) = match name.as_str() {
        "powershell" | "pwsh" => (powershell_history_path(), HistoryFormat::PowerShell),
        "bash" => (
            home_dir().map(|home| home.join(".bash_history")),
            HistoryFormat::Plain,
        ),
        "zsh" => (
            home_dir().map(|home| home.join(".zsh_history")),
            HistoryFormat::Zsh,
        ),
        _ => (None, HistoryFormat::Plain),
    };
    let Some(contents) = path.and_then(|path| std::fs::read(path).ok()) else {
        return Vec::new();
    };
    parse_history(&String::from_utf8_lossy(&contents), format)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HistoryFormat {
    Plain,
    /// PSReadLine: a line ending in a backtick continues on the next line.
    PowerShell,
    /// Extended zsh history: `: <time>:<duration>;<command>`.
    Zsh,
}

pub fn parse_history(contents: &str, format: HistoryFormat) -> Vec<String> {
    let mut entries = Vec::new();
    let mut pending = String::new();
    for line in contents.lines() {
        match format {
            HistoryFormat::Plain => entries.push(line.to_string()),
            HistoryFormat::PowerShell => {
                if let Some(continued) = line.strip_suffix('`') {
                    pending.push_str(continued);
                    pending.push('\n');
                } else {
                    pending.push_str(line);
                    entries.push(std::mem::take(&mut pending));
                }
            }
            HistoryFormat::Zsh => {
                let command = line
                    .strip_prefix(": ")
                    .and_then(|rest| rest.split_once(';'))
                    .map_or(line, |(_, command)| command);
                entries.push(command.to_string());
            }
        }
    }
    let skip = entries.len().saturating_sub(MAX_ENTRIES);
    entries.into_iter().skip(skip).collect()
}

fn powershell_history_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|app_data| {
            PathBuf::from(app_data)
                .join("Microsoft/Windows/PowerShell/PSReadLine/ConsoleHost_history.txt")
        })
    } else {
        home_dir()
            .map(|home| home.join(".local/share/powershell/PSReadLine/ConsoleHost_history.txt"))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_navigation_filters_by_prefix() {
        let mut history = CommandHistory::new(vec![
            "git status".into(),
            "cargo build".into(),
            "git commit".into(),
            "git status".into(),
        ]);
        // The repeated "git status" moved to the end.
        assert_eq!(history.len(), 3);
        assert_eq!(history.previous("git").as_deref(), Some("git status"));
        assert_eq!(history.previous("git").as_deref(), Some("git commit"));
        assert_eq!(history.previous("git"), None);
        assert_eq!(history.next().as_deref(), Some("git status"));
        assert_eq!(history.next().as_deref(), Some("git"));
        assert!(!history.is_navigating());
    }

    #[test]
    fn test_suggestion() {
        let history = CommandHistory::new(vec!["cargo build".into(), "cargo test".into()]);
        assert_eq!(history.suggestion("cargo"), Some("cargo test"));
        assert_eq!(history.suggestion("cargo b"), Some("cargo build"));
        assert_eq!(history.suggestion("cargo test"), None);
        assert_eq!(history.suggestion(""), None);
    }

    #[test]
    fn test_parse_history() {
        assert_eq!(
            parse_history(
                "dir\nforeach ($a in 1..2) {`\n  $a`\n}\nls",
                HistoryFormat::PowerShell
            ),
            vec!["dir", "foreach ($a in 1..2) {\n  $a\n}", "ls"]
        );
        assert_eq!(
            parse_history(": 1700000000:0;git status\nls", HistoryFormat::Zsh),
            vec!["git status", "ls"]
        );
    }
}
