//! SOLO: the agent board, a panel listing every terminal that runs or ran an
//! AI agent and every agent task: what each agent is doing, its branch, what
//! its last turn changed, how long it has run and what it last said. Agents
//! that need input come first; clicking one shows its terminal.
//!
//! It follows the terminals' own state (status, session, turns) through their
//! events rather than reading anything from disk on a timer. Only the list of
//! agent tasks comes from git, read again when terminals come and go.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use agent_sessions::AgentSessionEvent;
use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Pixels, Render, Subscription, Task, WeakEntity, Window, actions, px,
};
use ui::{Tab, Tooltip, prelude::*};
use util::i18n::{is_chinese, tr};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    item::ItemEvent,
};
use zed_actions::agent_task::ManageAgentTasks;

use crate::{
    TerminalView,
    agent_status::AgentStatus,
    agent_tasks::agent_tasks,
    agent_turns::AgentTurns,
    command_blocks::{activate_terminal_view, terminal_views},
    terminal_panel::TerminalPanel,
};

actions!(
    agent_board,
    [
        /// Toggles focus on the agent board, which lists the AI agents running
        /// in terminals and the agent tasks.
        ToggleFocus,
    ]
);

const AGENT_BOARD_KEY: &str = "AgentBoard";
/// How often running times are brought up to date while the board is shown.
const TICK: Duration = Duration::from_secs(1);
const MAX_SUMMARY_CHARS: usize = 200;

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<AgentBoard>(window, cx);
        });
    })
    .detach();
}

/// What the board shows of a terminal running an agent.
#[derive(Clone, Debug, PartialEq)]
struct TerminalSnapshot {
    terminal: EntityId,
    title: String,
    status: AgentStatus,
    branch: Option<String>,
    changed_files: Option<usize>,
    elapsed: Option<Duration>,
    last_message: Option<String>,
}

/// An agent task, with the terminals open in its worktree.
#[derive(Clone, Debug, PartialEq)]
struct TaskSnapshot {
    name: String,
    branch: String,
    worktree_path: Option<PathBuf>,
    terminals: Vec<EntityId>,
}

#[derive(Clone, Debug, PartialEq)]
struct BoardRow {
    /// The terminal clicking the row shows, if any.
    terminal: Option<EntityId>,
    title: String,
    /// `None` for a task none of whose terminals runs an agent.
    status: Option<AgentStatus>,
    task: Option<String>,
    branch: Option<String>,
    changed_files: Option<usize>,
    elapsed: Option<String>,
    summary: Option<String>,
    worktree_path: Option<PathBuf>,
}

/// Where a row goes: agents that need input first, then working ones, then
/// idle and finished ones, then tasks without an agent.
fn status_rank(status: Option<AgentStatus>) -> u8 {
    match status {
        Some(AgentStatus::NeedsInput) => 0,
        Some(AgentStatus::Working) => 1,
        Some(AgentStatus::Idle) => 2,
        Some(AgentStatus::Done | AgentStatus::Failed) => 3,
        None => 4,
    }
}

/// The board's rows: a row per agent terminal, named after its task if it
/// runs in one, and a row per task without an agent terminal. Rows of the
/// same rank keep the terminals' order.
fn board_rows(
    terminals: Vec<TerminalSnapshot>,
    tasks: &[TaskSnapshot],
    chinese: bool,
) -> Vec<BoardRow> {
    let mut rows: Vec<BoardRow> = terminals
        .iter()
        .map(|terminal| {
            let task = tasks
                .iter()
                .find(|task| task.terminals.contains(&terminal.terminal));
            BoardRow {
                terminal: Some(terminal.terminal),
                title: terminal.title.clone(),
                status: Some(terminal.status),
                task: task.map(|task| task.name.clone()),
                branch: task
                    .map(|task| task.branch.clone())
                    .or_else(|| terminal.branch.clone()),
                changed_files: terminal.changed_files,
                elapsed: terminal
                    .elapsed
                    .map(|elapsed| format_elapsed(elapsed, chinese)),
                summary: terminal.last_message.as_deref().and_then(summarize),
                worktree_path: task.and_then(|task| task.worktree_path.clone()),
            }
        })
        .collect();
    rows.extend(
        tasks
            .iter()
            .filter(|task| {
                !task
                    .terminals
                    .iter()
                    .any(|id| terminals.iter().any(|terminal| terminal.terminal == *id))
            })
            .map(|task| BoardRow {
                terminal: task.terminals.first().copied(),
                title: task.name.clone(),
                status: None,
                task: Some(task.name.clone()),
                branch: Some(task.branch.clone()),
                changed_files: None,
                elapsed: None,
                summary: None,
                worktree_path: task.worktree_path.clone(),
            }),
    );
    rows.sort_by_key(|row| status_rank(row.status));
    rows
}

