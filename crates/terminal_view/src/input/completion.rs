//! Completions for the terminal input.
//!
//! PowerShell is asked for its own completions (see [`super::powershell`]).
//! Every shell also gets Solo's built-in completions: commands on `PATH`,
//! subcommands of common tools, and file paths.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use gpui::{App, Task};

use super::powershell::PowerShellCompleter;

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionItem {
    /// Text that replaces the completed range.
    pub text: String,
    /// Text shown in the menu.
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Subcommand,
    Parameter,
    Directory,
    File,
    Other,
}

/// Completions for the token around the cursor, which spans the byte range
/// `start..end` of the input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompletionSet {
    pub start: usize,
    pub end: usize,
    pub items: Vec<CompletionItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellFlavor {
    PowerShell,
    /// bash, zsh and other shells with POSIX quoting and `/` paths.
    Posix,
}

impl ShellFlavor {
    pub fn from_shell(shell: Option<&str>) -> Self {
        let name = shell
            .and_then(|shell| Path::new(shell).file_stem())
            .map(|stem| stem.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match name.as_str() {
            "powershell" | "pwsh" => Self::PowerShell,
            "bash" | "zsh" | "sh" | "fish" => Self::Posix,
            _ if cfg!(windows) => Self::PowerShell,
            _ => Self::Posix,
        }
    }
}

/// Completes `input` at byte offset `cursor` in directory `cwd`.
pub fn complete(
    shell: Option<String>,
    input: String,
    cursor: usize,
    cwd: Option<PathBuf>,
    cx: &App,
) -> Task<Result<CompletionSet>> {
    let executor = cx.background_executor().clone();
    let flavor = ShellFlavor::from_shell(shell.as_deref());
    let powershell = (flavor == ShellFlavor::PowerShell)
        .then(|| shell.filter(|shell| Path::new(shell).is_absolute()))
        .flatten()
        .map(|program| PowerShellCompleter::get(&program));
    executor.spawn(async move {
        let builtin = builtin_completions(&input, cursor, cwd.as_deref(), flavor);
        let Some(powershell) = powershell else {
            return Ok(builtin);
        };
        let from_shell = powershell
            .complete(&input, cursor, cwd.as_deref())
            .await
            .unwrap_or_else(|error| {
                log::warn!("PowerShell completion failed: {error:#}");
                CompletionSet::default()
            });
        Ok(merge(from_shell, builtin))
    })
}

/// The shell knows best; Solo's subcommands fill in where it has nothing.
fn merge(from_shell: CompletionSet, builtin: CompletionSet) -> CompletionSet {
    if from_shell.items.is_empty() {
        return builtin;
    }
    let mut merged = from_shell;
    if (merged.start, merged.end) == (builtin.start, builtin.end) {
        let extra: Vec<_> = builtin
            .items
            .into_iter()
            .filter(|item| item.kind == CompletionKind::Subcommand)
            .filter(|item| {
                !merged
                    .items
                    .iter()
                    .any(|existing| existing.text == item.text)
            })
            .collect();
        merged.items.splice(0..0, extra);
    }
    merged
}

/// A token of the input, with its byte range. Quotes are kept in `text`.
#[derive(Debug, PartialEq)]
struct Token<'a> {
    text: &'a str,
    start: usize,
}

/// Splits the input up to the cursor into the tokens of the command being
/// typed (after the last `|`, `;` or `&&`), the last one being the one under
/// the cursor (possibly empty).
fn command_tokens(input: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut token_start = None;
    let mut quote = None;
    for (ix, ch) in input.char_indices() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => {
                quote = Some(ch);
                token_start.get_or_insert(ix);
            }
            (None, '|' | ';' | '&') => {
                tokens.clear();
                token_start = None;
            }
            (None, ch) if ch.is_whitespace() => {
                if let Some(start) = token_start.take() {
                    tokens.push(Token {
                        text: &input[start..ix],
                        start,
                    });
                }
            }
            (None, _) => {
                token_start.get_or_insert(ix);
            }
        }
    }
    let start = token_start.unwrap_or(input.len());
    tokens.push(Token {
        text: &input[start..],
        start,
    });
    tokens
}

