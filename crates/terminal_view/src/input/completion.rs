//! Completions for the terminal input, the way Warp does them.
//!
//! Commands with a spec (Warp's command signatures, see [`super::spec`]) get
//! their subcommands, options and argument values, including values generated
//! by running a command in the shell (git branches, npm scripts, ...). Other
//! commands fall back to the shell: PowerShell is asked for its own
//! completions (see [`super::powershell`]), and every shell gets commands on
//! `PATH` and file paths.

use std::collections::{BTreeSet, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{App, Task};

use super::powershell::{PowerShellCompleter, hide_window};
use super::spec;

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
    /// An argument value from a spec or a generator.
    Value,
    Branch,
    Container,
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

/// How long a generator command may run.
const GENERATOR_TIMEOUT: Duration = Duration::from_secs(5);

/// Completes `input` at byte offset `cursor` in directory `cwd`. `shell` is
/// the shell program the integration script reported.
pub fn complete(
    shell: Option<String>,
    input: String,
    cursor: usize,
    cwd: Option<PathBuf>,
    cx: &App,
) -> Task<Result<CompletionSet>> {
    let executor = cx.background_executor().clone();
    let flavor = ShellFlavor::from_shell(shell.as_deref());
    executor.spawn(async move {
        let cursor = cursor.min(input.len());
        let tokens = command_tokens(&input[..cursor]);
        let Some(current) = tokens.last() else {
            return Ok(CompletionSet::default());
        };
        // The token continues past the cursor up to the next whitespace.
        let end = cursor
            + input[cursor..]
                .find(char::is_whitespace)
                .unwrap_or(input.len() - cursor);
        let mut texts: Vec<&str> = tokens.iter().map(|token| token.text).collect();
        // git aliases complete like the commands they stand for.
        let is_git = spec::command_name(texts[0]).as_deref() == Some("git");
        let git_aliases = if is_git && tokens.len() > 1 {
            git_aliases(cwd.as_deref())
        } else {
            Vec::new()
        };
        if let Some((_, expansion)) = texts
            .get(1)
            .filter(|_| texts.len() > 2)
            .and_then(|alias| git_aliases.iter().find(|(name, _)| name == alias))
        {
            texts.splice(1..2, expansion.split_whitespace());
        }

        if tokens.len() > 1
            && let Some(plan) = spec::plan(&texts, flavor)
        {
            let start = current.start + plan.value_prefix_len;
            let prefix = &current.text[plan.value_prefix_len..];
            let mut items = plan.items;
            for run in &plan.generators {
                match run_generator(&run.command, shell.as_deref(), flavor, cwd.as_deref()).await {
                    Some(output) => items.extend(run.results(&output)),
                    None => log::debug!("completion generator failed: {}", run.command),
                }
            }
            if let Some(paths) = &plan.paths {
                items.extend(
                    path_completions(prefix, cwd.as_deref(), flavor, paths.folders_only)
                        .into_iter()
                        .filter_map(|item| paths.filter(item)),
                );
            }
            if plan.commands {
                items.extend(command_completions(prefix, shell.as_deref(), flavor));
            }
            if tokens.len() == 2 {
                items.extend(
                    git_aliases
                        .iter()
                        .filter(|(name, _)| name.starts_with(prefix))
                        .map(|(name, expansion)| CompletionItem {
                            text: name.clone(),
                            label: name.clone(),
                            kind: CompletionKind::Subcommand,
                            detail: Some(format!("Alias for {expansion}")),
                        }),
                );
            }
            return Ok(CompletionSet {
                start,
                end,
                items: dedup(items),
            });
        }

        let builtin = builtin_completions(&texts, current.start, end, cwd.as_deref(), flavor);
        let powershell = (flavor == ShellFlavor::PowerShell)
            .then(|| {
                shell
                    .as_deref()
                    .filter(|shell| Path::new(shell).is_absolute())
            })
            .flatten()
            .map(PowerShellCompleter::get);
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

/// PowerShell knows its cmdlets, functions and aliases; Solo adds descriptions
/// for specced commands, and its own results where PowerShell has none.
fn merge(mut from_shell: CompletionSet, builtin: CompletionSet) -> CompletionSet {
    if from_shell.items.is_empty() {
        return builtin;
    }
    if let Some(descriptions) = spec::command_descriptions() {
        for item in &mut from_shell.items {
            if item.kind == CompletionKind::Command && item.detail.is_none() {
                item.detail = spec::command_name(&item.text)
                    .and_then(|name| descriptions.get(&name).cloned());
            }
        }
    }
    from_shell
}

fn dedup(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.text.clone()))
        .collect()
}

/// Runs a generator's command in the user's shell and returns its output.
async fn run_generator(
    command: &str,
    shell: Option<&str>,
    flavor: ShellFlavor,
    cwd: Option<&Path>,
) -> Option<String> {
    let posix_shell = match flavor {
        // Many specs' generators are written for POSIX shells (`[[ ]]`, pipes
        // into grep and cut); on Windows those run in Git's bash if there is
        // one.
        ShellFlavor::PowerShell if looks_posix(command) => posix_shell_for_windows(),
        ShellFlavor::PowerShell => None,
        // Generators are POSIX shell; fish isn't.
        ShellFlavor::Posix => Some(PathBuf::from(
            shell
                .filter(|shell| {
                    Path::new(shell).file_stem().and_then(|stem| stem.to_str()) != Some("fish")
                })
                .unwrap_or("sh"),
        )),
    };
    match posix_shell {
        Some(program) => {
            let mut process = Command::new(program);
            process.arg("-c").arg(command);
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            run_with_timeout(process, GENERATOR_TIMEOUT)
        }
        None => {
            let program = shell.filter(|shell| Path::new(shell).is_absolute())?;
            PowerShellCompleter::get_for_generators(program)
                .exec(command, cwd)
                .await
                .ok()
        }
    }
}

/// Whether a generator command uses POSIX shell syntax or tools.
fn looks_posix(command: &str) -> bool {
    const MARKERS: &[&str] = &[
        "[[",
        "]]",
        "$(",
        "`",
        "until ",
        "; do ",
        "; then ",
        "/dev/null",
        "| grep",
        "|grep",
        "| cut",
        "| awk",
        "| sed",
        "| sort",
        "| uniq",
        "| head",
        "| tail",
        "| tr ",
        "| xargs",
        "| wc",
        "| jq",
        "export ",
        "\\\n",
    ];
    MARKERS.iter().any(|marker| command.contains(marker))
}

/// Git for Windows' bash, found next to `git` on `PATH`. The `bash.exe` in
/// System32 starts WSL, whose Linux tools can't see Windows paths the same
/// way, so it isn't used.
fn posix_shell_for_windows() -> Option<PathBuf> {
    static SHELL: OnceLock<Option<PathBuf>> = OnceLock::new();
    SHELL
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
            let is_wsl = |path: &Path| {
                let path = path.to_string_lossy().to_lowercase();
                path.contains("system32") || path.contains("windowsapps")
            };
            if let Some(bash) = dirs
                .iter()
                .map(|dir| dir.join("bash.exe"))
                .find(|bash| bash.is_file() && !is_wsl(bash))
            {
                return Some(bash);
            }
            // `git.exe` lives in `<git>/cmd`; bash in `<git>/bin`.
            dirs.iter()
                .filter(|dir| dir.join("git.exe").is_file())
                .filter_map(|dir| dir.parent())
                .flat_map(|git| {
                    [
                        git.join("bin").join("bash.exe"),
                        git.join("usr").join("bin").join("bash.exe"),
                    ]
                })
                .find(|bash| bash.is_file())
        })
        .clone()
}

