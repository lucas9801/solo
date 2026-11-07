//! SOLO: the terminal tab's title, from what runs in it.
//!
//! In order: the title the user gave the tab; a running agent's name with its
//! session title (which agents like Claude Code set with OSC 0/2); the running
//! command; or, idle at the prompt, the directory with its git branch. Tabs
//! with the same title add parent directories until they differ.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use gpui::App;
use util::truncate_and_trailoff;

use crate::{TerminalView, agent_status::AgentStatus};

const MAX_COMMAND_CHARS: usize = 30;
const MAX_SESSION_TITLE_CHARS: usize = 30;
const MAX_DIRECTORY_CHARS: usize = 40;

/// What the title is made of.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TitleSource<'a> {
    pub custom_title: Option<&'a str>,
    /// The running agent's name, with the title it gave its session.
    pub agent: Option<(&'a str, Option<&'a str>)>,
    pub running_command: Option<&'a str>,
    pub working_directory: Option<&'a Path>,
    pub git_branch: Option<&'a str>,
}

/// The tab title, or `None` when nothing is known to make one from. `detail`
/// is how many parent directories to add to tell tabs with the same title
/// apart.
pub(crate) fn tab_title(source: &TitleSource, detail: usize, truncate: bool) -> Option<String> {
    let truncated = |text: &str, max_chars: usize| {
        if truncate {
            truncate_and_trailoff(text, max_chars)
        } else {
            text.to_string()
        }
    };
    if let Some(custom_title) = source.custom_title.filter(|title| !title.trim().is_empty()) {
        return Some(custom_title.to_string());
    }

    let activity = if let Some((name, session_title)) = source.agent {
        Some(match session_title {
            Some(session_title) if session_title.to_lowercase().contains(&name.to_lowercase()) => {
                truncated(session_title, MAX_SESSION_TITLE_CHARS)
            }
            Some(session_title) => format!(
                "{name} · {}",
                truncated(session_title, MAX_SESSION_TITLE_CHARS)
            ),
            None => name.to_string(),
        })
    } else {
        source
            .running_command
            .map(|command| truncated(command, MAX_COMMAND_CHARS))
    };

    let directory = source
        .working_directory
        .map(|directory| truncated(&last_components(directory, detail + 1), MAX_DIRECTORY_CHARS));
    match (activity, directory) {
        (Some(activity), Some(directory)) if detail > 0 => {
            Some(format!("{activity} — {directory}"))
        }
        (Some(activity), _) => Some(activity),
        (None, Some(directory)) => Some(match source.git_branch {
            Some(branch) => format!("{directory} ({branch})"),
            None => directory,
        }),
        (None, None) => None,
    }
}

/// The last `count` names of `path`, or the whole path if it has none (a
/// drive or root).
fn last_components(path: &Path, count: usize) -> String {
    let names = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if names.is_empty() {
        return path.to_string_lossy().into_owned();
    }
    names[names.len().saturating_sub(count)..].join(std::path::MAIN_SEPARATOR_STR)
}