pub fn builtin_completions(
    input: &str,
    cursor: usize,
    cwd: Option<&Path>,
    flavor: ShellFlavor,
) -> CompletionSet {
    let cursor = cursor.min(input.len());
    let before_cursor = &input[..cursor];
    let tokens = command_tokens(before_cursor);
    let Some(current) = tokens.last() else {
        return CompletionSet::default();
    };
    // The token continues past the cursor up to the next whitespace.
    let end = cursor
        + input[cursor..]
            .find(char::is_whitespace)
            .unwrap_or(input.len() - cursor);
    let mut items = Vec::new();
    let prefix = current.text;
    let is_path =
        prefix.contains(['/', '\\']) || prefix.starts_with('.') || prefix.starts_with('~');

    if tokens.len() == 1 && !is_path {
        items.extend(command_completions(prefix));
    } else if tokens.len() == 2 && !prefix.starts_with('-') {
        items.extend(subcommand_completions(tokens[0].text, prefix));
    }
    if tokens.len() > 1 || is_path {
        items.extend(path_completions(prefix, cwd, flavor));
    }
    CompletionSet {
        start: current.start,
        end,
        items,
    }
}

fn command_completions(prefix: &str) -> Vec<CompletionItem> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let prefix_lower = prefix.to_lowercase();
    path_commands()
        .iter()
        .filter(|command| command.to_lowercase().starts_with(&prefix_lower))
        .take(200)
        .map(|command| CompletionItem {
            text: command.clone(),
            label: command.clone(),
            kind: CompletionKind::Command,
            detail: None,
        })
        .collect()
}