/// The user's git aliases, as (name, expansion), except shell ones (`!...`).
/// They're read again at most every few seconds, not on every keystroke.
fn git_aliases(cwd: Option<&Path>) -> Vec<(String, String)> {
    type Cache = Option<(Option<PathBuf>, Instant, Vec<(String, String)>)>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);
    const MAX_AGE: Duration = Duration::from_secs(10);

    let cwd = cwd.map(Path::to_path_buf);
    if let Ok(cache) = CACHE.lock()
        && let Some((cached_cwd, read_at, aliases)) = cache.as_ref()
        && *cached_cwd == cwd
        && read_at.elapsed() < MAX_AGE
    {
        return aliases.clone();
    }
    let mut command = Command::new("git");
    command.args(["config", "--get-regexp", r"^alias\."]);
    if let Some(cwd) = &cwd {
        command.current_dir(cwd);
    }
    let aliases = run_with_timeout(command, Duration::from_secs(2))
        .map(|output| parse_git_aliases(&output))
        .unwrap_or_default();
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((cwd, Instant::now(), aliases.clone()));
    }
    aliases
}

fn parse_git_aliases(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (key, expansion) = line.split_once(' ')?;
            let name = key.strip_prefix("alias.")?;
            let expansion = expansion.trim();
            (!expansion.starts_with('!') && !expansion.is_empty())
                .then(|| (name.to_string(), expansion.to_string()))
        })
        .collect()
}

/// Runs `command` and returns its stdout, or `None` if it fails or takes
/// longer than `timeout`.
#[allow(
    clippy::disallowed_methods,
    reason = "completion helpers only run on background threads"
)]
pub(super) fn run_with_timeout(mut command: Command, timeout: Duration) -> Option<String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_window(&mut command);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).ok();
        output
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                child.kill().ok();
                return None;
            }
        }
    }
    Some(String::from_utf8_lossy(&reader.join().ok()?).into_owned())
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

