//! SOLO: writing to the terminal an AI agent (Claude Code, Codex, ...) runs
//! in: code, positions and diagnostics sent from the editor, and turning on
//! Claude Code's remote control.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use collections::HashMap;
use editor::Editor;
use futures::FutureExt as _;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Global, Task, WeakEntity, Window, actions,
};
use language::{DiagnosticSeverity, Point};
use picker::{Picker, PickerDelegate};
use settings::Settings as _;
use task::RevealStrategy;
use terminal::terminal_settings::TerminalSettings;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, Toast, Workspace, item::Item as _, notifications::NotificationId};
use zed_actions::agent_terminal::{
    SendDiagnosticsToAgent, SendPositionToAgent, SendSelectionToAgent,
};

use crate::{
    TerminalView, agent_status::AgentStatus, command_blocks, default_working_directory,
    terminal_panel::TerminalPanel,
};

actions!(
    terminal,
    [
        /// Turns on Claude Code's remote control in this terminal, to carry on
        /// the session from a phone; or, when no Claude Code runs here, starts
        /// one with remote control in a new terminal.
        EnableRemoteControl,
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|editor: &mut Editor, _window, cx| {
        if !editor.mode().is_full() {
            return;
        }
        let editor_handle = cx.weak_entity();
        editor
            .register_action({
                let editor = editor_handle.clone();
                move |action: &SendSelectionToAgent, window, cx| {
                    send_from_editor(
                        &editor,
                        action.choose_terminal,
                        selection_context,
                        window,
                        cx,
                    )
                }
            })
            .detach();
        editor
            .register_action({
                let editor = editor_handle.clone();
                move |action: &SendPositionToAgent, window, cx| {
                    send_from_editor(
                        &editor,
                        action.choose_terminal,
                        position_context,
                        window,
                        cx,
                    )
                }
            })
            .detach();
        editor
            .register_action({
                let editor = editor_handle;
                move |action: &SendDiagnosticsToAgent, window, cx| {
                    send_from_editor(
                        &editor,
                        action.choose_terminal,
                        diagnostics_context,
                        window,
                        cx,
                    )
                }
            })
            .detach();
    })
    .detach();

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        // Reached when no terminal has focus; a focused one handles it itself.
        workspace.register_action(|workspace, _: &EnableRemoteControl, window, cx| {
            start_claude_with_remote_control(workspace, window, cx)
        });
    })
    .detach();
}

/// When each terminal last had focus, to send to the agent used last.
#[derive(Default)]
struct TerminalFocusHistory(HashMap<EntityId, Instant>);

impl Global for TerminalFocusHistory {}

pub(crate) fn record_terminal_focus(view: EntityId, cx: &mut App) {
    cx.default_global::<TerminalFocusHistory>()
        .0
        .insert(view, Instant::now());
}

fn last_focused(view: EntityId, cx: &App) -> Option<Instant> {
    cx.try_global::<TerminalFocusHistory>()?
        .0
        .get(&view)
        .copied()
}

/// Orders terminals by when they last had focus, most recent first; ones that
/// never had it keep their order, after the others.
pub(crate) fn most_recently_focused_first(views: &mut [Entity<TerminalView>], cx: &App) {
    views.sort_by_key(|view| std::cmp::Reverse(last_focused(view.entity_id(), cx)));
}

/// Whether `message` can go to the prompt of the agent in `view` as one
/// paste: an agent still runs there, and a message of several lines arrives
/// whole (with bracketed paste). Otherwise the shell, or the agent, would take
/// each line as a command or prompt of its own.
pub(crate) fn can_paste_to_agent(view: &TerminalView, message: &str, cx: &App) -> bool {
    view.runs_agent(cx)
        && (!message.trim_end().contains('\n') || view.terminal.read(cx).bracketed_paste_enabled())
}

impl TerminalView {
    /// Whether an AI agent is running in this terminal now.
    pub(crate) fn runs_agent(&self, cx: &App) -> bool {
        let extra = &TerminalSettings::get_global(cx).agent_commands;
        let running_agent_command = self
            .terminal
            .read(cx)
            .shell_integration()
            .blocks()
            .last()
            .filter(|block| block.is_running())
            .and_then(|block| block.command.as_deref())
            .is_some_and(|command| command_blocks::is_agent_command(command, extra));
        running_agent_command
            || matches!(
                self.agent_status(),
                Some((
                    _,
                    AgentStatus::Working
                        | AgentStatus::NeedsInput
                        | AgentStatus::Idle
                        | AgentStatus::Finished
                ))
            )
    }
}

