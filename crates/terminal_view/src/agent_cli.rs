//! SOLO: `solo-cli agent ...`, which lets an agent running in a Solo terminal
//! see and drive the others: list terminals and agents, send an agent a
//! prompt, wait for it, read its answer, open files and reviews, and start an
//! agent task in a new worktree (docs/agent-cli.md).
//!
//! The CLI forwards its arguments to the running Solo (`cli::CliRequest::Agent`)
//! with the id of the terminal it runs in (`SOLO_TERMINAL_ID`), and prints
//! what this returns. The `agent_control` setting limits or turns it off.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor::Editor;
use gpui::{App, AppContext as _, AsyncApp, Entity, WindowHandle};
use language::Point;
use serde::Serialize;
use settings::Settings as _;
use terminal::terminal_settings::{AgentControl, TerminalSettings};
use util::i18n::is_chinese;
use util::paths::PathWithPosition;
use workspace::{MultiWorkspace, OpenOptions, Workspace};

use crate::TerminalView;
use crate::agent_status::AgentStatus;
use crate::agent_tasks::git;
use crate::agent_turns::{AgentTurnReview, AgentTurns};
use crate::command_blocks::terminal_views;

/// How often `wait` looks at the agent again.
const WAIT_POLL: Duration = Duration::from_millis(250);
/// After a prompt is sent, how long an idle agent gets to start working
/// before `wait --until idle` takes it as done.
const WAIT_SETTLE: Duration = Duration::from_secs(10);
/// How long a new task's agent gets to start before its prompt is sent.
const NEW_TASK_AGENT_START: Duration = Duration::from_secs(90);
const DEFAULT_OUTPUT_LINES: usize = 40;

/// What the CLI prints and exits with.
#[derive(Debug, Default, PartialEq)]
pub struct AgentCliOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

