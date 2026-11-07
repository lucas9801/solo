//! Command history for the terminal input: the shell's history file plus the
//! commands submitted in this session.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// Entries kept in memory; older ones are dropped when loading.
const MAX_ENTRIES: usize = 10_000;
/// Matching entries, newest first, that a ghost suggestion is chosen from.
const MAX_SUGGESTION_CANDIDATES: usize = 200;
/// How many times a command has to have followed the last one to be
/// suggested before anything is typed.
const MIN_NEXT_COMMAND_COUNT: u32 = 3;

#[derive(Default)]
pub struct CommandHistory {
    /// Oldest first, without duplicates (a repeated command moves to the end).
    entries: Vec<String>,
    navigation: Option<Navigation>,
    /// SOLO: how many times each command ran.
    uses: HashMap<String, u32>,
    /// SOLO: how many times each command ran right after another one.
    followers: HashMap<String, HashMap<String, u32>>,
    /// SOLO: the command run last.
    last: Option<String>,
}

/// SOLO: what this session's commands tell about where a suggestion would run.
#[derive(Default)]
pub struct SuggestionContext {
    /// The shell's working directory; `cd` commands are only suggested when
    /// their directory is there.
    pub cwd: Option<PathBuf>,
    /// Commands run in `cwd` before.
    pub run_here: HashSet<String>,
    /// Commands whose last run failed.
    pub failed: HashSet<String>,
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
        *self.uses.entry(entry.clone()).or_default() += 1;
        if let Some(last) = self.last.take() {
            *self
                .followers
                .entry(last)
                .or_default()
                .entry(entry.clone())
                .or_default() += 1;
        }
        self.last = Some(entry.clone());
        self.entries.retain(|existing| *existing != entry);
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let dropped = self.entries.remove(0);
            self.uses.remove(&dropped);
            self.followers.remove(&dropped);
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

    #[cfg(test)]
    pub fn suggestion(&self, prefix: &str) -> Option<&str> {
        self.suggestion_in(prefix, &SuggestionContext::default())
    }

    /// The entry that extends `prefix` most likely to be wanted, for showing
    /// as a ghost suggestion while typing: the command usually run after the
    /// last one, then often run ones, then recent ones. With nothing typed,
    /// the command that usually comes next, once it has a few times.
    /// SOLO: from what `context` says, a `cd` to a directory that isn't there
    /// is left out, a command run in this directory before is preferred and
    /// one that failed last time isn't.
    pub fn suggestion_in(&self, prefix: &str, context: &SuggestionContext) -> Option<&str> {
        let followers = self.last.as_ref().and_then(|last| self.followers.get(last));
        let applies = |entry: &str| can_change_directory(entry, context.cwd.as_deref());
        if prefix.trim().is_empty() {
            return followers?
                .iter()
                .filter(|(command, count)| {
                    **count >= MIN_NEXT_COMMAND_COUNT
                        && !context.failed.contains(command.as_str())
                        && applies(command)
                })
                .max_by_key(|(command, count)| (**count, self.recency(command)))
                .map(|(command, _)| command.as_str());
        }
        self.entries
            .iter()
            .rev()
            .filter(|entry| entry.len() > prefix.len() && entry.starts_with(prefix))
            .take(MAX_SUGGESTION_CANDIDATES)
            .filter(|entry| applies(entry))
            .enumerate()
            .max_by(|(a_rank, a), (b_rank, b)| {
                let score = |rank: usize, entry: &str| {
                    let follows = followers
                        .and_then(|followers| followers.get(entry))
                        .copied()
                        .unwrap_or(0);
                    let uses = self.uses.get(entry).copied().unwrap_or(1);
                    let here = if context.run_here.contains(entry) {
                        4.0
                    } else {
                        0.0
                    };
                    let failed = if context.failed.contains(entry) {
                        6.0
                    } else {
                        0.0
                    };
                    // Following the last command counts most, then having
                    // run here; how often it ran grows slowly; the newest
                    // match gets a head start that fades with each newer one.
                    follows.min(10) as f32 * 3.0 + here - failed
                        + (uses as f32).ln()
                        + 3.0 / (rank as f32 + 1.0)
                };
                score(*a_rank, a)
                    .total_cmp(&score(*b_rank, b))
                    // Ties go to the newer one.
                    .then(b_rank.cmp(a_rank))
            })
            .map(|(_, entry)| entry.as_str())
    }

    /// Higher for newer entries.
    fn recency(&self, command: &str) -> usize {
        self.entries
            .iter()
            .rposition(|entry| entry == command)
            .unwrap_or(0)
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

/// SOLO: whether `command` can run in `cwd` as far as changing directory
/// goes: `cd godot_engine` can't inside `godot_engine` itself, where there's
/// no `godot_engine` to go into. Commands that aren't a plain `cd` can.
fn can_change_directory(command: &str, cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return true;
    };
    let mut words = command.trim().splitn(2, char::is_whitespace);
    let Some(program) = words.next() else {
        return true;
    };
    const CHANGE_DIRECTORY: &[&str] = &["cd", "chdir", "pushd", "set-location", "sl"];
    if !CHANGE_DIRECTORY.contains(&program.to_lowercase().as_str()) {
        return true;
    }
    let target = words.next().unwrap_or_default().trim();
    let target = target.trim_matches(|c| c == '"' || c == '\'');
    let unknown = target.is_empty()
        || target.starts_with(['-', '~'])
        || target.contains(['$', '%', '&', ';', '|', '*', '?', '`']);
    unknown || cwd.join(target).is_dir()
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
    fn test_suggestion_prefers_frequent_and_following_commands() {
        let mut history = CommandHistory::new(Vec::new());
        for _ in 0..5 {
            history.push("git add .".into());
            history.push("git commit".into());
        }
        history.push("git status".into());
        // Run often beats run last.
        assert_eq!(history.suggestion("git"), Some("git commit"));
        history.push("git add .".into());
        // "git commit" always followed "git add .".
        assert_eq!(history.suggestion("git"), Some("git commit"));
        assert_eq!(history.suggestion(""), Some("git commit"));
        history.push("cargo build".into());
        // Nothing usually follows "cargo build".
        assert_eq!(history.suggestion(""), None);
        assert_eq!(history.suggestion("git"), Some("git add ."));
    }

    #[test]
    fn test_suggestion_in_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("app")).unwrap();
        let history = CommandHistory::new(vec![
            "cd app".into(),
            "cd missing".into(),
            "cargo build".into(),
            "cargo test".into(),
        ]);
        let mut context = SuggestionContext {
            cwd: Some(root.path().to_path_buf()),
            ..Default::default()
        };
        assert_eq!(history.suggestion_in("cd ", &context), Some("cd app"));
        context.cwd = Some(root.path().join("app"));
        assert_eq!(history.suggestion_in("cd ", &context), None);
        assert_eq!(history.suggestion_in("cargo", &context), Some("cargo test"));
        context.run_here.insert("cargo build".into());
        assert_eq!(
            history.suggestion_in("cargo", &context),
            Some("cargo build")
        );
        context.failed.insert("cargo build".into());
        assert_eq!(history.suggestion_in("cargo", &context), Some("cargo test"));
        assert!(can_change_directory("cd -", context.cwd.as_deref()));
        assert!(can_change_directory("cd ..", context.cwd.as_deref()));
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