/// Terminals running an AI agent, the most recently focused first.
fn agent_terminals(workspace: &Entity<Workspace>, cx: &App) -> Vec<Entity<TerminalView>> {
    let mut views: Vec<_> = command_blocks::terminal_views(workspace, cx)
        .into_iter()
        .filter(|view| view.read(cx).runs_agent(cx))
        .collect();
    most_recently_focused_first(&mut views, cx);
    views
}

/// Shows `view`, opening the terminal panel if it's in there, and focuses it.
pub(crate) fn reveal_terminal_view(
    workspace: &Entity<Workspace>,
    view: &Entity<TerminalView>,
    window: &mut Window,
    cx: &mut App,
) {
    let panel_pane = workspace
        .read(cx)
        .panel::<TerminalPanel>(cx)
        .and_then(|panel| {
            panel
                .read(cx)
                .center
                .panes()
                .into_iter()
                .find(|pane| pane.read(cx).index_for_item(view).is_some())
                .cloned()
        });
    match panel_pane {
        Some(pane) => {
            workspace.update(cx, |workspace, cx| {
                workspace.open_panel::<TerminalPanel>(window, cx)
            });
            pane.update(cx, |pane, cx| {
                if let Some(index) = pane.index_for_item(view) {
                    pane.activate_item(index, true, true, window, cx);
                }
            });
            let focus_handle = view.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
        }
        None => {
            workspace.update(cx, |workspace, cx| {
                workspace.activate_item(view, true, true, window, cx);
            });
        }
    }
}

fn show_toast<T: 'static>(workspace: &Entity<Workspace>, message: impl Into<String>, cx: &mut App) {
    let message = message.into();
    workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(NotificationId::unique::<T>(), message).autohide(),
            cx,
        );
    });
}

// Sending from the editor.