impl AgentCliOutput {
    fn ok(stdout: String) -> Self {
        Self {
            stdout,
            ..Self::default()
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            stderr: message.into(),
            status: 1,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Until {
    Idle,
    NeedsInput,
    Exit,
}

#[derive(Debug, PartialEq)]
enum Command {
    Ls,
    Send {
        id: String,
        text: String,
    },
    Wait {
        id: String,
        until: Until,
        timeout: Option<Duration>,
    },
    Output {
        id: String,
        lines: usize,
    },
    Open {
        path: String,
    },
    Review {
        id: String,
    },
    NewTask {
        prompt: String,
        agent: Option<String>,
        base: Option<String>,
        name: Option<String>,
    },
    Help,
}

impl Command {
    /// Whether it changes something: sends text to a terminal, or makes a
    /// worktree and starts an agent.
    fn acts(&self) -> bool {
        matches!(self, Command::Send { .. } | Command::NewTask { .. })
    }
}

#[derive(Debug, PartialEq)]
struct Invocation {
    command: Command,
    json: bool,
}

pub const USAGE: &str = "\
usage: solo-cli agent <command> [--json]

  ls                                 terminals and the agents in them
  send <id> <text>...                send a prompt to the agent in a terminal
  wait <id> [--until idle|needs-input|exit] [--timeout <seconds>]
                                     wait for the agent (default: until idle)
  output <id> [--lines <n>]          the agent's last answer
  open <path[:line[:column]]>        open a file in Solo
  review <id>                        open the review of the agent's last turn
  new-task --prompt <text> [--agent <command>] [--base <branch>] [--name <name>]
                                     start an agent in a new git worktree and branch

<id> is a terminal's id from `ls`; `self` is the terminal the command runs in.";

fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut json = false;
    let mut words = Vec::new();
    let mut options: Vec<(String, String)> = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                return Ok(Invocation {
                    command: Command::Help,
                    json,
                });
            }
            option @ ("--until" | "--timeout" | "--lines" | "--prompt" | "--agent" | "--base"
            | "--name") => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{option} needs a value"))?;
                options.push((option.to_string(), value.clone()));
            }
            option if option.starts_with("--") => {
                return Err(format!("unknown option {option}"));
            }
            word => words.push(word.to_string()),
        }
    }
    let option = |name: &str| {
        options
            .iter()
            .find(|(option, _)| option == name)
            .map(|(_, value)| value.clone())
    };
    let allow = |allowed: &[&str]| -> Result<(), String> {
        match options
            .iter()
            .find(|(option, _)| !allowed.contains(&option.as_str()))
        {
            Some((option, _)) => Err(format!("{option} doesn't go with this command")),
            None => Ok(()),
        }
    };
    let id = |words: &[String]| -> Result<String, String> {
        words
            .get(1)
            .cloned()
            .ok_or_else(|| "which terminal? give its id from `ls`".to_string())
    };
    let no_more = |words: &[String], count: usize| -> Result<(), String> {
        match words.get(count) {
            Some(extra) => Err(format!("unexpected argument {extra:?}")),
            None => Ok(()),
        }
    };
    let command = match words.first().map(String::as_str) {
        None | Some("help") => Command::Help,
        Some("ls") => {
            allow(&[])?;
            no_more(&words, 1)?;
            Command::Ls
        }
        Some("send") => {
            allow(&[])?;
            let id = id(&words)?;
            let text = words[2..].join(" ");
            if text.trim().is_empty() {
                return Err("send what? give the text after the id".to_string());
            }
            Command::Send { id, text }
        }
        Some("wait") => {
            allow(&["--until", "--timeout"])?;
            let id = id(&words)?;
            no_more(&words, 2)?;
            let until = match option("--until").as_deref() {
                None | Some("idle") => Until::Idle,
                Some("needs-input") => Until::NeedsInput,
                Some("exit") => Until::Exit,
                Some(other) => {
                    return Err(format!(
                        "--until {other:?}: expected idle, needs-input or exit"
                    ));
                }
            };
            let timeout = option("--timeout")
                .map(|seconds| {
                    seconds
                        .parse::<f64>()
                        .ok()
                        .filter(|seconds| seconds.is_finite() && *seconds >= 0.)
                        .map(Duration::from_secs_f64)
                        .ok_or_else(|| format!("--timeout {seconds:?} isn't a number of seconds"))
                })
                .transpose()?;
            Command::Wait { id, until, timeout }
        }
        Some("output") => {
            allow(&["--lines"])?;
            let id = id(&words)?;
            no_more(&words, 2)?;
            let lines = option("--lines")
                .map(|lines| {
                    lines
                        .parse::<usize>()
                        .ok()
                        .filter(|lines| *lines > 0)
                        .ok_or_else(|| format!("--lines {lines:?} isn't a positive number"))
                })
                .transpose()?
                .unwrap_or(DEFAULT_OUTPUT_LINES);
            Command::Output { id, lines }
        }
        Some("open") => {
            allow(&[])?;
            let path = words
                .get(1)
                .cloned()
                .ok_or_else(|| "open what? give a path".to_string())?;
            no_more(&words, 2)?;
            Command::Open { path }
        }
        Some("review") => {
            allow(&[])?;
            let id = id(&words)?;
            no_more(&words, 2)?;
            Command::Review { id }
        }
        Some("new-task") => {
            allow(&["--prompt", "--agent", "--base", "--name"])?;
            no_more(&words, 1)?;
            let prompt = option("--prompt")
                .filter(|prompt| !prompt.trim().is_empty())
                .ok_or_else(|| "--prompt is required".to_string())?;
            let agent = option("--agent");
            // Typed into the task's shell: a command name, nothing more.
            if let Some(agent) = &agent
                && (agent.is_empty()
                    || !agent
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            {
                return Err(format!("--agent {agent:?} isn't a command name"));
            }
            let base = option("--base");
            if let Some(base) = &base
                && (base.is_empty() || base.starts_with('-'))
            {
                return Err(format!("--base {base:?} isn't a branch"));
            }
            Command::NewTask {
                prompt,
                agent,
                base,
                name: option("--name"),
            }
        }
        Some(other) => return Err(format!("unknown command {other:?}")),
    };
    Ok(Invocation { command, json })
}