/// A running time, briefly: 45s, 3m, 1h 5m.
fn format_elapsed(elapsed: Duration, chinese: bool) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes) = (seconds / 3600, seconds / 60 % 60);
    match (chinese, seconds) {
        (false, 0..60) => format!("{seconds}s"),
        (false, 60..3600) => format!("{minutes}m"),
        (false, _) => format!("{hours}h {minutes}m"),
        (true, 0..60) => format!("{seconds} 秒"),
        (true, 60..3600) => format!("{minutes} 分钟"),
        (true, _) => format!("{hours} 小时 {minutes} 分"),
    }
}

/// An agent's message on one line, shortened.
fn summarize(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    match line.char_indices().nth(MAX_SUMMARY_CHARS) {
        Some((end, _)) => Some(format!("{}…", &line[..end])),
        None => Some(line),
    }
}

fn changed_files_label(count: usize, chinese: bool) -> String {
    if chinese {
        format!("改动 {count} 个文件")
    } else if count == 1 {
        "1 file changed".to_string()
    } else {
        format!("{count} files changed")
    }
}

fn terminal_snapshot(
    view: &Entity<TerminalView>,
    turns: Option<&AgentTurns>,
    now: Instant,
    cx: &App,
) -> Option<TerminalSnapshot> {
    let terminal_view = view.read(cx);
    let (_, status) = terminal_view.agent_status()?;
    let terminal = terminal_view.terminal.read(cx);
    let elapsed = terminal
        .shell_integration()
        .blocks()
        .iter()
        .rev()
        .find(|block| block.started_at.is_some())
        .and_then(|block| {
            let started_at = block.started_at?;
            Some(
                block
                    .finished_at
                    .unwrap_or(now)
                    .saturating_duration_since(started_at),
            )
        });
    let last_message = terminal_view.agent_session().and_then(|session| {
        session
            .read(cx)
            .latest_assistant_text()
            .map(ToString::to_string)
    });
    Some(TerminalSnapshot {
        terminal: view.entity_id(),
        title: terminal_view.smart_title(0, true, cx),
        status,
        branch: terminal_view.cached_git_branch(cx).map(ToString::to_string),
        changed_files: turns.and_then(|turns| turns.last_turn_changed_files(view.entity_id())),
        elapsed,
        last_message,
    })
}

/// A terminal the board follows.
struct WatchedTerminal {
    session: Option<EntityId>,
    _subscriptions: [Subscription; 2],
    _session_subscription: Option<Subscription>,
}