/// What to send to an agent, with absolute paths: they're shown relative to
/// the directory the agent runs in, known once the terminal is picked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AgentContext {
    Selections(Vec<SelectedCode>),
    Position {
        path: PathBuf,
        line: u32,
    },
    Diagnostics {
        path: PathBuf,
        diagnostics: Vec<DiagnosticLine>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SelectedCode {
    pub path: PathBuf,
    /// 1-based, inclusive.
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DiagnosticLine {
    /// 1-based.
    pub line: u32,
    pub column: u32,
    pub is_error: bool,
    pub message: String,
    pub source: Option<String>,
}

/// `path` as the agent should see it: relative to the directory it runs in
/// when inside it (with `/`, which every agent and shell takes), else as is.
pub(crate) fn display_path(path: &Path, agent_directory: Option<&Path>) -> String {
    if let Some(relative) = agent_directory
        .and_then(|directory| path.strip_prefix(directory).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
    {
        return relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
    }
    path.to_string_lossy().into_owned()
}

/// A Markdown code fence that the code can't end early.
fn code_fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

fn line_range(start_line: u32, end_line: u32) -> String {
    if start_line == end_line {
        start_line.to_string()
    } else {
        format!("{start_line}-{end_line}")
    }
}

impl AgentContext {
    /// The text to paste into the agent's prompt. Multi-line text ends with a
    /// newline, so what the user types next starts on a line of its own.
    pub(crate) fn message(&self, agent_directory: Option<&Path>) -> String {
        match self {
            AgentContext::Selections(selections) => {
                let mut message = String::new();
                for selection in selections {
                    let path = display_path(&selection.path, agent_directory);
                    let fence = code_fence(&selection.text);
                    let language = selection
                        .path
                        .extension()
                        .map(|extension| extension.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let code = selection.text.strip_suffix('\n').unwrap_or(&selection.text);
                    let code = code.strip_suffix('\r').unwrap_or(code);
                    message.push_str(&format!(
                        "{path}:{}\n{fence}{language}\n{code}\n{fence}\n",
                        line_range(selection.start_line, selection.end_line)
                    ));
                }
                message
            }
            AgentContext::Position { path, line } => {
                format!("@{}:{line} ", display_path(path, agent_directory))
            }
            AgentContext::Diagnostics { path, diagnostics } => {
                let path = display_path(path, agent_directory);
                let mut message = format!("Errors and warnings in {path}:\n");
                for diagnostic in diagnostics {
                    let severity = if diagnostic.is_error {
                        "error"
                    } else {
                        "warning"
                    };
                    let source = diagnostic
                        .source
                        .as_ref()
                        .map(|source| format!(" [{source}]"))
                        .unwrap_or_default();
                    // Continuation lines are indented to keep each diagnostic one item.
                    let text = diagnostic.message.trim_end().replace('\n', "\n  ");
                    message.push_str(&format!(
                        "- {path}:{}:{} {severity}{source}: {text}\n",
                        diagnostic.line, diagnostic.column
                    ));
                }
                message
            }
        }
    }
}

fn buffer_path(buffer: &language::BufferSnapshot, cx: &App) -> Option<PathBuf> {
    let file = buffer.file()?;
    Some(match file.as_local() {
        Some(local) => local.abs_path(cx),
        None => file.full_path(cx),
    })
}

/// Why nothing can be sent, for the toast.
type ContextResult = Result<AgentContext, &'static str>;

fn selection_context(editor: &mut Editor, cx: &mut Context<Editor>) -> ContextResult {
    let display_snapshot = editor.display_snapshot(cx);
    let selections = editor.selections.all::<Point>(&display_snapshot);
    if selections.iter().all(|selection| selection.is_empty()) {
        return position_context(editor, cx);
    }
    let multibuffer = editor.buffer().read(cx).snapshot(cx);
    let selected = selections
        .iter()
        .filter(|selection| !selection.is_empty())
        .filter_map(|selection| {
            let (buffer, range) = multibuffer.range_to_buffer_range(selection.range())?;
            let path = buffer_path(buffer, cx)?;
            let start_line = range.start.row + 1;
            let mut end_line = range.end.row + 1;
            // A selection of whole lines ends at the start of the next one.
            if range.end.column == 0 && end_line > start_line {
                end_line -= 1;
            }
            Some(SelectedCode {
                path,
                start_line,
                end_line,
                text: buffer.text_for_range(range).collect(),
            })
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(util::i18n::tr(
            "The selection isn't in a saved file",
            "选区不在已保存的文件里",
        ));
    }
    Ok(AgentContext::Selections(selected))
}

fn position_context(editor: &mut Editor, cx: &mut Context<Editor>) -> ContextResult {
    let display_snapshot = editor.display_snapshot(cx);
    let head = editor.selections.newest::<Point>(&display_snapshot).head();
    let multibuffer = editor.buffer().read(cx).snapshot(cx);
    multibuffer
        .point_to_buffer_point(head)
        .and_then(|(buffer, point)| {
            Some(AgentContext::Position {
                path: buffer_path(buffer, cx)?,
                line: point.row + 1,
            })
        })
        .ok_or(util::i18n::tr(
            "The cursor isn't in a saved file",
            "光标不在已保存的文件里",
        ))
}

fn diagnostics_context(editor: &mut Editor, cx: &mut Context<Editor>) -> ContextResult {
    let not_in_file = util::i18n::tr("The cursor isn't in a saved file", "光标不在已保存的文件里");
    let buffer = editor.active_buffer(cx).ok_or(not_in_file)?;
    let snapshot = buffer.read(cx).snapshot();
    let path = buffer_path(&snapshot, cx).ok_or(not_in_file)?;
    let mut diagnostics = snapshot
        .diagnostics_in_range::<_, Point>(0..snapshot.len(), false)
        .filter(|entry| entry.diagnostic.is_primary)
        .filter(|entry| {
            entry.diagnostic.severity == DiagnosticSeverity::ERROR
                || entry.diagnostic.severity == DiagnosticSeverity::WARNING
        })
        .map(|entry| DiagnosticLine {
            line: entry.range.start.row + 1,
            column: entry.range.start.column + 1,
            is_error: entry.diagnostic.severity == DiagnosticSeverity::ERROR,
            message: entry.diagnostic.message.to_string(),
            source: entry.diagnostic.source.clone(),
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return Err(util::i18n::tr(
            "No errors or warnings in this file",
            "这个文件没有错误或警告",
        ));
    }
    diagnostics.sort_by_key(|diagnostic| (diagnostic.line, diagnostic.column));
    Ok(AgentContext::Diagnostics { path, diagnostics })
}

fn send_from_editor(
    editor: &WeakEntity<Editor>,
    choose_terminal: bool,
    gather: fn(&mut Editor, &mut Context<Editor>) -> ContextResult,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((workspace, context)) = editor
        .update(cx, |editor, cx| {
            Some((editor.workspace()?, gather(editor, cx)))
        })
        .ok()
        .flatten()
    else {
        return;
    };
    let context = match context {
        Ok(context) => context,
        Err(reason) => {
            struct NothingToSendToast;
            show_toast::<NothingToSendToast>(&workspace, reason, cx);
            return;
        }
    };
    let agents = agent_terminals(&workspace, cx);
    let Some(last_used) = agents.first() else {
        cx.write_to_clipboard(ClipboardItem::new_string(context.message(None)));
        struct NoAgentToast;
        show_toast::<NoAgentToast>(
            &workspace,
            util::i18n::tr(
                "No terminal is running an AI agent; copied to the clipboard instead",
                "没有终端在运行 AI agent，已改为复制到剪贴板",
            ),
            cx,
        );
        return;
    };
    if !choose_terminal || agents.len() == 1 {
        let last_used = last_used.clone();
        send_to_agent_terminal(&workspace, &last_used, &context, window, cx);
        return;
    }
    let weak_workspace = workspace.downgrade();
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_modal(window, cx, |window, cx| {
            AgentTerminalPicker::new(weak_workspace, agents, context, window, cx)
        })
    });
}

/// Pastes `context` into the agent's prompt and switches to its terminal.
fn send_to_agent_terminal(
    workspace: &Entity<Workspace>,
    agent: &Entity<TerminalView>,
    context: &AgentContext,
    window: &mut Window,
    cx: &mut App,
) {
    let terminal = agent.read(cx).terminal.clone();
    let directory = terminal.read(cx).working_directory();
    let message = context.message(directory.as_deref());
    // Without bracketed paste each line would reach the agent as a prompt of its own.
    if message.trim_end().contains('\n') && !terminal.read(cx).bracketed_paste_enabled() {
        cx.write_to_clipboard(ClipboardItem::new_string(message));
        struct NoBracketedPasteToast;
        show_toast::<NoBracketedPasteToast>(
            workspace,
            util::i18n::tr(
                "The agent isn't accepting pasted text right now; copied to the clipboard instead",
                "agent 现在不接受粘贴，已改为复制到剪贴板",
            ),
            cx,
        );
        return;
    }
    terminal.update(cx, |terminal, _| terminal.paste(&message));
    reveal_terminal_view(workspace, agent, window, cx);
}

struct AgentTerminalPicker {
    picker: Entity<Picker<AgentTerminalPickerDelegate>>,
}

impl AgentTerminalPicker {
    fn new(
        workspace: WeakEntity<Workspace>,
        agents: Vec<Entity<TerminalView>>,
        context: AgentContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let labels = agents
            .iter()
            .map(|agent| {
                let view = agent.read(cx);
                let title = view.tab_content_text(0, cx);
                match view.agent_status() {
                    Some((name, status)) => format!("{name} · {} — {title}", status.label()).into(),
                    None => title,
                }
            })
            .collect();
        let delegate = AgentTerminalPickerDelegate {
            picker: cx.entity().downgrade(),
            workspace,
            agents,
            labels,
            context,
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::nonsearchable_uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for AgentTerminalPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for AgentTerminalPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for AgentTerminalPicker {}
impl ModalView for AgentTerminalPicker {}

struct AgentTerminalPickerDelegate {
    picker: WeakEntity<AgentTerminalPicker>,
    workspace: WeakEntity<Workspace>,
    agents: Vec<Entity<TerminalView>>,
    labels: Vec<SharedString>,
    context: AgentContext,
    selected_index: usize,
}

impl PickerDelegate for AgentTerminalPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "agent terminal picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        util::i18n::tr("Send to which agent?", "发送给哪个 agent？").into()
    }

    fn match_count(&self) -> usize {
        self.agents.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        _query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let agent = self.agents.get(self.selected_index).cloned();
        let workspace = self.workspace.upgrade();
        let context = self.context.clone();
        self.dismissed(window, cx);
        if let Some((agent, workspace)) = agent.zip(workspace) {
            window.defer(cx, move |window, cx| {
                send_to_agent_terminal(&workspace, &agent, &context, window, cx)
            });
        }
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(Icon::new(IconName::Terminal).color(Color::Muted))
                .child(Label::new(self.labels.get(ix)?.clone()).truncate()),
        )
    }
}

// Remote control.

/// What turning on remote control does for a terminal, by the agent in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteControl {
    /// Claude Code waits for a prompt: type `/remote-control` into it.
    Type,
    /// Claude Code is busy; typing now would queue up behind its turn.
    WaitUntilIdle,
    /// No Claude Code runs here: start one with remote control elsewhere.
    StartNew,
}

pub(crate) fn remote_control_for(agent: Option<(&str, AgentStatus)>) -> RemoteControl {
    match agent {
        Some((name, status)) if name.eq_ignore_ascii_case("claude") => match status {
            // A prompt waiting for an answer would take the typed Enter as one.
            AgentStatus::Working | AgentStatus::NeedsInput => RemoteControl::WaitUntilIdle,
            AgentStatus::Idle | AgentStatus::Finished => RemoteControl::Type,
            AgentStatus::Done | AgentStatus::Failed => RemoteControl::StartNew,
        },
        _ => RemoteControl::StartNew,
    }
}

const REMOTE_CONTROL_COMMAND: &str = "/remote-control";
/// Enter goes in a write of its own, so the agent reads it as a key press
/// rather than as part of the text typed before it.
const ENTER_DELAY: Duration = Duration::from_millis(100);

pub(crate) fn remote_control_label() -> &'static str {
    util::i18n::tr("Enable Remote Control", "开启手机控制")
}

impl TerminalView {
    /// Whether the menus offer to turn on remote control: Claude Code runs here.
    pub(crate) fn offers_remote_control(&self) -> bool {
        !self.read_only && remote_control_for(self.agent_status()) != RemoteControl::StartNew
    }

    pub(crate) fn enable_remote_control(
        &mut self,
        _: &EnableRemoteControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let remote_control = if self.read_only {
            RemoteControl::StartNew
        } else {
            remote_control_for(self.agent_status())
        };
        match remote_control {
            RemoteControl::Type => {
                self.terminal.update(cx, |terminal, _| {
                    terminal.input(REMOTE_CONTROL_COMMAND.as_bytes().to_vec())
                });
                let terminal = self.terminal.downgrade();
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(ENTER_DELAY).await;
                    terminal
                        .update(cx, |terminal, _| terminal.input(b"\r".to_vec()))
                        .ok();
                })
                .detach();
            }
            RemoteControl::WaitUntilIdle => {
                struct ClaudeBusyToast;
                show_toast::<ClaudeBusyToast>(
                    &workspace,
                    util::i18n::tr(
                        "Claude is working. Enable remote control once it's idle.",
                        "Claude 正在工作，等它空闲后再开启手机控制",
                    ),
                    cx,
                );
            }
            RemoteControl::StartNew => workspace.update(cx, |workspace, cx| {
                start_claude_with_remote_control(workspace, window, cx)
            }),
        }
    }
}