/// A terminal as `ls` lists it.
#[derive(Clone, Debug, Serialize)]
struct TerminalRow {
    id: String,
    title: String,
    /// The agent running or last run in it.
    agent: Option<String>,
    /// `working`, `needs-input`, `idle`, `done` (a turn you haven't looked
    /// at), `exited` or `failed`.
    status: Option<&'static str>,
    directory: Option<PathBuf>,
    branch: Option<String>,
    /// Whether the command runs in it.
    current: bool,
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::NeedsInput => "needs-input",
        AgentStatus::Idle => "idle",
        AgentStatus::Finished => "done",
        AgentStatus::Done => "exited",
        AgentStatus::Failed => "failed",
    }
}

/// A terminal view, with the workspace and window it's in.
#[derive(Clone)]
struct Found {
    window: WindowHandle<MultiWorkspace>,
    workspace: Entity<Workspace>,
    view: Entity<TerminalView>,
}

/// The id `ls` shows for a terminal: the one its shell got in
/// `SOLO_TERMINAL_ID`, or one made of its view's for terminals without.
fn terminal_id(view: &Entity<TerminalView>, cx: &App) -> String {
    view.read(cx)
        .terminal
        .read(cx)
        .shell_integration()
        .terminal_id()
        .map(str::to_string)
        .unwrap_or_else(|| format!("v{}", view.entity_id().as_u64()))
}

fn all_terminals(cx: &App) -> Vec<Found> {
    let mut found = Vec::new();
    for window in cx.windows() {
        let Some(window) = window.downcast::<MultiWorkspace>() else {
            continue;
        };
        let Ok(multi_workspace) = window.read(cx) else {
            continue;
        };
        for workspace in multi_workspace.workspaces() {
            for view in terminal_views(workspace, cx) {
                found.push(Found {
                    window,
                    workspace: workspace.clone(),
                    view,
                });
            }
        }
    }
    found
}

fn find(id: &str, caller: Option<&str>, cx: &App) -> Result<Found, String> {
    let id = match id {
        "self" => caller.ok_or_else(|| {
            "`self` only works in a Solo terminal (SOLO_TERMINAL_ID isn't set)".to_string()
        })?,
        id => id,
    };
    all_terminals(cx)
        .into_iter()
        .find(|found| terminal_id(&found.view, cx) == id)
        .ok_or_else(|| format!("no terminal {id:?}; `solo-cli agent ls` lists them"))
}

fn row(found: &Found, caller: Option<&str>, cx: &App) -> TerminalRow {
    let view = found.view.read(cx);
    let id = terminal_id(&found.view, cx);
    let agent = view.agent_status();
    TerminalRow {
        current: caller == Some(id.as_str()),
        id,
        title: view.smart_title(0, false, cx),
        agent: agent.map(|(name, _)| name.to_string()),
        status: agent.map(|(_, status)| status_name(status)),
        directory: view.terminal.read(cx).working_directory(),
        branch: view.cached_git_branch(cx).map(str::to_string),
    }
}