pub struct AgentBoard {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    position: DockPosition,
    active: bool,
    rows: Vec<BoardRow>,
    tasks: Vec<TaskSnapshot>,
    watched: HashMap<EntityId, WatchedTerminal>,
    list_tasks: Task<()>,
    tick: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AgentBoard {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |_, _window, cx| {
            let workspace_entity = cx.entity();
            cx.new(|cx| Self::new(workspace_entity, cx))
        })
    }

    fn new(workspace_entity: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        let board = cx.weak_entity();
        let agent_turns = AgentTurns::global(cx);
        let subscriptions = vec![
            cx.subscribe(&workspace_entity, |this, _, event, cx| match event {
                workspace::Event::ItemAdded { item } => {
                    if item.downcast::<TerminalView>().is_some() {
                        this.terminals_changed(cx);
                    }
                }
                workspace::Event::ItemRemoved { .. }
                | workspace::Event::PaneRemoved
                | workspace::Event::PanelAdded(_) => this.refresh(cx),
                _ => {}
            }),
            cx.observe(&agent_turns, |this, _, cx| this.refresh(cx)),
            // Terminals in the terminal panel's panes aren't announced by the
            // workspace.
            cx.observe_new(move |_: &mut TerminalView, _, cx| {
                let board = board.clone();
                cx.defer(move |cx| {
                    board
                        .update(cx, |board, cx| board.terminals_changed(cx))
                        .ok();
                });
            }),
        ];
        // Not now: the workspace is being updated while it adds the board.
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| this.terminals_changed(cx)).ok();
        })
        .detach();
        Self {
            workspace: workspace_entity.downgrade(),
            focus_handle: cx.focus_handle(),
            position: DockPosition::Left,
            active: false,
            rows: Vec::new(),
            tasks: Vec::new(),
            watched: HashMap::new(),
            list_tasks: Task::ready(()),
            tick: None,
            _subscriptions: subscriptions,
        }
    }

    fn terminals_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
        self.refresh_tasks(cx);
    }

    /// Lists the agent tasks again, in the background.
    fn refresh_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let list = agent_tasks(&workspace, cx);
        self.list_tasks = cx.spawn(async move |this, cx| {
            let tasks = match list.await {
                Ok(tasks) => tasks,
                Err(error) => {
                    log::warn!("failed to list the agent tasks: {error:#}");
                    return;
                }
            };
            let tasks = tasks
                .into_iter()
                .map(|entry| TaskSnapshot {
                    name: entry.task.name,
                    branch: entry.task.branch,
                    worktree_path: entry.task.worktree_path,
                    terminals: entry.terminals.iter().map(Entity::entity_id).collect(),
                })
                .collect();
            this.update(cx, |this, cx| {
                this.tasks = tasks;
                this.refresh(cx);
            })
            .ok();
        });
    }

    /// Follows the workspace's terminals and their sessions, and builds the
    /// rows again, redrawing if they changed.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let views = terminal_views(&workspace, cx);
        self.watched
            .retain(|id, _| views.iter().any(|view| view.entity_id() == *id));
        for view in &views {
            let session = view.read(cx).agent_session().cloned();
            let session_id = session.as_ref().map(Entity::entity_id);
            let watched = self
                .watched
                .entry(view.entity_id())
                .or_insert_with(|| WatchedTerminal {
                    session: None,
                    _subscriptions: [
                        cx.subscribe(view, |this, _, _: &ItemEvent, cx| this.refresh(cx)),
                        cx.observe_release(view, |this, _, cx| this.refresh(cx)),
                    ],
                    _session_subscription: None,
                });
            if watched.session != session_id {
                watched.session = session_id;
                watched._session_subscription = session.map(|session| {
                    cx.subscribe(&session, |this, _, _: &AgentSessionEvent, cx| {
                        this.refresh(cx)
                    })
                });
            }
        }

        let now = Instant::now();
        let turns = AgentTurns::try_global(cx).map(|turns| turns.read(cx));
        let terminals = views
            .iter()
            .filter_map(|view| terminal_snapshot(view, turns, now, cx))
            .collect();
        let rows = board_rows(terminals, &self.tasks, is_chinese());
        if rows != self.rows {
            self.rows = rows;
            cx.notify();
        }
        self.update_tick(cx);
    }

    /// Keeps running times up to date while the board is shown and an agent
    /// runs.
    fn update_tick(&mut self, cx: &mut Context<Self>) {
        let running = self.rows.iter().any(|row| {
            matches!(
                row.status,
                Some(AgentStatus::Working | AgentStatus::NeedsInput | AgentStatus::Idle)
            )
        });
        if !(self.active && running) {
            self.tick = None;
        } else if self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    fn needs_input_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.status == Some(AgentStatus::NeedsInput))
            .count()
    }

    fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let view = row.terminal.and_then(|terminal| {
            terminal_views(&workspace, cx)
                .into_iter()
                .find(|view| view.entity_id() == terminal)
        });
        let Some(view) = view else {
            window.dispatch_action(ManageAgentTasks.boxed_clone(), cx);
            return;
        };
        let in_terminal_panel =
            workspace
                .read(cx)
                .panel::<TerminalPanel>(cx)
                .is_some_and(|panel| {
                    panel
                        .read(cx)
                        .center
                        .panes()
                        .into_iter()
                        .any(|pane| pane.read(cx).index_for_item(&view).is_some())
                });
        if in_terminal_panel {
            workspace.update(cx, |workspace, cx| {
                workspace.open_panel::<TerminalPanel>(window, cx)
            });
        }
        activate_terminal_view(&workspace, &view, window, cx);
    }

    fn render_row(&self, index: usize, row: &BoardRow, cx: &mut Context<Self>) -> AnyElement {
        let chinese = is_chinese();
        let (icon, color) = match row.status {
            Some(status) => status.icon(),
            None => (IconName::GitBranch, Color::Muted),
        };
        let status_label = match row.status {
            Some(status) => status.label(),
            None => tr("No agent running", "没有运行的 agent"),
        };
        let needs_input = row.status == Some(AgentStatus::NeedsInput);
        let mut details = vec![status_label.to_string()];
        if let Some(task) = &row.task
            && *task != row.title
        {
            details.push(task.clone());
        }
        if let Some(branch) = &row.branch {
            details.push(branch.clone());
        }
        if let Some(count) = row.changed_files.filter(|count| *count > 0) {
            details.push(changed_files_label(count, chinese));
        }
        let tooltip = row
            .worktree_path
            .as_ref()
            .map(|path| path.display().to_string());
        let warning_background = cx.theme().status().warning_background.opacity(0.25);
        let warning_border = cx.theme().status().warning;
        let hover_background = cx.theme().colors().element_hover;
        v_flex()
            .id(("agent-board-row", index))
            .w_full()
            .px_2()
            .py_1()
            .gap_0p5()
            .rounded_sm()
            .cursor_pointer()
            .when(needs_input, |this| {
                this.bg(warning_background)
                    .border_l_2()
                    .border_color(warning_border)
            })
            .hover(move |style| style.bg(hover_background))
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(Tooltip::text(tooltip))
            })
            .on_click(cx.listener(move |this, _, window, cx| this.open_row(index, window, cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .child(Icon::new(icon).size(IconSize::Small).color(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(row.title.clone()).truncate()),
                    )
                    .when_some(row.elapsed.clone(), |this, elapsed| {
                        this.child(
                            Label::new(elapsed)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(
                Label::new(details.join(" · "))
                    .size(LabelSize::Small)
                    .color(if needs_input {
                        Color::Warning
                    } else {
                        Color::Muted
                    })
                    .truncate(),
            )
            .when_some(row.summary.clone(), |this, summary| {
                this.child(
                    Label::new(summary)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
            })
            .into_any_element()
    }
}

impl Render for AgentBoard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = self
            .rows
            .clone()
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_row(index, row, cx))
            .collect();
        v_flex()
            .id("agent-board")
            .key_context("AgentBoard")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .h(Tab::container_height(cx))
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(tr("Agents", "Agent 看板")).size(LabelSize::Small)),
            )
            .child(if rows.is_empty() {
                v_flex()
                    .flex_1()
                    .p_4()
                    .items_center()
                    .justify_center()
                    .child(
                        Label::new(tr(
                            "No AI agents are running in terminals",
                            "没有在终端里运行的 AI agent",
                        ))
                        .color(Color::Muted),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .id("agent-board-rows")
                    .flex_1()
                    .p_1()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element()
            })
    }
}

impl Focusable for AgentBoard {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for AgentBoard {}

impl Panel for AgentBoard {
    fn persistent_name() -> &'static str {
        "Agent Board"
    }

    fn panel_key() -> &'static str {
        AGENT_BOARD_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
        px(300.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::ListTodo)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some(tr("Agent Board", "Agent 看板"))
    }

    fn icon_label(&self, _window: &Window, _cx: &App) -> Option<String> {
        let count = self.needs_input_count();
        (count > 0).then(|| count.to_string())
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            // The dock activates panels while the workspace is being updated.
            let board = cx.weak_entity();
            cx.defer(move |cx| {
                board
                    .update(cx, |board, cx| board.terminals_changed(cx))
                    .ok();
            });
        } else {
            self.update_tick(cx);
        }
    }

    fn activation_priority(&self) -> u32 {
        12
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(id: u64, status: AgentStatus) -> TerminalSnapshot {
        TerminalSnapshot {
            terminal: EntityId::from(id),
            title: format!("terminal {id}"),
            status,
            branch: Some("main".to_string()),
            changed_files: None,
            elapsed: None,
            last_message: None,
        }
    }

    fn task(name: &str, terminals: &[u64]) -> TaskSnapshot {
        TaskSnapshot {
            name: name.to_string(),
            branch: format!("solo/{name}"),
            worktree_path: Some(PathBuf::from(format!("/work/{name}"))),
            terminals: terminals.iter().map(|id| EntityId::from(*id)).collect(),
        }
    }

    #[test]
    fn test_rows_sorted_by_status() {
        let rows = board_rows(
            vec![
                terminal(1, AgentStatus::Done),
                terminal(2, AgentStatus::Idle),
                terminal(3, AgentStatus::Working),
                terminal(4, AgentStatus::NeedsInput),
                terminal(5, AgentStatus::Failed),
                terminal(6, AgentStatus::Working),
            ],
            &[task("unstarted", &[])],
            false,
        );
        let order: Vec<_> = rows
            .iter()
            .map(|row| (row.title.as_str(), row.status))
            .collect();
        assert_eq!(
            order,
            [
                ("terminal 4", Some(AgentStatus::NeedsInput)),
                ("terminal 3", Some(AgentStatus::Working)),
                ("terminal 6", Some(AgentStatus::Working)),
                ("terminal 2", Some(AgentStatus::Idle)),
                ("terminal 1", Some(AgentStatus::Done)),
                ("terminal 5", Some(AgentStatus::Failed)),
                ("unstarted", None),
            ]
        );
    }

    #[test]
    fn test_rows_from_tasks() {
        let mut in_task = terminal(1, AgentStatus::Working);
        in_task.changed_files = Some(3);
        in_task.elapsed = Some(Duration::from_secs(125));
        in_task.last_message = Some("Done.\n\n  All   tests pass.".to_string());
        let rows = board_rows(
            vec![in_task, terminal(2, AgentStatus::Idle)],
            &[
                task("fix-login", &[1]),
                // Its terminal runs no agent: the row shows the terminal.
                task("docs", &[7]),
                task("gone", &[]),
            ],
            false,
        );
        assert_eq!(
            rows[0],
            BoardRow {
                terminal: Some(EntityId::from(1)),
                title: "terminal 1".to_string(),
                status: Some(AgentStatus::Working),
                task: Some("fix-login".to_string()),
                branch: Some("solo/fix-login".to_string()),
                changed_files: Some(3),
                elapsed: Some("2m".to_string()),
                summary: Some("Done. All tests pass.".to_string()),
                worktree_path: Some(PathBuf::from("/work/fix-login")),
            }
        );
        assert_eq!(rows[1].task, None);
        assert_eq!(rows[1].branch.as_deref(), Some("main"));
        assert_eq!(rows[2].title, "docs");
        assert_eq!(rows[2].terminal, Some(EntityId::from(7)));
        assert_eq!(rows[2].status, None);
        assert_eq!(rows[3].title, "gone");
        assert_eq!(rows[3].terminal, None);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn test_format_elapsed() {
        assert_eq!(format_elapsed(Duration::from_secs(5), false), "5s");
        assert_eq!(format_elapsed(Duration::from_secs(59), false), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(60), false), "1m");
        assert_eq!(format_elapsed(Duration::from_secs(3599), false), "59m");
        assert_eq!(format_elapsed(Duration::from_secs(3900), false), "1h 5m");
        assert_eq!(format_elapsed(Duration::from_secs(45), true), "45 秒");
        assert_eq!(format_elapsed(Duration::from_secs(180), true), "3 分钟");
        assert_eq!(
            format_elapsed(Duration::from_secs(3900), true),
            "1 小时 5 分"
        );
    }

    #[test]
    fn test_summarize() {
        assert_eq!(summarize(""), None);
        assert_eq!(summarize(" \n\t "), None);
        assert_eq!(
            summarize("Fixed it.\n\n- one\n- two").as_deref(),
            Some("Fixed it. - one - two")
        );
        let long = "改".repeat(MAX_SUMMARY_CHARS + 10);
        let summary = summarize(&long).unwrap_or_default();
        assert_eq!(summary.chars().count(), MAX_SUMMARY_CHARS + 1);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn test_changed_files_label() {
        assert_eq!(changed_files_label(1, false), "1 file changed");
        assert_eq!(changed_files_label(4, false), "4 files changed");
        assert_eq!(changed_files_label(4, true), "改动 4 个文件");
    }
}