/// Executables on `PATH`, without their extension on Windows.
fn path_commands() -> &'static BTreeSet<String> {
    static COMMANDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    COMMANDS.get_or_init(|| {
        let mut commands = BTreeSet::new();
        let extensions: Vec<String> = if cfg!(windows) {
            std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .map(|ext| ext.trim_start_matches('.').to_lowercase())
                .collect()
        } else {
            Vec::new()
        };
        let Some(path) = std::env::var_os("PATH") else {
            return commands;
        };
        for dir in std::env::split_paths(&path) {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if cfg!(windows) {
                    let Some(ext) = path
                        .extension()
                        .map(|ext| ext.to_string_lossy().to_lowercase())
                    else {
                        continue;
                    };
                    if extensions.contains(&ext)
                        && let Some(stem) = path.file_stem()
                    {
                        commands.insert(stem.to_string_lossy().into_owned());
                    }
                } else if entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_file() || kind.is_symlink())
                {
                    commands.insert(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        commands
    })
}

/// Subcommands of tools that are commonly driven from a terminal, and of the
/// agents run in it.
const SUBCOMMANDS: &[(&str, &[(&str, &str)])] = &[
    (
        "git",
        &[
            ("add", "Add file contents to the index"),
            ("branch", "List, create, or delete branches"),
            ("checkout", "Switch branches or restore working tree files"),
            (
                "cherry-pick",
                "Apply the changes introduced by some existing commits",
            ),
            ("clone", "Clone a repository into a new directory"),
            ("commit", "Record changes to the repository"),
            (
                "diff",
                "Show changes between commits, commit and working tree, etc",
            ),
            ("fetch", "Download objects and refs from another repository"),
            ("init", "Create an empty Git repository"),
            ("log", "Show commit logs"),
            ("merge", "Join two or more development histories together"),
            ("mv", "Move or rename a file, a directory, or a symlink"),
            (
                "pull",
                "Fetch from and integrate with another repository or branch",
            ),
            ("push", "Update remote refs along with associated objects"),
            ("rebase", "Reapply commits on top of another base tip"),
            ("remote", "Manage set of tracked repositories"),
            ("reset", "Reset current HEAD to the specified state"),
            ("restore", "Restore working tree files"),
            ("revert", "Revert some existing commits"),
            (
                "rm",
                "Remove files from the working tree and from the index",
            ),
            ("show", "Show various types of objects"),
            (
                "stash",
                "Stash the changes in a dirty working directory away",
            ),
            ("status", "Show the working tree status"),
            ("switch", "Switch branches"),
            ("tag", "Create, list, delete or verify a tag object"),
            ("worktree", "Manage multiple working trees"),
        ],
    ),
    (
        "cargo",
        &[
            ("add", "Add dependencies to a manifest file"),
            ("bench", "Execute all benchmarks of a local package"),
            (
                "build",
                "Compile a local package and all of its dependencies",
            ),
            (
                "check",
                "Check a local package and all of its dependencies for errors",
            ),
            ("clean", "Remove the target directory"),
            ("clippy", "Check a package to catch common mistakes"),
            ("doc", "Build a package's documentation"),
            ("fmt", "Format all Rust files of the current crate"),
            (
                "init",
                "Create a new cargo package in an existing directory",
            ),
            ("install", "Install a Rust binary"),
            ("new", "Create a new cargo package"),
            ("publish", "Upload a package to the registry"),
            ("remove", "Remove dependencies from a manifest file"),
            ("run", "Run a binary or example of the local package"),
            ("test", "Execute all unit and integration tests"),
            ("tree", "Display a tree visualization of a dependency graph"),
            ("update", "Update dependencies listed in Cargo.lock"),
        ],
    ),
    (
        "npm",
        &[
            ("ci", "Clean install a project"),
            ("exec", "Run a command from a local or remote npm package"),
            ("init", "Create a package.json file"),
            ("install", "Install a package"),
            ("list", "List installed packages"),
            ("outdated", "Check for outdated packages"),
            ("publish", "Publish a package"),
            ("run", "Run arbitrary package scripts"),
            ("start", "Start a package"),
            ("test", "Test a package"),
            ("uninstall", "Remove a package"),
            ("update", "Update packages"),
        ],
    ),
    (
        "pnpm",
        &[
            (
                "add",
                "Install a package and any packages that it depends on",
            ),
            ("dlx", "Run a package without installing it"),
            ("exec", "Execute a shell command in scope of a project"),
            ("install", "Install all dependencies for a project"),
            ("remove", "Remove packages"),
            ("run", "Run a script defined in the package's manifest"),
            ("test", "Run the test script"),
            ("update", "Update packages to their latest version"),
        ],
    ),
    (
        "docker",
        &[
            ("build", "Build an image from a Dockerfile"),
            ("compose", "Define and run multi-container applications"),
            ("exec", "Execute a command in a running container"),
            ("images", "List images"),
            ("logs", "Fetch the logs of a container"),
            ("ps", "List containers"),
            ("pull", "Download an image from a registry"),
            ("push", "Upload an image to a registry"),
            ("rm", "Remove one or more containers"),
            ("run", "Create and run a new container from an image"),
            ("stop", "Stop one or more running containers"),
        ],
    ),
    (
        "claude",
        &[
            ("config", "Manage configuration"),
            ("doctor", "Check the health of the installation"),
            ("mcp", "Configure and manage MCP servers"),
            ("update", "Check for updates and install if available"),
        ],
    ),
];

fn subcommand_completions(command: &str, prefix: &str) -> Vec<CompletionItem> {
    let command = Path::new(command)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    SUBCOMMANDS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, subcommands)| {
            subcommands
                .iter()
                .filter(|(name, _)| name.starts_with(prefix))
                .map(|(name, description)| CompletionItem {
                    text: name.to_string(),
                    label: name.to_string(),
                    kind: CompletionKind::Subcommand,
                    detail: Some(description.to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn path_completions(token: &str, cwd: Option<&Path>, flavor: ShellFlavor) -> Vec<CompletionItem> {
    let unquoted = token.trim_matches(['"', '\'']);
    let split = unquoted.rfind(['/', '\\']).map_or(0, |ix| ix + 1);
    let (dir_part, name_prefix) = unquoted.split_at(split);
    let dir = resolve_dir(dir_part, cwd);
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let case_insensitive = cfg!(windows);
    let name_prefix_cmp = if case_insensitive {
        name_prefix.to_lowercase()
    } else {
        name_prefix.to_string()
    };
    let separator = if flavor == ShellFlavor::PowerShell && cfg!(windows) {
        '\\'
    } else {
        '/'
    };
    let mut items: Vec<CompletionItem> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let cmp = if case_insensitive {
                name.to_lowercase()
            } else {
                name.clone()
            };
            if !cmp.starts_with(&name_prefix_cmp) {
                return None;
            }
            // Hidden entries only when asked for.
            if name.starts_with('.') && !name_prefix.starts_with('.') {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let mut path = format!("{dir_part}{name}");
            if is_dir {
                path.push(separator);
            }
            Some(CompletionItem {
                text: quote_path(&path, flavor),
                label: if is_dir {
                    format!("{name}{separator}")
                } else {
                    name
                },
                kind: if is_dir {
                    CompletionKind::Directory
                } else {
                    CompletionKind::File
                },
                detail: None,
            })
        })
        .collect();
    // Directories first, then by name.
    items.sort_by(|a, b| {
        (a.kind != CompletionKind::Directory, a.label.to_lowercase())
            .cmp(&(b.kind != CompletionKind::Directory, b.label.to_lowercase()))
    });
    items.truncate(500);
    items
}

fn resolve_dir(dir_part: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if dir_part.is_empty() {
        return cwd
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());
    }
    let expanded = if let Some(rest) = dir_part.strip_prefix('~') {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
        PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(dir_part)
    };
    Some(if expanded.is_absolute() {
        expanded
    } else {
        cwd?.join(expanded)
    })
}

fn quote_path(path: &str, flavor: ShellFlavor) -> String {
    if !path.contains([' ', '\'', '"', '(', ')', '&', ';']) {
        return path.to_string();
    }
    match flavor {
        ShellFlavor::PowerShell => format!("'{}'", path.replace('\'', "''")),
        ShellFlavor::Posix => format!("'{}'", path.replace('\'', r"'\''")),
    }
}

/// Converts a byte offset in `text` to UTF-16 code units, as PowerShell counts.
pub fn byte_to_utf16(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset.min(text.len())].encode_utf16().count()
}

/// Converts UTF-16 code units in `text` to a byte offset.
pub fn utf16_to_byte(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (ix, ch) in text.char_indices() {
        if units >= utf16_offset {
            return ix;
        }
        units += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_tokens() {
        let texts = |input| {
            command_tokens(input)
                .into_iter()
                .map(|token| token.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts("git che"), vec!["git", "che"]);
        assert_eq!(texts("git "), vec!["git", ""]);
        assert_eq!(texts("ls | grep 'a b"), vec!["grep", "'a b"]);
        assert_eq!(texts("cd x; car"), vec!["car"]);
        assert_eq!(texts(""), vec![""]);
    }

    #[test]
    fn test_subcommands() {
        let set = builtin_completions("git chec", 8, None, ShellFlavor::Posix);
        assert_eq!(set.start, 4);
        assert_eq!(set.end, 8);
        assert_eq!(set.items[0].text, "checkout");
        assert_eq!(set.items[0].kind, CompletionKind::Subcommand);
    }

    #[test]
    fn test_path_completions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("my dir")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();

        let set = builtin_completions("cat s", 5, Some(dir.path()), ShellFlavor::Posix);
        assert_eq!(set.start, 4);
        let texts: Vec<_> = set.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, vec!["src/"]);

        let set = builtin_completions("cat src/", 8, Some(dir.path()), ShellFlavor::Posix);
        let texts: Vec<_> = set.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, vec!["src/main.rs"]);

        let set = builtin_completions("cd my", 5, Some(dir.path()), ShellFlavor::Posix);
        let texts: Vec<_> = set.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, vec!["'my dir/'"]);
    }

    #[test]
    fn test_utf16_offsets() {
        let text = "echo 你好 x";
        let byte = text.find('x').unwrap();
        let utf16 = byte_to_utf16(text, byte);
        assert_eq!(utf16, 8);
        assert_eq!(utf16_to_byte(text, utf16), byte);
    }
}