fn format_rows(rows: &[TerminalRow]) -> String {
    if rows.is_empty() {
        return "no terminals\n".to_string();
    }
    let mut out = String::new();
    for row in rows {
        out.push_str(&format!(
            "{}{}\t{}\t{}\t{}\t{}\t{}\n",
            row.id,
            if row.current { "*" } else { "" },
            row.agent.as_deref().unwrap_or("-"),
            row.status.unwrap_or("-"),
            row.branch.as_deref().unwrap_or("-"),
            row.directory
                .as_deref()
                .map(|directory| directory.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
            row.title,
        ));
    }
    out
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

/// Runs `solo-cli agent <args>`, from the terminal `caller` if it runs in one.
pub async fn run(
    args: Vec<String>,
    caller: Option<String>,
    cwd: Option<PathBuf>,
    cx: &mut AsyncApp,
) -> AgentCliOutput {
    let invocation = match parse(&args) {
        Ok(invocation) => invocation,
        Err(error) => {
            return AgentCliOutput {
                stderr: format!("{error}\n\n{USAGE}"),
                status: 2,
                ..AgentCliOutput::default()
            };
        }
    };
    let control = cx.update(|cx| TerminalSettings::get_global(cx).agent_control);
    match control {
        AgentControl::Off => {
            return AgentCliOutput::error(
                "`solo-cli agent` is off (the terminal setting `agent_control` is \"off\")",
            );
        }
        AgentControl::ReadOnly if invocation.command.acts() => {
            return AgentCliOutput::error(
                "`send` and `new-task` are off (the terminal setting `agent_control` is \"read_only\")",
            );
        }
        _ => {}
    }
    log::info!(
        "solo agent CLI from terminal {}: {args:?}",
        caller.as_deref().unwrap_or("(none)")
    );
    let caller = caller.as_deref();
    let json_output = invocation.json;
    match invocation.command {
        Command::Help => AgentCliOutput::ok(format!("{USAGE}\n")),
        Command::Ls => cx.update(|cx| {
            let rows: Vec<TerminalRow> = all_terminals(cx)
                .iter()
                .map(|found| row(found, caller, cx))
                .collect();
            AgentCliOutput::ok(if json_output {
                json(&rows)
            } else {
                format_rows(&rows)
            })
        }),
        Command::Send { id, text } => cx.update(|cx| send(&id, &text, caller, cx)),
        Command::Wait { id, until, timeout } => {
            wait(&id, until, timeout, caller, json_output, cx).await
        }
        Command::Output { id, lines } => {
            cx.update(|cx| output(&id, lines, caller, json_output, cx))
        }
        Command::Open { path } => open(&path, caller, cwd, cx).await,
        Command::Review { id } => cx.update(|cx| review(&id, caller, cx)),
        Command::NewTask {
            prompt,
            agent,
            base,
            name,
        } => new_task(prompt, agent, base, name, caller, cwd, json_output, cx).await,
    }
}

fn send(id: &str, text: &str, caller: Option<&str>, cx: &mut App) -> AgentCliOutput {
    let found = match find(id, caller, cx) {
        Ok(found) => found,
        Err(error) => return AgentCliOutput::error(error),
    };
    let view = found.view.read(cx);
    if !view.runs_agent(cx) {
        return AgentCliOutput::error(format!("no agent is running in terminal {id}"));
    }
    if !crate::agent_context::can_paste_to_agent(view, text, cx) {
        return AgentCliOutput::error(format!(
            "the agent in terminal {id} isn't taking pasted text right now"
        ));
    }
    let terminal = view.terminal.clone();
    terminal.update(cx, |terminal, _| {
        terminal.paste(text);
        terminal.input(b"\r".to_vec());
    });
    AgentCliOutput::ok(String::new())
}

/// What `wait` saw: the terminal's agent status, `None` without an agent, or
/// the terminal gone.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
    Status(Option<AgentStatus>),
    Gone,
}

/// Whether waiting `until` is over, given what was seen now and whether the
/// agent was seen busy (working or waiting for input) since the wait started.
fn wait_is_over(until: Until, seen: Seen, was_busy: bool, waited: Duration) -> bool {
    let status = match seen {
        Seen::Gone => return true,
        Seen::Status(status) => status,
    };
    let running = status.is_some_and(AgentStatus::is_running);
    match until {
        Until::Exit => !running,
        Until::NeedsInput => !running || status == Some(AgentStatus::NeedsInput),
        // A prompt sent just before may not have started the agent yet.
        Until::Idle => match status {
            Some(AgentStatus::Idle | AgentStatus::Finished) => was_busy || waited >= WAIT_SETTLE,
            Some(AgentStatus::Working | AgentStatus::NeedsInput) => false,
            _ => true,
        },
    }
}

async fn wait(
    id: &str,
    until: Until,
    timeout: Option<Duration>,
    caller: Option<&str>,
    json_output: bool,
    cx: &mut AsyncApp,
) -> AgentCliOutput {
    let found = match cx.update(|cx| find(id, caller, cx)) {
        Ok(found) => found,
        Err(error) => return AgentCliOutput::error(error),
    };
    let view = found.view.downgrade();
    let started = Instant::now();
    let mut was_busy = false;
    loop {
        let seen = cx.update(|cx| match view.upgrade() {
            Some(view) => Seen::Status(view.read(cx).agent_status().map(|(_, status)| status)),
            None => Seen::Gone,
        });
        was_busy |= matches!(
            seen,
            Seen::Status(Some(AgentStatus::Working | AgentStatus::NeedsInput))
        );
        let waited = started.elapsed();
        if wait_is_over(until, seen, was_busy, waited) {
            let status = match seen {
                Seen::Gone => "closed",
                Seen::Status(None) => "no-agent",
                Seen::Status(Some(status)) => status_name(status),
            };
            let failed = matches!(until, Until::Idle | Until::NeedsInput)
                && matches!(
                    seen,
                    Seen::Gone | Seen::Status(None | Some(AgentStatus::Done | AgentStatus::Failed))
                );
            let mut result = AgentCliOutput::ok(if json_output {
                json(&serde_json::json!({ "id": id, "status": status }))
            } else {
                format!("{status}\n")
            });
            if failed {
                result.status = 1;
            }
            return result;
        }
        if timeout.is_some_and(|timeout| waited >= timeout) {
            return AgentCliOutput {
                stderr: format!("timed out waiting for terminal {id}"),
                status: 124,
                ..AgentCliOutput::default()
            };
        }
        cx.background_executor().timer(WAIT_POLL).await;
    }
}

/// The last `lines` non-empty lines of `text`.
fn last_lines(text: &str, lines: usize) -> String {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
}

fn output(
    id: &str,
    lines: usize,
    caller: Option<&str>,
    json_output: bool,
    cx: &mut App,
) -> AgentCliOutput {
    let found = match find(id, caller, cx) {
        Ok(found) => found,
        Err(error) => return AgentCliOutput::error(error),
    };
    let view = found.view.read(cx);
    let from_transcript = view
        .agent_session()
        .and_then(|session| session.read(cx).latest_assistant_text().map(str::to_string));
    let (source, text) = match from_transcript {
        Some(text) => ("transcript", text),
        None => (
            "terminal",
            last_lines(&view.terminal.read(cx).get_content(), lines),
        ),
    };
    AgentCliOutput::ok(if json_output {
        json(&serde_json::json!({ "id": id, "source": source, "text": text }))
    } else {
        format!("{text}\n")
    })
}

/// The window and workspace for commands that open something: the calling
/// terminal's, or else the first window's.
fn target_workspace(
    caller: Option<&str>,
    cx: &App,
) -> Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> {
    if let Some(found) = caller.and_then(|caller| find(caller, None, cx).ok()) {
        return Some((found.window, found.workspace));
    }
    cx.windows().into_iter().find_map(|window| {
        let window = window.downcast::<MultiWorkspace>()?;
        let workspace = window.read(cx).ok()?.workspace().clone();
        Some((window, workspace))
    })
}

async fn open(
    path: &str,
    caller: Option<&str>,
    cwd: Option<PathBuf>,
    cx: &mut AsyncApp,
) -> AgentCliOutput {
    let position = PathWithPosition::parse_str(path);
    let abs_path = if position.path.is_absolute() {
        position.path.clone()
    } else {
        match &cwd {
            Some(cwd) => cwd.join(&position.path),
            None => return AgentCliOutput::error("give an absolute path"),
        }
    };
    let Some((window, workspace)) = cx.update(|cx| target_workspace(caller, cx)) else {
        return AgentCliOutput::error("Solo has no window open");
    };
    let opened = window.update(cx, |_, window, cx| {
        window.activate_window();
        workspace.update(cx, |workspace, cx| {
            workspace.open_abs_path(abs_path.clone(), OpenOptions::default(), window, cx)
        })
    });
    let item = match opened {
        Ok(task) => task.await,
        Err(error) => Err(error),
    };
    let item = match item {
        Ok(item) => item,
        Err(error) => {
            return AgentCliOutput::error(format!(
                "couldn't open {}: {error:#}",
                abs_path.display()
            ));
        }
    };
    if let Some(row) = position.row {
        let point = Point::new(
            row.saturating_sub(1),
            position.column.unwrap_or(1).saturating_sub(1),
        );
        if let Some(editor) = cx.update(|cx| item.act_as::<Editor>(cx)) {
            window
                .update(cx, |_, window, cx| {
                    editor.update(cx, |editor, cx| {
                        editor.go_to_singleton_buffer_point(point, window, cx)
                    })
                })
                .ok();
        }
    }
    AgentCliOutput::ok(String::new())
}

fn review(id: &str, caller: Option<&str>, cx: &mut App) -> AgentCliOutput {
    let found = match find(id, caller, cx) {
        Ok(found) => found,
        Err(error) => return AgentCliOutput::error(error),
    };
    let last_turn = AgentTurns::try_global(cx).and_then(|turns| {
        turns
            .read(cx)
            .turns(found.view.entity_id())
            .last()
            .map(|turn| turn.number)
    });
    let Some(number) = last_turn else {
        return AgentCliOutput::error(format!(
            "the agent in terminal {id} has no turn with changes to review"
        ));
    };
    let Found {
        window,
        workspace,
        view,
    } = found;
    let opened = window.update(cx, |_, window, cx| {
        window.activate_window();
        AgentTurnReview::open(view, number, workspace, window, cx);
    });
    match opened {
        Ok(()) => AgentCliOutput::ok(String::new()),
        Err(error) => AgentCliOutput::error(format!("{error:#}")),
    }
}

/// A task name from its prompt: its first line, shortened.
fn task_name(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or_default().trim();
    let mut name: String = line.chars().take(48).collect();
    if line.chars().count() > 48 {
        name.push('…');
    }
    name
}

#[allow(clippy::too_many_arguments)]
async fn new_task(
    prompt: String,
    agent: Option<String>,
    base: Option<String>,
    name: Option<String>,
    caller: Option<&str>,
    cwd: Option<PathBuf>,
    json_output: bool,
    cx: &mut AsyncApp,
) -> AgentCliOutput {
    let Some((window, workspace)) = cx.update(|cx| target_workspace(caller, cx)) else {
        return AgentCliOutput::error("Solo has no window open");
    };
    // The repository of the calling terminal, or else of the CLI's directory.
    let directory = cx.update(|cx| {
        caller
            .and_then(|caller| find(caller, None, cx).ok())
            .and_then(|found| found.view.read(cx).terminal.read(cx).working_directory())
            .or(cwd)
    });
    let Some(directory) = directory else {
        return AgentCliOutput::error("run it in a git repository");
    };
    let (setting, command) = cx.update(|cx| {
        (
            project::project_settings::ProjectSettings::get_global(cx)
                .git
                .worktree_directory
                .clone(),
            agent.unwrap_or_else(|| TerminalSettings::get_global(cx).agent_task_command.clone()),
        )
    });
    let name = name.unwrap_or_else(|| task_name(&prompt));
    let created = cx
        .background_spawn(async move { create_task(&directory, &name, base, &setting) })
        .await;
    let task = match created {
        Ok(task) => task,
        Err(error) => return AgentCliOutput::error(format!("{error:#}")),
    };
    let Some(worktree) = task.worktree_path.clone() else {
        return AgentCliOutput::error("the task's worktree is gone");
    };
    let opened = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            crate::agent_tasks::open_task_terminal(
                workspace,
                worktree.clone(),
                task.name.clone(),
                Some(command),
                window,
                cx,
            )
        })
    });
    let view = match opened {
        Ok(task) => task.await,
        Err(error) => Err(error),
    };
    let view = match view {
        Ok(view) => view,
        Err(error) => {
            return AgentCliOutput::error(format!(
                "created the task {} but couldn't open its terminal: {error:#}",
                task.branch
            ));
        }
    };
    let id = cx.update(|cx| terminal_id(&view, cx));

    // The prompt goes to the agent once it's running and takes a paste, as
    // `send` would: typed into the shell, it would need quoting for each
    // shell.
    let started = Instant::now();
    let sent = loop {
        let ready = cx.update(|cx| {
            let view = view.read(cx);
            view.runs_agent(cx)
                && view.terminal.read(cx).bracketed_paste_enabled()
                && view
                    .agent_status()
                    .is_some_and(|(_, status)| status == AgentStatus::Idle)
        });
        if ready {
            cx.update(|cx| {
                view.read(cx).terminal.clone().update(cx, |terminal, _| {
                    terminal.paste(&prompt);
                    terminal.input(b"\r".to_vec());
                })
            });
            break true;
        }
        if started.elapsed() >= NEW_TASK_AGENT_START {
            break false;
        }
        cx.background_executor().timer(WAIT_POLL).await;
    };
    let mut result = AgentCliOutput::ok(if json_output {
        json(&serde_json::json!({
            "id": id,
            "branch": task.branch,
            "worktree": worktree,
            "prompt_sent": sent,
        }))
    } else {
        format!("{id}\n")
    });
    if !sent {
        result.stderr = if is_chinese() {
            format!("agent 没有在终端 {id} 里启动，提示词没有发送")
        } else {
            format!("the agent didn't start in terminal {id}; the prompt wasn't sent")
        };
        result.status = 1;
    }
    result
}

