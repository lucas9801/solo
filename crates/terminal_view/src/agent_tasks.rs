//! SOLO: agent tasks. "New Agent Task" makes a git worktree on a new branch
//! for a task and starts an AI agent (`terminal.agent_task_command`) in a
//! terminal there, named after the task; "Manage Agent Tasks" lists the tasks
//! to review their changes, merge them back or delete them.
//!
//! Tasks live in git (see [`git`]), so they're found again after a restart.
//! [`agent_tasks`] lists them with their terminals.

pub mod git;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use futures::{FutureExt as _, channel::oneshot};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Render, SharedString, Task, WeakEntity, Window,
};
use project::project_settings::ProjectSettings;
use settings::Settings as _;
use terminal::{Event as TerminalEvent, Terminal, terminal_settings::TerminalSettings};
use ui::{ButtonStyle, TintColor, prelude::*};
use util::{
    ResultExt as _,
    i18n::{is_chinese, tr},
};
use workspace::{
    ModalView, OpenOptions, OpenVisible, Pane, SaveIntent, Toast, Workspace,
    notifications::NotificationId,
};
use zed_actions::agent_task::{ManageAgentTasks, NewAgentTask};

pub use git::{AgentTask, MergeOutcome};

use crate::{
    TerminalView,
    command_blocks::{activate_terminal_view, terminal_views},
    follow_working_directory,
    terminal_panel::TerminalPanel,
};

/// How long to wait for the new terminal's shell to show its prompt before
/// typing the agent command anyway (shells without integration never say).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LISTED_CHANGES: usize = 100;
const MAX_LISTED_COMMITS: usize = 20;

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &NewAgentTask, window, cx| {
            NewAgentTaskModal::toggle(workspace, window, cx);
        });
        workspace.register_action(|workspace, _: &ManageAgentTasks, window, cx| {
            AgentTasksModal::toggle(workspace, window, cx);
        });
    })
    .detach();
}

/// An agent task with the terminals open in its worktree.
#[derive(Clone)]
pub struct AgentTaskEntry {
    pub task: AgentTask,
    pub terminals: Vec<Entity<TerminalView>>,
}

/// Whether `path` is `root` or inside it. Case-insensitive on Windows, where
/// git and the shell may spell the same directory differently.
pub(crate) fn is_within(path: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let normalize =
            |path: &Path| PathBuf::from(path.to_string_lossy().replace('/', "\\").to_lowercase());
        normalize(path).starts_with(normalize(root))
    } else {
        path.starts_with(root)
    }
}

/// The directory the terminal's shell is in.
fn terminal_directory(terminal: &Terminal) -> Option<PathBuf> {
    terminal
        .shell_integration()
        .working_directory()
        .cloned()
        .or_else(|| terminal.working_directory())
}

/// Where a new task starts from: the active terminal's directory, else the
/// project's.
fn start_directory(workspace: &Workspace, cx: &App) -> Option<PathBuf> {
    workspace
        .active_item_as::<TerminalView>(cx)
        .and_then(|view| terminal_directory(view.read(cx).terminal.read(cx)))
        .or_else(|| {
            workspace
                .project()
                .read(cx)
                .active_repository(cx)
                .map(|repository| repository.read(cx).work_directory_abs_path.to_path_buf())
        })
        .or_else(|| {
            workspace
                .project()
                .read(cx)
                .visible_worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        })
}