const SHELL_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a terminal and runs `claude --remote-control` in it once its shell is up.
fn start_claude_with_remote_control(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = workspace.panel::<TerminalPanel>(cx) else {
        return;
    };
    let working_directory = default_working_directory(workspace, cx);
    let new_terminal = panel.update(cx, |panel, cx| {
        panel.add_terminal_shell(false, working_directory, RevealStrategy::Always, window, cx)
    });
    cx.spawn(async move |_, cx| {
        let terminal = new_terminal.await?;
        let input = b"claude --remote-control\r".to_vec();
        let is_pty = terminal.read_with(cx, |terminal, _| terminal.is_pty())?;
        if !is_pty {
            terminal.update(cx, |terminal, _| terminal.write_init_command(input))?;
            return anyhow::Ok(());
        }
        let startup = terminal.update(cx, |terminal, _| {
            terminal.start_init_command_startup_handshake()
        })?;
        let timeout = cx.background_executor().timer(SHELL_STARTUP_TIMEOUT);
        futures::select_biased! {
            _ = startup.fuse() => {}
            _ = timeout.fuse() => {}
        }
        terminal.update(cx, |terminal, cx| {
            if !terminal.write_init_command_after_startup(input, cx) {
                log::debug!("not starting Claude Code: the terminal got input first");
            }
        })?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn test_display_path() {
        let root = std::env::temp_dir().join("project");
        let file = root.join("src").join("main.rs");
        assert_eq!(display_path(&file, Some(&root)), "src/main.rs");
        assert_eq!(
            display_path(&file, Some(&std::env::temp_dir().join("other"))),
            file.to_string_lossy()
        );
        assert_eq!(display_path(&file, None), file.to_string_lossy());
    }

    #[test]
    fn test_selection_message() {
        let text = (1..=10).map(|n| format!("line {n}\n")).collect::<String>();
        let context = AgentContext::Selections(vec![SelectedCode {
            path: path("src/lib.rs"),
            start_line: 11,
            end_line: 20,
            text: text.clone(),
        }]);
        let message = context.message(None);
        assert_eq!(
            message,
            format!("src/lib.rs:11-20\n```rs\n{}\n```\n", text.trim_end())
        );

        let one_line = AgentContext::Selections(vec![SelectedCode {
            path: path("Makefile"),
            start_line: 3,
            end_line: 3,
            text: "all: build".into(),
        }]);
        assert_eq!(one_line.message(None), "Makefile:3\n```\nall: build\n```\n");
    }

    #[test]
    fn test_selection_containing_a_fence() {
        let context = AgentContext::Selections(vec![SelectedCode {
            path: path("README.md"),
            start_line: 1,
            end_line: 3,
            text: "```sh\nls\n```".into(),
        }]);
        assert_eq!(
            context.message(None),
            "README.md:1-3\n````md\n```sh\nls\n```\n````\n"
        );
    }

    #[test]
    fn test_position_message() {
        let context = AgentContext::Position {
            path: path("src/main.rs"),
            line: 42,
        };
        assert_eq!(context.message(None), "@src/main.rs:42 ");
    }

    #[test]
    fn test_diagnostics_message() {
        let context = AgentContext::Diagnostics {
            path: path("src/main.rs"),
            diagnostics: vec![
                DiagnosticLine {
                    line: 3,
                    column: 5,
                    is_error: true,
                    message: "mismatched types\nexpected `u32`".into(),
                    source: Some("rustc".into()),
                },
                DiagnosticLine {
                    line: 9,
                    column: 1,
                    is_error: false,
                    message: "unused variable".into(),
                    source: None,
                },
            ],
        };
        assert_eq!(
            context.message(None),
            "Errors and warnings in src/main.rs:\n\
             - src/main.rs:3:5 error [rustc]: mismatched types\n  expected `u32`\n\
             - src/main.rs:9:1 warning: unused variable\n"
        );
    }

    #[test]
    fn test_remote_control_for() {
        use AgentStatus::*;
        assert_eq!(
            remote_control_for(Some(("claude", Idle))),
            RemoteControl::Type
        );
        assert_eq!(
            remote_control_for(Some(("Claude", NeedsInput))),
            RemoteControl::WaitUntilIdle
        );
        assert_eq!(
            remote_control_for(Some(("claude", Working))),
            RemoteControl::WaitUntilIdle
        );
        assert_eq!(
            remote_control_for(Some(("claude", Done))),
            RemoteControl::StartNew
        );
        assert_eq!(
            remote_control_for(Some(("claude", Failed))),
            RemoteControl::StartNew
        );
        assert_eq!(
            remote_control_for(Some(("codex", Idle))),
            RemoteControl::StartNew
        );
        assert_eq!(remote_control_for(None), RemoteControl::StartNew);
    }
}