/// The session title in a title a program set, without the status symbol
/// agents put in front (Claude Code's "✳ " or spinner).
pub(crate) fn session_title(program_title: &str) -> Option<&str> {
    let title = program_title.trim();
    let mut chars = title.chars();
    let title = match chars.next() {
        Some(symbol)
            if !symbol.is_alphanumeric()
                && (chars.as_str().is_empty() || chars.as_str().starts_with(' ')) =>
        {
            chars.as_str().trim_start()
        }
        _ => title,
    };
    // Windows' console titles itself with the path of the program it runs.
    let is_program_path = matches!(
        title.as_bytes(),
        [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic()
    ) || title.starts_with("\\\\");
    (!title.is_empty() && !is_program_path).then_some(title)
}

/// What the title needs that's cached rather than read on every render.
#[derive(Default)]
pub(crate) struct TitleState {
    /// The git branch of a directory, and the last command finished when it
    /// was read, since a command may switch branches.
    git_branch: Option<(PathBuf, Option<Instant>, Option<String>)>,
    /// When a program last set the terminal's title.
    program_title_at: Option<Instant>,
}

impl TerminalView {
    /// The tab title; see the module docs.
    pub(crate) fn smart_title(&self, detail: usize, truncate: bool, cx: &App) -> String {
        let terminal = self.terminal.read(cx);
        if terminal.task().is_some() {
            return terminal.title(truncate);
        }
        let integration = terminal.shell_integration();
        let running_block = integration
            .blocks()
            .last()
            .filter(|block| block.is_running());
        let running_command = running_block
            .and_then(|block| block.command.as_deref())
            .map(str::trim)
            .filter(|command| !command.is_empty());
        let agent = self.running_agent().map(|name| {
            // A title set before the agent started is someone else's.
            let set_by_agent = match running_block.and_then(|block| block.started_at) {
                Some(started_at) => self
                    .tab_title
                    .program_title_at
                    .is_some_and(|set_at| set_at >= started_at),
                None => true,
            };
            let session_title = set_by_agent
                .then(|| session_title(&terminal.breadcrumb_text))
                .flatten();
            (name, session_title)
        });
        let working_directory = self.title_working_directory(cx);
        // Without shell integration, it isn't known whether the shell is idle.
        let working_directory = working_directory.filter(|_| integration.is_active());
        let source = TitleSource {
            custom_title: self.custom_title.as_deref(),
            agent,
            running_command,
            working_directory: working_directory.as_deref(),
            git_branch: self
                .tab_title
                .git_branch
                .as_ref()
                .filter(|(directory, _, _)| Some(directory) == working_directory.as_ref())
                .and_then(|(_, _, branch)| branch.as_deref()),
        };
        tab_title(&source, detail, truncate)
            .unwrap_or_else(|| terminal.title(truncate && detail == 0))
    }

    /// The tab's tooltip lines below its title: the full command, directory
    /// and branch.
    pub(crate) fn title_details(&self, cx: &App) -> Vec<String> {
        let terminal = self.terminal.read(cx);
        let mut details = Vec::new();
        if let Some(command) = terminal
            .shell_integration()
            .blocks()
            .last()
            .filter(|block| block.is_running())
            .and_then(|block| block.command.as_deref())
        {
            details.push(format!(
                "{}{}",
                util::i18n::tr("Command: ", "命令："),
                command.trim()
            ));
        }
        if let Some(directory) = self.title_working_directory(cx) {
            details.push(format!(
                "{}{}",
                util::i18n::tr("Directory: ", "目录："),
                directory.display()
            ));
            if let Some((_, _, Some(branch))) = self
                .tab_title
                .git_branch
                .as_ref()
                .filter(|(cached, _, _)| *cached == directory)
            {
                details.push(format!("{}{branch}", util::i18n::tr("Branch: ", "分支：")));
            }
        }
        // SOLO: the agent session's usage, from its records.
        if let Some(session) = self.agent_session() {
            let session = session.read(cx);
            details.extend(crate::agent_session::session_tooltip_lines(
                session.state(),
                session.location().matched_by,
                util::i18n::is_chinese(),
            ));
        }
        details
    }

    fn running_agent(&self) -> Option<&str> {
        self.agent_status().and_then(|(name, status)| match status {
            AgentStatus::Working
            | AgentStatus::NeedsInput
            | AgentStatus::Idle
            | AgentStatus::Finished => Some(name),
            AgentStatus::Done | AgentStatus::Failed => None,
        })
    }

    fn title_working_directory(&self, cx: &App) -> Option<PathBuf> {
        let terminal = self.terminal.read(cx);
        terminal
            .shell_integration()
            .working_directory()
            .cloned()
            .or_else(|| terminal.working_directory())
    }

    /// SOLO: the git branch last read for the terminal's directory.
    pub(crate) fn cached_git_branch(&self, cx: &App) -> Option<&str> {
        let directory = self.title_working_directory(cx)?;
        self.tab_title
            .git_branch
            .as_ref()
            .filter(|(cached, _, _)| *cached == directory)
            .and_then(|(_, _, branch)| branch.as_deref())
    }

    /// A program set the terminal's title.
    pub(crate) fn program_title_changed(&mut self) {
        self.tab_title.program_title_at = Some(Instant::now());
    }

    /// Reads the git branch again when the directory changed or a command
    /// finished since it was read.
    pub(crate) fn update_title_git_branch(&mut self, cx: &App) {
        let Some(directory) = self.title_working_directory(cx) else {
            self.tab_title.git_branch = None;
            return;
        };
        let last_finished = self
            .terminal
            .read(cx)
            .shell_integration()
            .blocks()
            .iter()
            .rev()
            .find_map(|block| block.finished_at);
        if self
            .tab_title
            .git_branch
            .as_ref()
            .is_some_and(|(cached, finished, _)| *cached == directory && *finished == last_finished)
        {
            return;
        }
        let branch = crate::input::git_branch(&directory);
        self.tab_title.git_branch = Some((directory, last_finished, branch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(components: &[&str]) -> PathBuf {
        let mut path = if cfg!(windows) {
            PathBuf::from("C:\\")
        } else {
            PathBuf::from("/")
        };
        path.extend(components);
        path
    }

    fn joined(components: &[&str]) -> String {
        components.join(std::path::MAIN_SEPARATOR_STR)
    }

    #[test]
    fn test_custom_title_comes_first() {
        let directory = path(&["code", "solo"]);
        let source = TitleSource {
            custom_title: Some("server"),
            agent: Some(("claude", Some("Fix the build"))),
            running_command: Some("claude"),
            working_directory: Some(&directory),
            git_branch: Some("main"),
        };
        assert_eq!(tab_title(&source, 0, true).as_deref(), Some("server"));
        assert_eq!(tab_title(&source, 2, true).as_deref(), Some("server"));

        let blank = TitleSource {
            custom_title: Some("  "),
            ..source
        };
        assert_eq!(
            tab_title(&blank, 0, true).as_deref(),
            Some("claude · Fix the build")
        );
    }

    #[test]
    fn test_agent_title() {
        let directory = path(&["code", "solo"]);
        let source = TitleSource {
            agent: Some(("claude", Some("Fix the build"))),
            running_command: Some("claude --resume"),
            working_directory: Some(&directory),
            git_branch: Some("main"),
            ..Default::default()
        };
        assert_eq!(
            tab_title(&source, 0, true).as_deref(),
            Some("claude · Fix the build")
        );
        assert_eq!(
            tab_title(&source, 1, true),
            Some(format!(
                "claude · Fix the build — {}",
                joined(&["code", "solo"])
            ))
        );

        // A title that already names the agent isn't repeated.
        let default_title = TitleSource {
            agent: Some(("claude", Some("Claude Code"))),
            ..source
        };
        assert_eq!(
            tab_title(&default_title, 0, true).as_deref(),
            Some("Claude Code")
        );

        let untitled = TitleSource {
            agent: Some(("codex", None)),
            ..source
        };
        assert_eq!(tab_title(&untitled, 0, true).as_deref(), Some("codex"));

        let long = TitleSource {
            agent: Some((
                "claude",
                Some("Investigate why the integration tests are flaky on Windows"),
            )),
            ..source
        };
        assert_eq!(
            tab_title(&long, 0, true).as_deref(),
            Some("claude · Investigate why the integratio…")
        );
        assert_eq!(
            tab_title(&long, 0, false).as_deref(),
            Some("claude · Investigate why the integration tests are flaky on Windows")
        );
    }

    #[test]
    fn test_command_title() {
        let directory = path(&["code", "web"]);
        let source = TitleSource {
            running_command: Some("npm run dev"),
            working_directory: Some(&directory),
            git_branch: Some("main"),
            ..Default::default()
        };
        assert_eq!(tab_title(&source, 0, true).as_deref(), Some("npm run dev"));
        assert_eq!(
            tab_title(&source, 1, true),
            Some(format!("npm run dev — {}", joined(&["code", "web"])))
        );

        let long = TitleSource {
            running_command: Some("cargo test -p terminal_view --all-features -- tab_title"),
            ..source
        };
        assert_eq!(
            tab_title(&long, 0, true).as_deref(),
            Some("cargo test -p terminal_view --…")
        );
        assert_eq!(
            tab_title(&long, 0, false).as_deref(),
            Some("cargo test -p terminal_view --all-features -- tab_title")
        );
    }

    #[test]
    fn test_idle_title() {
        let directory = path(&["code", "solo"]);
        let source = TitleSource {
            working_directory: Some(&directory),
            git_branch: Some("main"),
            ..Default::default()
        };
        assert_eq!(tab_title(&source, 0, true).as_deref(), Some("solo (main)"));
        assert_eq!(
            tab_title(&source, 1, true),
            Some(format!("{} (main)", joined(&["code", "solo"])))
        );
        // More detail than there are directories shows the whole path.
        assert_eq!(
            tab_title(&source, 5, true),
            Some(format!("{} (main)", joined(&["code", "solo"])))
        );

        let outside_git = TitleSource {
            git_branch: None,
            ..source
        };
        assert_eq!(tab_title(&outside_git, 0, true).as_deref(), Some("solo"));

        let root = path(&[]);
        let at_root = TitleSource {
            working_directory: Some(&root),
            git_branch: None,
            ..Default::default()
        };
        assert_eq!(
            tab_title(&at_root, 0, true),
            Some(root.to_string_lossy().into_owned())
        );

        assert_eq!(tab_title(&TitleSource::default(), 0, true), None);
    }

    #[test]
    fn test_session_title() {
        assert_eq!(session_title("✳ Fix the build"), Some("Fix the build"));
        assert_eq!(session_title("◐ Fix the build"), Some("Fix the build"));
        assert_eq!(session_title("Fix the build"), Some("Fix the build"));
        assert_eq!(session_title("✳ 修复构建"), Some("修复构建"));
        assert_eq!(session_title("修 复构建"), Some("修 复构建"));
        assert_eq!(session_title("  "), None);
        assert_eq!(session_title("✳ "), None);
        assert_eq!(session_title(""), None);
        assert_eq!(
            session_title(r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe"),
            None
        );
        assert_eq!(session_title(r"Fix C:\x"), Some(r"Fix C:\x"));
    }
}