/// Lists the agent tasks of the repositories the workspace has open, or has
/// terminals in, with the terminals open in each task's worktree.
pub fn agent_tasks(
    workspace: &Entity<Workspace>,
    cx: &mut App,
) -> Task<Result<Vec<AgentTaskEntry>>> {
    let views = terminal_views(workspace, cx);
    let mut directories: Vec<PathBuf> = workspace
        .read(cx)
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect();
    let view_directories: Vec<(Entity<TerminalView>, PathBuf)> = views
        .into_iter()
        .filter_map(|view| {
            let directory = terminal_directory(view.read(cx).terminal.read(cx))?;
            Some((view, directory))
        })
        .collect();
    directories.extend(
        view_directories
            .iter()
            .map(|(_, directory)| directory.clone()),
    );
    if !workspace.read(cx).project().read(cx).is_local() {
        return Task::ready(Ok(Vec::new()));
    }

    let tasks = cx.background_spawn(async move {
        let mut roots: Vec<PathBuf> = Vec::new();
        for directory in directories {
            let Ok(info) = git::repository_info(&directory) else {
                continue;
            };
            if !roots
                .iter()
                .any(|root| is_within(root, &info.main_root) && is_within(&info.main_root, root))
            {
                roots.push(info.main_root);
            }
        }
        let mut tasks = Vec::new();
        for root in roots {
            match git::list_tasks(&root) {
                Ok(found) => tasks.extend(found),
                Err(error) => log::warn!("failed to list the agent tasks in {root:?}: {error:#}"),
            }
        }
        tasks
    });
    cx.spawn(async move |_| {
        let tasks = tasks.await;
        Ok(tasks
            .into_iter()
            .map(|task| {
                let terminals = task
                    .worktree_path
                    .as_deref()
                    .map(|worktree| {
                        view_directories
                            .iter()
                            .filter(|(_, directory)| is_within(directory, worktree))
                            .map(|(view, _)| view.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                AgentTaskEntry { task, terminals }
            })
            .collect())
    })
}

/// Opens a terminal in `directory` named `title`, and types `command` in it
/// once its shell is ready.
fn open_task_terminal(
    workspace: &mut Workspace,
    directory: PathBuf,
    title: String,
    command: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<()>> {
    let terminal = TerminalPanel::add_center_terminal(workspace, window, cx, move |project, cx| {
        project.create_terminal_shell(Some(directory), cx)
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let terminal = terminal.await?.upgrade().context("the terminal closed")?;
        let workspace = workspace.upgrade().context("the workspace closed")?;
        cx.update(|_, cx| {
            let view = terminal_views(&workspace, cx)
                .into_iter()
                .find(|view| view.read(cx).terminal == terminal);
            if let Some(view) = view {
                view.update(cx, |view, cx| view.set_custom_title(Some(title), cx));
            }
            if let Some(command) = command.filter(|command| !command.trim().is_empty()) {
                type_when_ready(terminal, command, cx);
            }
        })
    })
}

/// Types `command` into the terminal once its shell shows its first prompt,
/// so the shell (and its integration, which wraps `claude`) is set up.
fn type_when_ready(terminal: Entity<Terminal>, command: String, cx: &mut App) {
    let (prompt_shown, prompt_receiver) = oneshot::channel::<()>();
    let prompt_shown = Rc::new(RefCell::new(Some(prompt_shown)));
    let subscription = cx.subscribe(&terminal, move |_, event: &TerminalEvent, _| {
        if *event == TerminalEvent::PromptStarted
            && let Some(prompt_shown) = prompt_shown.borrow_mut().take()
        {
            prompt_shown.send(()).ok();
        }
    });
    cx.spawn(async move |cx| {
        let timeout = cx.background_executor().timer(PROMPT_TIMEOUT);
        futures::select_biased! {
            _ = prompt_receiver.fuse() => {}
            _ = timeout.fuse() => {}
        }
        drop(subscription);
        terminal.update(cx, |terminal, _| {
            terminal.input(format!("{command}\r").into_bytes());
        });
    })
    .detach();
}

fn show_toast(workspace: &Entity<Workspace>, message: String, cx: &mut App) {
    struct AgentTaskToast;
    workspace.update(cx, |workspace, cx| {
        workspace.show_toast(
            Toast::new(NotificationId::unique::<AgentTaskToast>(), message).autohide(),
            cx,
        );
    });
}

/// Writes the task's changes to a `.diff` file and opens it.
fn open_task_diff(
    workspace: Entity<Workspace>,
    task: AgentTask,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    let file_name = format!("{}.diff", task.branch.replace(['/', '\\'], "-"));
    let path = paths::temp_dir().join("agent-task-diffs").join(file_name);
    window.spawn(cx, async move |cx| {
        let diff = cx
            .background_spawn({
                let path = path.clone();
                async move {
                    let diff = git::diff_against_base(&task)?;
                    let diff = if diff.trim().is_empty() {
                        if is_chinese() {
                            format!("# {} 相对 {} 没有改动\n", task.branch, task.base_branch)
                        } else {
                            format!(
                                "# No changes in {} since {}\n",
                                task.branch, task.base_branch
                            )
                        }
                    } else {
                        diff
                    };
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, diff)?;
                    anyhow::Ok(())
                }
            })
            .await;
        diff?;
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    path,
                    OpenOptions {
                        visible: Some(OpenVisible::None),
                        ..OpenOptions::default()
                    },
                    window,
                    cx,
                )
            })?
            .await?;
        Ok(())
    })
}

fn all_panes(workspace: &Entity<Workspace>, cx: &App) -> Vec<Entity<Pane>> {
    let workspace = workspace.read(cx);
    let mut panes: Vec<Entity<Pane>> = workspace
        .panel::<TerminalPanel>(cx)
        .map(|panel| panel.read(cx).center.panes().into_iter().cloned().collect())
        .unwrap_or_default();
    panes.extend(workspace.panes().iter().cloned());
    panes
}