/// Completions without a spec: commands in the command position, else paths.
fn builtin_completions(
    tokens: &[&str],
    start: usize,
    end: usize,
    cwd: Option<&Path>,
    flavor: ShellFlavor,
) -> CompletionSet {
    let prefix = tokens.last().copied().unwrap_or_default();
    let is_path =
        prefix.contains(['/', '\\']) || prefix.starts_with('.') || prefix.starts_with('~');
    let items = if tokens.len() == 1 && !is_path {
        command_completions(prefix, None, flavor)
    } else {
        path_completions(prefix, cwd, flavor, false)
    };
    CompletionSet { start, end, items }
}

fn command_completions(
    prefix: &str,
    _shell: Option<&str>,
    _flavor: ShellFlavor,
) -> Vec<CompletionItem> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let prefix_lower = prefix.to_lowercase();
    let descriptions = spec::command_descriptions();
    path_commands()
        .iter()
        .filter(|command| command.to_lowercase().starts_with(&prefix_lower))
        .take(200)
        .map(|command| CompletionItem {
            text: command.clone(),
            label: command.clone(),
            kind: CompletionKind::Command,
            detail: descriptions
                .and_then(|descriptions| descriptions.get(&command.to_lowercase()).cloned()),
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

fn path_completions(
    token: &str,
    cwd: Option<&Path>,
    flavor: ShellFlavor,
    folders_only: bool,
) -> Vec<CompletionItem> {
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
            if folders_only && !is_dir {
                return None;
            }
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
    fn parses_git_aliases() {
        assert_eq!(
            parse_git_aliases(
                "alias.co checkout\nalias.lg log --oneline --graph\nalias.up !git pull\n"
            ),
            vec![
                ("co".to_string(), "checkout".to_string()),
                ("lg".to_string(), "log --oneline --graph".to_string()),
            ]
        );
    }

    fn complete_now(input: &str, cwd: Option<&Path>) -> CompletionSet {
        futures::executor::block_on(complete_inner(input, cwd))
    }

    async fn complete_inner(input: &str, cwd: Option<&Path>) -> CompletionSet {
        let tokens = command_tokens(input);
        let texts: Vec<&str> = tokens.iter().map(|token| token.text).collect();
        let current = tokens.last().unwrap();
        if tokens.len() > 1
            && let Some(plan) = spec::plan(&texts, ShellFlavor::Posix)
        {
            let mut items = plan.items;
            if let Some(paths) = &plan.paths {
                items.extend(path_completions(
                    &current.text[plan.value_prefix_len..],
                    cwd,
                    ShellFlavor::Posix,
                    paths.folders_only,
                ));
            }
            return CompletionSet {
                start: current.start + plan.value_prefix_len,
                end: input.len(),
                items,
            };
        }
        builtin_completions(&texts, current.start, input.len(), cwd, ShellFlavor::Posix)
    }

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
    fn test_spec_completions() {
        let set = complete_now("git chec", None);
        assert_eq!(set.start, 4);
        assert!(set.items.iter().any(|item| item.text == "checkout"));

        let set = complete_now("cargo build --rel", None);
        assert!(set.items.iter().any(|item| item.text == "--release"));
    }

    #[test]
    fn test_path_completions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("my dir")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();

        let texts = |set: CompletionSet| {
            set.items
                .into_iter()
                .map(|item| item.text)
                .collect::<Vec<_>>()
        };
        // `cat` is specced to take files.
        let set = complete_now("cat s", Some(dir.path()));
        assert_eq!(set.start, 4);
        assert_eq!(texts(set), vec!["src/"]);

        let set = complete_now("cat src/", Some(dir.path()));
        assert_eq!(texts(set), vec!["src/main.rs"]);

        // `cd` takes folders only.
        let set = complete_now("cd ", Some(dir.path()));
        assert_eq!(texts(set), vec!["'my dir/'", "src/"]);
    }

    #[test]
    fn test_run_with_timeout() {
        let mut command = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            command.args(["/C", "echo hi"]);
        } else {
            command.args(["-c", "echo hi"]);
        }
        assert_eq!(
            run_with_timeout(command, Duration::from_secs(5))
                .map(|output| output.trim().to_string()),
            Some("hi".to_string())
        );
    }

    #[test]
    fn test_posix_generator_in_powershell_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"dev": "vite", "build": "vite build"}}"#,
        )
        .unwrap();
        let plan = spec::plan(&["npm", "run", ""], ShellFlavor::PowerShell).unwrap();
        let run = plan.generators.first().unwrap();
        assert!(looks_posix(&run.command));
        if cfg!(windows) && posix_shell_for_windows().is_none() {
            return;
        }
        let output = futures::executor::block_on(run_generator(
            &run.command,
            None,
            ShellFlavor::PowerShell,
            Some(dir.path()),
        ))
        .unwrap();
        let mut scripts: Vec<_> = run
            .results(&output)
            .into_iter()
            .map(|item| item.text)
            .collect();
        scripts.sort();
        assert_eq!(scripts, vec!["build", "dev"]);
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