fn create_task(
    directory: &Path,
    name: &str,
    base: Option<String>,
    worktree_directory_setting: &str,
) -> anyhow::Result<git::AgentTask> {
    let mut info = git::repository_info(directory)?;
    if let Some(base) = base {
        info.current_branch = Some(base);
    }
    let plan = git::plan_task(&info, name, worktree_directory_setting)?;
    git::create_task(&info, &plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn command(text: &[&str]) -> Result<Command, String> {
        parse(&args(text)).map(|invocation| invocation.command)
    }

    #[test]
    fn parses_commands() {
        assert_eq!(
            parse(&args(&["ls", "--json"])),
            Ok(Invocation {
                command: Command::Ls,
                json: true
            })
        );
        assert_eq!(
            command(&["send", "a1b2", "fix", "the", "bug"]),
            Ok(Command::Send {
                id: "a1b2".into(),
                text: "fix the bug".into()
            })
        );
        assert_eq!(
            command(&["wait", "a1b2", "--until", "needs-input", "--timeout", "1.5"]),
            Ok(Command::Wait {
                id: "a1b2".into(),
                until: Until::NeedsInput,
                timeout: Some(Duration::from_millis(1500)),
            })
        );
        assert_eq!(
            command(&["wait", "self"]),
            Ok(Command::Wait {
                id: "self".into(),
                until: Until::Idle,
                timeout: None,
            })
        );
        assert_eq!(
            command(&["output", "x", "--lines", "5"]),
            Ok(Command::Output {
                id: "x".into(),
                lines: 5
            })
        );
        assert_eq!(
            command(&["open", "src/main.rs:12:3"]),
            Ok(Command::Open {
                path: "src/main.rs:12:3".into()
            })
        );
        assert_eq!(
            command(&[
                "new-task",
                "--prompt",
                "Add tests",
                "--agent",
                "codex",
                "--base",
                "main"
            ]),
            Ok(Command::NewTask {
                prompt: "Add tests".into(),
                agent: Some("codex".into()),
                base: Some("main".into()),
                name: None,
            })
        );
        assert_eq!(command(&[]), Ok(Command::Help));
        assert_eq!(command(&["ls", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn rejects_bad_commands() {
        for bad in [
            &["dance"][..],
            &["send", "x"][..],
            &["send"][..],
            &["wait", "x", "--until", "soon"][..],
            &["wait", "x", "--timeout", "-1"][..],
            &["wait", "x", "--timeout"][..],
            &["output", "x", "--lines", "0"][..],
            &["ls", "--until", "idle"][..],
            &["ls", "extra"][..],
            &["new-task"][..],
            &["new-task", "--prompt", "x", "--agent", "claude; rm -rf ~"][..],
            &["new-task", "--prompt", "x", "--base", "--force"][..],
            &["ls", "--bogus"][..],
        ] {
            assert!(command(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn waits_until_the_agent_is_done() {
        use AgentStatus::*;
        let at = Duration::from_secs(1);
        let idle = Seen::Status(Some(Idle));
        // Just after a prompt: idle doesn't count until the agent got to work,
        // or long enough passed.
        assert!(!wait_is_over(Until::Idle, idle, false, at));
        assert!(wait_is_over(Until::Idle, idle, true, at));
        assert!(wait_is_over(Until::Idle, idle, false, WAIT_SETTLE));
        assert!(wait_is_over(
            Until::Idle,
            Seen::Status(Some(Finished)),
            true,
            at
        ));
        assert!(!wait_is_over(
            Until::Idle,
            Seen::Status(Some(Working)),
            true,
            at
        ));
        assert!(!wait_is_over(
            Until::Idle,
            Seen::Status(Some(NeedsInput)),
            true,
            at
        ));
        assert!(wait_is_over(
            Until::Idle,
            Seen::Status(Some(Done)),
            false,
            at
        ));
        assert!(wait_is_over(Until::Idle, Seen::Gone, false, at));

        assert!(wait_is_over(
            Until::NeedsInput,
            Seen::Status(Some(NeedsInput)),
            false,
            at
        ));
        assert!(!wait_is_over(
            Until::NeedsInput,
            Seen::Status(Some(Working)),
            false,
            at
        ));
        assert!(wait_is_over(
            Until::NeedsInput,
            Seen::Status(None),
            false,
            at
        ));

        assert!(!wait_is_over(Until::Exit, idle, true, at));
        assert!(wait_is_over(
            Until::Exit,
            Seen::Status(Some(Failed)),
            true,
            at
        ));
        assert!(wait_is_over(Until::Exit, Seen::Status(None), true, at));
    }

    #[test]
    fn keeps_the_last_lines() {
        assert_eq!(last_lines("a\n\nb  \nc\n\n", 2), "b\nc");
        assert_eq!(last_lines("a", 5), "a");
        assert_eq!(last_lines("", 5), "");
    }

    #[test]
    fn names_tasks_after_their_prompt() {
        assert_eq!(task_name("Add tests\nfor the parser"), "Add tests");
        assert_eq!(task_name(&"x".repeat(60)).chars().count(), 49);
    }

    #[test]
    fn lists_terminals() {
        let rows = vec![TerminalRow {
            id: "a1b2c3d4".into(),
            title: "claude".into(),
            agent: Some("Claude".into()),
            status: Some("working"),
            directory: Some(PathBuf::from("/code/solo")),
            branch: Some("main".into()),
            current: true,
        }];
        assert_eq!(
            format_rows(&rows),
            format!(
                "a1b2c3d4*\tClaude\tworking\tmain\t{}\tclaude\n",
                PathBuf::from("/code/solo").display()
            )
        );
        let parsed: serde_json::Value = serde_json::from_str(&json(&rows)).unwrap();
        assert_eq!(parsed[0]["id"], "a1b2c3d4");
        assert_eq!(parsed[0]["status"], "working");
        assert_eq!(parsed[0]["current"], true);
    }
}