/// Closes the terminals without asking: the user confirmed deleting the
/// task they're in.
fn close_terminals(
    workspace: &Entity<Workspace>,
    terminals: &[Entity<TerminalView>],
    window: &mut Window,
    cx: &mut App,
) -> Vec<Task<Result<()>>> {
    let mut closing = Vec::new();
    for pane in all_panes(workspace, cx) {
        for terminal in terminals {
            let item_id = terminal.entity_id();
            if pane.read(cx).items().any(|item| item.item_id() == item_id) {
                closing.push(pane.update(cx, |pane, cx| {
                    pane.close_item_by_id(item_id, SaveIntent::Skip, window, cx)
                }));
            }
        }
    }
    closing
}

enum RepositoryState {
    Loading,
    Ready(git::RepositoryInfo),
    Failed(SharedString),
}

/// Asks for the new task's name.
pub struct NewAgentTaskModal {
    workspace: WeakEntity<Workspace>,
    editor: Entity<Editor>,
    repository: RepositoryState,
    worktree_directory_setting: String,
    creating: bool,
    error: Option<SharedString>,
    _load: Task<()>,
}

impl NewAgentTaskModal {
    fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        if !workspace.project().read(cx).is_local() {
            show_toast(
                &cx.entity(),
                tr(
                    "Agent tasks only work in local projects",
                    "agent 任务只支持本地项目",
                )
                .to_string(),
                cx,
            );
            return;
        }
        let directory = start_directory(workspace, cx);
        let weak_workspace = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| {
            Self::new(weak_workspace, directory, window, cx)
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        directory: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(
                tr(
                    "Task name, e.g. Fix the login redirect",
                    "任务名，例如：修复登录跳转",
                ),
                window,
                cx,
            );
            editor
        });
        cx.subscribe(&editor, |this, _, event: &editor::EditorEvent, cx| {
            if let editor::EditorEvent::BufferEdited = event {
                this.error = None;
                cx.notify();
            }
        })
        .detach();
        let load = cx.spawn(async move |this, cx| {
            let info = match directory {
                Some(directory) => {
                    cx.background_spawn(async move { git::repository_info(&directory) })
                        .await
                }
                None => Err(anyhow!("no directory")),
            };
            this.update(cx, |this, cx| {
                this.repository = match info {
                    Ok(info) => RepositoryState::Ready(info),
                    Err(error) => {
                        log::info!("no repository for a new agent task: {error:#}");
                        RepositoryState::Failed(
                            tr(
                                "The active terminal or project isn't in a git repository",
                                "当前终端或项目不在 git 仓库里",
                            )
                            .into(),
                        )
                    }
                };
                cx.notify();
            })
            .log_err();
        });
        Self {
            workspace,
            editor,
            repository: RepositoryState::Loading,
            worktree_directory_setting: ProjectSettings::get_global(cx)
                .git
                .worktree_directory
                .clone(),
            creating: false,
            error: None,
            _load: load,
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let RepositoryState::Ready(info) = &self.repository else {
            return;
        };
        let name = self.editor.read(cx).text(cx).trim().to_string();
        if name.is_empty() {
            self.error = Some(tr("Give the task a name", "请输入任务名").into());
            cx.notify();
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let info = info.clone();
        let setting = self.worktree_directory_setting.clone();
        let command = TerminalSettings::get_global(cx).agent_task_command.clone();
        self.creating = true;
        cx.notify();

        // Detached: dismissing the modal drops it, and the terminal must open
        // once the modal is gone so it gets the focus.
        cx.spawn_in(window, async move |this, cx| {
            let created = cx
                .background_spawn({
                    let name = name.clone();
                    async move {
                        let plan = git::plan_task(&info, &name, &setting)?;
                        git::create_task(&info, &plan)
                    }
                })
                .await;
            let task = match created {
                Ok(task) => task,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.creating = false;
                        this.error = Some(format!("{error:#}").into());
                        cx.notify();
                    })
                    .log_err();
                    return;
                }
            };
            this.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
            let Some(directory) = task.worktree_path.clone() else {
                return;
            };
            let opened = workspace.update_in(cx, |workspace, window, cx| {
                open_task_terminal(
                    workspace,
                    directory,
                    task.name.clone(),
                    Some(command),
                    window,
                    cx,
                )
            });
            let opened = match opened {
                Ok(opened) => opened.await,
                Err(error) => Err(error),
            };
            if let Err(error) = opened {
                cx.update(|_, cx| {
                    let message = if is_chinese() {
                        format!("已创建任务 {}，但没能打开终端：{error:#}", task.branch)
                    } else {
                        format!(
                            "Created the task {}, but couldn't open its terminal: {error:#}",
                            task.branch
                        )
                    };
                    show_toast(&workspace, message, cx);
                })
                .log_err();
            }
        })
        .detach();
    }

    /// The branch and directory the task would get, before making them
    /// unique.
    fn preview(&self, cx: &App) -> Option<(String, PathBuf)> {
        let RepositoryState::Ready(info) = &self.repository else {
            return None;
        };
        let slug = git::task_slug(self.editor.read(cx).text(cx).trim());
        let directory =
            git::worktrees_directory(&info.main_root, &self.worktree_directory_setting).ok()?;
        Some((git::branch_for_slug(&slug), directory.join(slug)))
    }
}

impl EventEmitter<DismissEvent> for NewAgentTaskModal {}

impl Focusable for NewAgentTaskModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl ModalView for NewAgentTaskModal {}

impl Render for NewAgentTaskModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status: Option<(SharedString, Color)> = match &self.repository {
            RepositoryState::Loading => None,
            RepositoryState::Failed(message) => Some((message.clone(), Color::Error)),
            RepositoryState::Ready(info) => match &info.current_branch {
                None => Some((
                    tr(
                        "HEAD is detached; check out the branch the task should start from",
                        "HEAD 处于分离状态，请先切换到任务要基于的分支",
                    )
                    .into(),
                    Color::Error,
                )),
                Some(branch) => self.preview(cx).map(|(new_branch, directory)| {
                    let text = if is_chinese() {
                        format!(
                            "从 {branch} 新建分支 {new_branch}，工作区 {}",
                            directory.display()
                        )
                    } else {
                        format!(
                            "New branch {new_branch} from {branch}, worktree {}",
                            directory.display()
                        )
                    };
                    (text.into(), Color::Muted)
                }),
            },
        };
        let command = TerminalSettings::get_global(cx).agent_task_command.clone();
        let runs = if command.trim().is_empty() {
            None
        } else if is_chinese() {
            Some(format!(
                "然后在其中的终端运行 {command}（设置项 terminal.agent_task_command）"
            ))
        } else {
            Some(format!(
                "Then runs {command} in a terminal there (terminal.agent_task_command)"
            ))
        };

        v_flex()
            .w(rems(34.))
            .elevation_2(cx)
            .key_context("NewAgentTask")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .child(
                h_flex().px_2().pt_2().child(
                    Label::new(tr("New Agent Task", "新建 agent 任务"))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .px_2()
                    .py_1()
                    .child(self.editor.clone()),
            )
            .child(
                v_flex()
                    .px_2()
                    .py_1()
                    .gap_0p5()
                    .when_some(status, |this, (text, color)| {
                        this.child(Label::new(text).size(LabelSize::Small).color(color))
                    })
                    .when_some(runs, |this, runs| {
                        this.child(Label::new(runs).size(LabelSize::Small).color(Color::Muted))
                    })
                    .when(self.creating, |this| {
                        this.child(
                            Label::new(tr("Creating the worktree…", "正在创建工作区…"))
                                .size(LabelSize::Small)
                                .color(Color::Accent),
                        )
                    })
                    .when_some(self.error.clone(), |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    }),
            )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FinishAction {
    Merge,
    Delete,
}

/// What the confirmation page shows before merging or deleting.
#[derive(Clone, Default)]
struct TaskDetails {
    uncommitted: git::UncommittedChanges,
    unmerged_commits: Vec<String>,
    base_worktree: Option<PathBuf>,
    base_uncommitted: git::UncommittedChanges,
}

enum Page {
    List,
    Confirm {
        action: FinishAction,
        index: usize,
        details: Option<Result<TaskDetails, SharedString>>,
    },
    Working(SharedString),
    Outcome {
        message: SharedString,
        is_error: bool,
        /// Offer to delete the task with this branch, e.g. once it's merged.
        /// By branch, since the list is reloaded meanwhile.
        offer_delete: Option<String>,
    },
}

/// Lists the agent tasks, to open their terminal, review, merge or delete
/// them.
pub struct AgentTasksModal {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    entries: Option<Result<Vec<AgentTaskEntry>, SharedString>>,
    selected: usize,
    /// The directory of the terminal that was active, to select its task.
    active_directory: Option<PathBuf>,
    page: Page,
    _task: Task<()>,
}

impl AgentTasksModal {
    fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let weak_workspace = workspace.weak_handle();
        let active_directory = workspace
            .active_item_as::<TerminalView>(cx)
            .and_then(|view| terminal_directory(view.read(cx).terminal.read(cx)));
        workspace.toggle_modal(window, cx, move |window, cx| {
            let mut modal = Self {
                workspace: weak_workspace,
                focus_handle: cx.focus_handle(),
                entries: None,
                selected: 0,
                active_directory,
                page: Page::List,
                _task: Task::ready(()),
            };
            modal.reload(window, cx);
            modal
        });
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let load = agent_tasks(&workspace, cx);
        self._task = cx.spawn_in(window, async move |this, cx| {
            let entries = load.await;
            this.update(cx, |this, cx| {
                let entries = entries.map_err(|error| SharedString::from(format!("{error:#}")));
                if let (Ok(entries), Some(directory)) = (&entries, &this.active_directory)
                    && let Some(index) = entries.iter().position(|entry| {
                        entry
                            .task
                            .worktree_path
                            .as_deref()
                            .is_some_and(|worktree| is_within(directory, worktree))
                    })
                {
                    this.selected = index;
                }
                if let Ok(entries) = &entries {
                    this.selected = this.selected.min(entries.len().saturating_sub(1));
                }
                this.entries = Some(entries);
                cx.notify();
            })
            .log_err();
        });
    }

    fn entry(&self, index: usize) -> Option<&AgentTaskEntry> {
        self.entries.as_ref()?.as_ref().ok()?.get(index)
    }

    fn index_of(&self, branch: &str) -> Option<usize> {
        self.entries
            .as_ref()?
            .as_ref()
            .ok()?
            .iter()
            .position(|entry| entry.task.branch == branch)
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        match self.page {
            Page::List => cx.emit(DismissEvent),
            Page::Working(_) => {}
            Page::Confirm { .. } | Page::Outcome { .. } => {
                self.page = Page::List;
                cx.notify();
            }
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.page, Page::List) {
            self.open_terminal(self.selected, window, cx);
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(Ok(entries)) = &self.entries
            && !entries.is_empty()
        {
            self.selected = (self.selected + 1) % entries.len();
            cx.notify();
        }
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Ok(entries)) = &self.entries
            && !entries.is_empty()
        {
            self.selected = (self.selected + entries.len() - 1) % entries.len();
            cx.notify();
        }
    }

    /// Shows the task's terminal, opening a new one in its worktree if it has
    /// none.
    fn open_terminal(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        cx.emit(DismissEvent);
        // Once the modal is gone, so the terminal gets the focus.
        window.defer(cx, move |window, cx| {
            if let Some(view) = entry.terminals.first() {
                activate_terminal_view(&workspace, view, window, cx);
                return;
            }
            let Some(directory) = entry.task.worktree_path.clone() else {
                return;
            };
            workspace.update(cx, |workspace, cx| {
                open_task_terminal(workspace, directory, entry.task.name, None, window, cx)
                    .detach_and_log_err(cx);
            });
        });
    }

    fn show_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        cx.emit(DismissEvent);
        let opened = open_task_diff(workspace.clone(), entry.task, window, cx);
        cx.spawn(async move |_, cx| {
            if let Err(error) = opened.await {
                cx.update(|cx| {
                    let message = if is_chinese() {
                        format!("没能生成 diff：{error:#}")
                    } else {
                        format!("Couldn't make the diff: {error:#}")
                    };
                    show_toast(&workspace, message, cx);
                });
            }
        })
        .detach();
    }

    fn ask(
        &mut self,
        action: FinishAction,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        self.page = Page::Confirm {
            action,
            index,
            details: None,
        };
        cx.notify();
        let task = entry.task;
        self._task = cx.spawn_in(window, async move |this, cx| {
            let details = cx
                .background_spawn(async move {
                    let uncommitted = match &task.worktree_path {
                        Some(worktree) if worktree.exists() => git::uncommitted_changes(worktree)?,
                        _ => Vec::new(),
                    };
                    let unmerged_commits = git::unmerged_commits(&task)?;
                    let base_worktree = git::base_worktree(&task)?;
                    let base_uncommitted = match (&base_worktree, action) {
                        (Some(base), FinishAction::Merge) => git::uncommitted_changes(base)?,
                        _ => Vec::new(),
                    };
                    anyhow::Ok(TaskDetails {
                        uncommitted,
                        unmerged_commits,
                        base_worktree,
                        base_uncommitted,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if let Page::Confirm {
                    index: page_index,
                    details: page_details,
                    ..
                } = &mut this.page
                    && *page_index == index
                {
                    *page_details =
                        Some(details.map_err(|error| SharedString::from(format!("{error:#}"))));
                    cx.notify();
                }
            })
            .log_err();
        });
    }

    fn run(
        &mut self,
        action: FinishAction,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        match action {
            FinishAction::Merge => self.merge(entry, window, cx),
            FinishAction::Delete => self.delete(workspace, entry, window, cx),
        }
    }

    fn merge(&mut self, entry: AgentTaskEntry, window: &mut Window, cx: &mut Context<Self>) {
        let task = entry.task;
        self.page = Page::Working(tr("Merging…", "正在合并…").into());
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_spawn({
                    let task = task.clone();
                    async move { git::merge_task(&task) }
                })
                .await;
            let chinese = is_chinese();
            let (message, is_error, offer_delete) = match outcome {
                Ok(MergeOutcome::Merged) => (
                    if chinese {
                        format!("已把 {} 合并到 {}。", task.branch, task.base_branch)
                    } else {
                        format!("Merged {} into {}.", task.branch, task.base_branch)
                    },
                    false,
                    Some(task.branch.clone()),
                ),
                Ok(MergeOutcome::NothingToMerge) => (
                    if chinese {
                        format!(
                            "{} 没有 {} 之外的提交，没有可合并的内容。未提交的改动需要先提交。",
                            task.branch, task.base_branch
                        )
                    } else {
                        format!(
                            "{} has no commits that {} doesn't have. Commit the task's changes first.",
                            task.branch, task.base_branch
                        )
                    },
                    false,
                    None,
                ),
                Ok(MergeOutcome::Conflicts(files)) => (
                    if chinese {
                        format!(
                            "合并有冲突，已中止，{} 保持原样。冲突文件：\n{}\n\n可以在任务的工作区里把 {} 合并进来并解决冲突（例如交给 agent），然后再合并。",
                            task.base_branch,
                            files.join("\n"),
                            task.base_branch
                        )
                    } else {
                        format!(
                            "The merge has conflicts and was aborted; {} is unchanged. Conflicting files:\n{}\n\nMerge {} into the task in its worktree and resolve them there (or have the agent do it), then merge again.",
                            task.base_branch,
                            files.join("\n"),
                            task.base_branch
                        )
                    },
                    true,
                    None,
                ),
                Err(error) => (format!("{error:#}"), true, None),
            };
            this.update_in(cx, |this, window, cx| {
                this.page = Page::Outcome {
                    message: message.into(),
                    is_error,
                    offer_delete,
                };
                this.reload(window, cx);
            })
            .log_err();
        });
    }

    fn delete(
        &mut self,
        workspace: Entity<Workspace>,
        entry: AgentTaskEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page = Page::Working(tr("Deleting…", "正在删除…").into());
        cx.notify();
        let closing = close_terminals(&workspace, &entry.terminals, window, cx);
        if let Some(worktree) = &entry.task.worktree_path {
            follow_working_directory::release_directory(&workspace, worktree, cx);
        }
        let task = entry.task;
        self._task = cx.spawn_in(window, async move |this, cx| {
            for closed in closing {
                closed.await.log_err();
            }
            let deleted = cx
                .background_spawn({
                    let task = task.clone();
                    async move { git::delete_task(&task) }
                })
                .await;
            let (message, is_error) = match deleted {
                Ok(()) => (
                    if is_chinese() {
                        format!("已删除任务 {} 的工作区和分支。", task.name)
                    } else {
                        format!("Deleted the worktree and branch of {}.", task.name)
                    },
                    false,
                ),
                Err(error) => (format!("{error:#}"), true),
            };
            this.update_in(cx, |this, window, cx| {
                this.page = Page::Outcome {
                    message: message.into(),
                    is_error,
                    offer_delete: None,
                };
                this.reload(window, cx);
            })
            .log_err();
        });
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let entries = match &self.entries {
            None => {
                return Label::new(tr("Looking for agent tasks…", "正在查找 agent 任务…"))
                    .color(Color::Muted)
                    .into_any_element();
            }
            Some(Err(error)) => {
                return Label::new(error.clone())
                    .color(Color::Error)
                    .into_any_element();
            }
            Some(Ok(entries)) => entries,
        };
        if entries.is_empty() {
            return v_flex()
                .gap_1()
                .child(Label::new(tr("No agent tasks yet.", "还没有 agent 任务。")))
                .child(
                    Button::new("new-agent-task", tr("New Agent Task", "新建 agent 任务"))
                        .style(ButtonStyle::Filled)
                        .on_click(cx.listener(|_, _, window, cx| {
                            cx.emit(DismissEvent);
                            window.dispatch_action(Box::new(NewAgentTask), cx);
                        })),
                )
                .into_any_element();
        }

        v_flex()
            .id("agent-tasks")
            .max_h(rems(28.))
            .overflow_y_scroll()
            .gap_1()
            .children(entries.iter().enumerate().map(|(index, entry)| {
                let task = &entry.task;
                let status = entry
                    .terminals
                    .iter()
                    .find_map(|view| view.read(cx).agent_status().map(|(_, status)| status));
                let has_worktree = task.worktree_path.is_some();
                let location = match &task.worktree_path {
                    Some(path) => path.display().to_string(),
                    None => tr("worktree is gone", "工作区已不存在").to_string(),
                };
                let selected = index == self.selected;
                v_flex()
                    .id(("agent-task", index))
                    .p_1()
                    .rounded_sm()
                    .when(selected, |this| {
                        this.bg(cx.theme().colors().element_selected)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(status, |this, status| {
                                let (icon, color) = status.icon();
                                this.child(Icon::new(icon).size(IconSize::Small).color(color))
                            })
                            .child(Label::new(task.name.clone()))
                            .child(
                                Label::new(format!("{} → {}", task.branch, task.base_branch))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        Label::new(location)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .pt_0p5()
                            .child(
                                Button::new(("terminal", index), tr("Terminal", "终端"))
                                    .style(ButtonStyle::Subtle)
                                    .label_size(LabelSize::Small)
                                    .disabled(!has_worktree)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_terminal(index, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("diff", index), tr("View Diff", "查看 diff"))
                                    .style(ButtonStyle::Subtle)
                                    .label_size(LabelSize::Small)
                                    .disabled(!has_worktree)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.show_diff(index, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(
                                    ("merge", index),
                                    if is_chinese() {
                                        format!("合并到 {}", task.base_branch)
                                    } else {
                                        format!("Merge into {}", task.base_branch)
                                    },
                                )
                                .style(ButtonStyle::Subtle)
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.ask(FinishAction::Merge, index, window, cx)
                                    },
                                )),
                            )
                            .child(
                                Button::new(("delete", index), tr("Delete", "删除"))
                                    .style(ButtonStyle::Subtle)
                                    .label_size(LabelSize::Small)
                                    .color(Color::Error)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask(FinishAction::Delete, index, window, cx)
                                    })),
                            ),
                    )
            }))
            .into_any_element()
    }

    fn render_confirm(
        &self,
        action: FinishAction,
        index: usize,
        details: Option<&Result<TaskDetails, SharedString>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(entry) = self.entry(index) else {
            return div().into_any_element();
        };
        let task = &entry.task;
        let chinese = is_chinese();
        let title = match (action, chinese) {
            (FinishAction::Merge, true) => {
                format!("把 {} 合并到 {}？", task.branch, task.base_branch)
            }
            (FinishAction::Merge, false) => {
                format!("Merge {} into {}?", task.branch, task.base_branch)
            }
            (FinishAction::Delete, true) => {
                format!("删除任务“{}”的工作区和分支 {}？", task.name, task.branch)
            }
            (FinishAction::Delete, false) => format!(
                "Delete the worktree and branch {} of \"{}\"?",
                task.branch, task.name
            ),
        };

        let mut body = v_flex().gap_1();
        let mut can_confirm = false;
        match details {
            None => {
                body = body.child(
                    Label::new(tr("Checking the task…", "正在检查任务…")).color(Color::Muted),
                );
            }
            Some(Err(error)) => {
                body = body.child(Label::new(error.clone()).color(Color::Error));
            }
            Some(Ok(details)) => {
                can_confirm = true;
                match action {
                    FinishAction::Merge => {
                        match &details.base_worktree {
                            Some(base) => {
                                body = body.child(
                                    Label::new(if chinese {
                                        format!("在 {} 中执行 git merge --no-ff", base.display())
                                    } else {
                                        format!("Runs git merge --no-ff in {}", base.display())
                                    })
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                                );
                            }
                            None => {
                                can_confirm = false;
                                body = body.child(
                                    Label::new(if chinese {
                                        format!(
                                            "{} 没有在任何工作区检出，请先检出它再合并。",
                                            task.base_branch
                                        )
                                    } else {
                                        format!(
                                            "{} isn't checked out in any worktree; check it out to merge into it.",
                                            task.base_branch
                                        )
                                    })
                                    .color(Color::Error),
                                );
                            }
                        }
                        body = body.child(lines_section(
                            if chinese {
                                format!("要合并的提交（{}）", details.unmerged_commits.len())
                            } else {
                                format!("Commits to merge ({})", details.unmerged_commits.len())
                            },
                            &details.unmerged_commits,
                            MAX_LISTED_COMMITS,
                            Color::Default,
                        ));
                        if details.unmerged_commits.is_empty() {
                            can_confirm = false;
                        }
                        if !details.uncommitted.is_empty() {
                            body = body.child(lines_section(
                                tr(
                                    "Uncommitted changes in the task, which won't be merged:",
                                    "任务里未提交的改动（不会被合并）：",
                                )
                                .to_string(),
                                &details.uncommitted,
                                MAX_LISTED_CHANGES,
                                Color::Warning,
                            ));
                        }
                        if !details.base_uncommitted.is_empty() {
                            body = body.child(lines_section(
                                if chinese {
                                    format!(
                                        "{} 有未提交的改动，可能会让合并失败：",
                                        task.base_branch
                                    )
                                } else {
                                    format!(
                                        "{} has uncommitted changes, which may stop the merge:",
                                        task.base_branch
                                    )
                                },
                                &details.base_uncommitted,
                                MAX_LISTED_CHANGES,
                                Color::Warning,
                            ));
                        }
                    }
                    FinishAction::Delete => {
                        if details.uncommitted.is_empty() {
                            body = body.child(
                                Label::new(tr("No uncommitted changes.", "没有未提交的改动。"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            );
                        } else {
                            body = body.child(lines_section(
                                if chinese {
                                    format!(
                                        "这些未提交的改动会丢失（{}）：",
                                        details.uncommitted.len()
                                    )
                                } else {
                                    format!(
                                        "These uncommitted changes will be lost ({}):",
                                        details.uncommitted.len()
                                    )
                                },
                                &details.uncommitted,
                                MAX_LISTED_CHANGES,
                                Color::Error,
                            ));
                        }
                        if !details.unmerged_commits.is_empty() {
                            body = body.child(lines_section(
                                if chinese {
                                    format!(
                                        "这些提交还没合并到 {}，会随分支一起删除：",
                                        task.base_branch
                                    )
                                } else {
                                    format!(
                                        "These commits aren't in {} and go with the branch:",
                                        task.base_branch
                                    )
                                },
                                &details.unmerged_commits,
                                MAX_LISTED_COMMITS,
                                Color::Warning,
                            ));
                        }
                        if !entry.terminals.is_empty() {
                            let running: Vec<String> = entry
                                .terminals
                                .iter()
                                .map(|view| {
                                    let view = view.read(cx);
                                    let title = view
                                        .custom_title()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| view.terminal.read(cx).title(true));
                                    match view.agent_status() {
                                        Some((agent, _)) => format!("{title} ({agent})"),
                                        None => title,
                                    }
                                })
                                .collect();
                            body = body.child(lines_section(
                                tr("These terminals will be closed:", "这些终端会被关闭：")
                                    .to_string(),
                                &running,
                                MAX_LISTED_CHANGES,
                                Color::Warning,
                            ));
                        }
                    }
                }
            }
        }

        let confirm_label = match action {
            FinishAction::Merge => tr("Merge", "合并"),
            FinishAction::Delete => tr("Delete", "删除"),
        };
        let tint = match action {
            FinishAction::Merge => TintColor::Accent,
            FinishAction::Delete => TintColor::Error,
        };
        v_flex()
            .gap_2()
            .child(Label::new(title))
            .child(
                div()
                    .id("agent-task-details")
                    .max_h(rems(24.))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new("agent-task-cancel", tr("Cancel", "取消"))
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = Page::List;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("agent-task-confirm", confirm_label)
                            .style(ButtonStyle::Tinted(tint))
                            .disabled(!can_confirm)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run(action, index, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn lines_section(title: String, lines: &[String], limit: usize, color: Color) -> AnyElement {
    let hidden = lines.len().saturating_sub(limit);
    v_flex()
        .child(Label::new(title).size(LabelSize::Small).color(color))
        .children(
            lines
                .iter()
                .take(limit)
                .map(|line| Label::new(line.clone()).size(LabelSize::Small)),
        )
        .when(hidden > 0, |this| {
            this.child(
                Label::new(if is_chinese() {
                    format!("……还有 {hidden} 项")
                } else {
                    format!("… and {hidden} more")
                })
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
        })
        .into_any_element()
}

impl EventEmitter<DismissEvent> for AgentTasksModal {}

impl Focusable for AgentTasksModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for AgentTasksModal {}

impl Render for AgentTasksModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.page {
            Page::List => self.render_list(cx),
            Page::Confirm {
                action,
                index,
                details,
            } => {
                let (action, index) = (*action, *index);
                let details = details.clone();
                self.render_confirm(action, index, details.as_ref(), cx)
            }
            Page::Working(message) => Label::new(message.clone())
                .color(Color::Accent)
                .into_any_element(),
            Page::Outcome {
                message,
                is_error,
                offer_delete,
            } => {
                let offer_delete = offer_delete.clone();
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("agent-task-outcome")
                            .max_h(rems(24.))
                            .overflow_y_scroll()
                            .child(Label::new(message.clone()).color(if *is_error {
                                Color::Error
                            } else {
                                Color::Default
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .justify_end()
                            .child(
                                Button::new("agent-task-back", tr("Back to Tasks", "返回任务列表"))
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.page = Page::List;
                                        cx.notify();
                                    })),
                            )
                            .when_some(offer_delete, |this, branch| {
                                this.child(
                                    Button::new(
                                        "agent-task-delete-merged",
                                        tr("Delete the Task", "删除这个任务"),
                                    )
                                    .style(ButtonStyle::Tinted(TintColor::Error))
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| match this.index_of(&branch) {
                                            Some(index) => {
                                                this.ask(FinishAction::Delete, index, window, cx)
                                            }
                                            None => {
                                                this.page = Page::List;
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                            }),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .w(rems(40.))
            .p_2()
            .gap_2()
            .elevation_2(cx)
            .key_context("AgentTasks")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .child(
                Label::new(tr("Agent Tasks", "agent 任务"))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(content)
    }
}
