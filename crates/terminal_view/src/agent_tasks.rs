//! SOLO: agent tasks. "New Agent Task" makes a git worktree on a new branch
//! for a task and starts an AI agent (`terminal.agent_task_command`) in a
//! terminal there, named after the task; "Manage Agent Tasks" lists the tasks
//! to review their changes, merge them back or delete them.
//!
//! Tasks live in git (see [`git`]), so they're found again after a restart.
//! [`agent_tasks`] lists them with their terminals.

pub mod config;
pub mod git;
pub mod pull_request;

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
use ui::{ButtonStyle, Checkbox, TintColor, ToggleState, prelude::*};
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

/// SOLO: a new task, made the way the repository's `.solo/tasks.json` says
/// (see `config`), with what its terminal needs.
pub(crate) struct NewTask {
    pub task: AgentTask,
    /// Where its terminal opens: its worktree, or for a task spanning several
    /// repositories the folder with all of them.
    pub directory: PathBuf,
    /// For its terminal: the task's port and the config's variables.
    pub env: collections::HashMap<String, String>,
    /// Run in its terminal before the agent.
    pub setup: Option<String>,
    /// What went wrong after the worktree was made (copying files, a port),
    /// which doesn't undo it.
    pub warnings: Vec<String>,
}

/// Makes a task's branch and worktree, copies files into it and gives it a
/// port, as the repository's config says. Blocks on git.
pub(crate) fn make_task(
    info: &git::RepositoryInfo,
    name: &str,
    worktree_directory_setting: &str,
) -> Result<NewTask> {
    let config = config::TasksConfig::load(&info.main_root)?.unwrap_or_default();
    let plan = match config.worktrees_directory(&info.main_root)? {
        Some(directory) => git::plan_task_in(info, name, directory)?,
        None => git::plan_task(info, name, worktree_directory_setting)?,
    };
    let mut task = git::create_task(info, &plan)?;
    let mut warnings = Vec::new();
    if let Some(worktree) = task.worktree_path.clone()
        && let Err(error) = config::copy_files(&config.copy, &info.main_root, &worktree)
    {
        warnings.push(format!("{error:#}"));
    }
    if let Some(range) = config.port_range {
        let used: Vec<u16> = git::list_tasks(&info.main_root)
            .unwrap_or_default()
            .iter()
            .filter(|other| other.branch != task.branch)
            .filter_map(|other| other.port)
            .collect();
        match config::free_port(range, &used) {
            Some(port) => {
                if let Err(error) = git::set_task_port(&mut task, port) {
                    warnings.push(format!("{error:#}"));
                }
            }
            None => warnings.push(if is_chinese() {
                format!("端口范围 {}–{} 已经用完", range[0], range[1])
            } else {
                format!("every port in {}–{} is taken", range[0], range[1])
            }),
        }
    }
    Ok(NewTask {
        directory: task.worktree_path.clone().unwrap_or_default(),
        env: config.task_env(task.port),
        setup: config.setup.filter(|setup| !setup.trim().is_empty()),
        task,
        warnings,
    })
}

/// Makes a task spanning `repositories`, the first being the one it's made
/// from: a worktree of each on the same new branch, side by side in a folder
/// named for the task, where its agent works. The first repository's config
/// gives the folder's place, the port and the variables; each copies its own
/// files. Setup commands don't run, as each repository's would run in its own
/// worktree. Blocks on git.
pub(crate) fn make_group_task(
    repositories: &[git::RepositoryInfo],
    name: &str,
    worktree_directory_setting: &str,
) -> Result<NewTask> {
    let Some(primary) = repositories.first() else {
        return Err(anyhow!("no repositories"));
    };
    let config = config::TasksConfig::load(&primary.main_root)?.unwrap_or_default();
    let directory = match config.worktrees_directory(&primary.main_root)? {
        Some(directory) => directory,
        None => git::worktrees_directory(&primary.main_root, worktree_directory_setting)?,
    };
    let plans = git::plan_group_task(repositories, name, directory)?;
    let mut tasks: Vec<AgentTask> = Vec::with_capacity(plans.len());
    for (repository, plan) in repositories.iter().zip(&plans) {
        match git::create_task(repository, plan) {
            Ok(task) => tasks.push(task),
            Err(error) => {
                // All or none.
                for task in &tasks {
                    if let Err(rollback_error) = git::delete_task(task) {
                        log::error!("failed to roll back an agent task: {rollback_error:#}");
                    }
                }
                return Err(error.context(format!(
                    "{} {}",
                    tr("in", "仓库"),
                    repository.main_root.display()
                )));
            }
        }
    }
    let mut warnings = Vec::new();
    for task in &tasks {
        let Ok(Some(repository_config)) = config::TasksConfig::load(&task.repository_root) else {
            continue;
        };
        if let Some(worktree) = &task.worktree_path
            && let Err(error) =
                config::copy_files(&repository_config.copy, &task.repository_root, worktree)
        {
            warnings.push(format!("{error:#}"));
        }
        if repository_config
            .setup
            .as_deref()
            .is_some_and(|setup| !setup.trim().is_empty())
        {
            warnings.push(if is_chinese() {
                format!(
                    "{} 的 setup 命令没有运行（跨仓库任务不运行 setup），需要的话请在它的文件夹里手动运行",
                    task.repository_root.display()
                )
            } else {
                format!(
                    "the setup commands of {} didn't run (tasks spanning repositories don't run them); run them in its folder if needed",
                    task.repository_root.display()
                )
            });
        }
    }
    let mut task = tasks.swap_remove(0);
    if let Some(range) = config.port_range {
        let used: Vec<u16> = git::list_tasks(&primary.main_root)
            .unwrap_or_default()
            .iter()
            .filter(|other| other.branch != task.branch)
            .filter_map(|other| other.port)
            .collect();
        if let Some(port) = config::free_port(range, &used)
            && let Err(error) = git::set_task_port(&mut task, port)
        {
            warnings.push(format!("{error:#}"));
        }
    }
    Ok(NewTask {
        directory: task.group.clone().unwrap_or_default(),
        env: config.task_env(task.port),
        setup: None,
        task,
        warnings,
    })
}

/// The task's name; for a part of a task spanning repositories, with its
/// repository's.
pub(crate) fn display_name(task: &AgentTask) -> String {
    match (&task.group, task.repository_root.file_name()) {
        (Some(_), Some(repository)) => {
            format!("{} ({})", task.name, repository.to_string_lossy())
        }
        _ => task.name.clone(),
    }
}

/// Tells tasks apart: the parts of a task spanning repositories share their
/// branch.
fn task_key(task: &AgentTask) -> String {
    format!("{}\n{}", task.repository_root.display(), task.branch)
}

/// What a task's agent is asked to commit its changes with.
pub(crate) fn commit_request(task: &AgentTask) -> String {
    let worktree = task
        .worktree_path
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    if is_chinese() {
        format!(
            "请提交 {worktree}（分支 {}）里所有未提交的改动。根据 git diff 写提交信息，说明改了什么、为什么改，风格和这个仓库的 git log 保持一致。",
            task.branch
        )
    } else {
        format!(
            "Please commit all the uncommitted changes in {worktree} (branch {}). Write the commit message from the diff (git diff): what changed and why, in the style of this repository's git log.",
            task.branch
        )
    }
}

/// The commands a new task's terminal runs: the setup, then the agent.
pub(crate) fn task_commands(setup: Option<String>, agent: Option<String>) -> Option<String> {
    let commands: Vec<String> = setup
        .into_iter()
        .chain(agent)
        .filter(|command| !command.trim().is_empty())
        .collect();
    (!commands.is_empty()).then(|| commands.join("\r"))
}

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
                // A task spanning repositories has its agent in the folder
                // around its worktrees.
                let terminals = task
                    .worktree_path
                    .as_deref()
                    .map(|worktree| {
                        view_directories
                            .iter()
                            .filter(|(_, directory)| {
                                is_within(directory, worktree)
                                    || task
                                        .group
                                        .as_deref()
                                        .is_some_and(|group| is_within(directory, group))
                            })
                            .map(|(view, _)| view.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                AgentTaskEntry { task, terminals }
            })
            .collect())
    })
}

/// Opens a terminal named `title` in a task's `worktree`, with the task's
/// environment from the repository's `.solo/tasks.json`.
pub(crate) fn open_task_terminal_in(
    workspace: &Entity<Workspace>,
    worktree: PathBuf,
    title: String,
    window: &mut Window,
    cx: &mut App,
) {
    let env = cx.background_spawn({
        let worktree = worktree.clone();
        async move { task_env_of_worktree(&worktree) }
    });
    let workspace = workspace.downgrade();
    window
        .spawn(cx, async move |cx| {
            let env = env.await;
            workspace.update_in(cx, |workspace, window, cx| {
                open_task_terminal(workspace, worktree, title, env, None, window, cx)
                    .detach_and_log_err(cx);
            })
        })
        .detach_and_log_err(cx);
}

/// The environment `.solo/tasks.json` gives the task in `worktree`.
fn task_env_of_worktree(worktree: &Path) -> collections::HashMap<String, String> {
    let Ok(info) = git::repository_info(worktree) else {
        return Default::default();
    };
    let port = git::list_tasks(&info.main_root).ok().and_then(|tasks| {
        tasks
            .into_iter()
            .find(|task| {
                task.worktree_path
                    .as_deref()
                    .is_some_and(|path| is_within(path, worktree) && is_within(worktree, path))
            })?
            .port
    });
    config::TasksConfig::load(&info.main_root)
        .ok()
        .flatten()
        .unwrap_or_default()
        .task_env(port)
}

/// Opens a terminal in `directory` named `title`, and types `command` in it
/// once its shell is ready.
pub(crate) fn open_task_terminal(
    workspace: &mut Workspace,
    directory: PathBuf,
    title: String,
    env: collections::HashMap<String, String>,
    command: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Task<Result<Entity<TerminalView>>> {
    let terminal = TerminalPanel::add_center_terminal(workspace, window, cx, move |project, cx| {
        project.create_terminal_shell_with_env(Some(directory), env, cx)
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let terminal = terminal.await?.upgrade().context("the terminal closed")?;
        let workspace = workspace.upgrade().context("the workspace closed")?;
        cx.update(|_, cx| {
            let view = terminal_views(&workspace, cx)
                .into_iter()
                .find(|view| view.read(cx).terminal == terminal)
                .context("the terminal's view is gone")?;
            view.update(cx, |view, cx| view.set_custom_title(Some(title), cx));
            if let Some(command) = command.filter(|command| !command.trim().is_empty()) {
                type_when_ready(terminal, command, cx);
            }
            anyhow::Ok(view)
        })?
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
    /// The workspace's other repositories, which the task can span too.
    other_repositories: Vec<git::RepositoryInfo>,
    include_other_repositories: bool,
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
        let roots: Vec<PathBuf> = workspace
            .project()
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect();
        let weak_workspace = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| {
            Self::new(weak_workspace, directory, roots, window, cx)
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        directory: Option<PathBuf>,
        roots: Vec<PathBuf>,
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
            let (info, others) = cx
                .background_spawn(async move {
                    let info = match directory {
                        Some(directory) => git::repository_info(&directory),
                        None => Err(anyhow!("no directory")),
                    };
                    let mut others: Vec<git::RepositoryInfo> = Vec::new();
                    if let Ok(info) = &info {
                        for root in roots {
                            let Ok(other) = git::repository_info(&root) else {
                                continue;
                            };
                            let same = |left: &Path, right: &Path| {
                                is_within(left, right) && is_within(right, left)
                            };
                            if !same(&other.main_root, &info.main_root)
                                && !others
                                    .iter()
                                    .any(|known| same(&known.main_root, &other.main_root))
                            {
                                others.push(other);
                            }
                        }
                    }
                    (info, others)
                })
                .await;
            this.update(cx, |this, cx| {
                this.other_repositories = others;
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
            other_repositories: Vec::new(),
            include_other_repositories: false,
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
        let others = if self.include_other_repositories {
            self.other_repositories.clone()
        } else {
            Vec::new()
        };
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
                        if others.is_empty() {
                            make_task(&info, &name, &setting)
                        } else {
                            let repositories: Vec<_> =
                                std::iter::once(info).chain(others).collect();
                            make_group_task(&repositories, &name, &setting)
                        }
                    }
                })
                .await;
            let NewTask {
                task,
                directory,
                env,
                setup,
                warnings,
            } = match created {
                Ok(new_task) => new_task,
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
            let opened = workspace.update_in(cx, |workspace, window, cx| {
                if !warnings.is_empty() {
                    // The workspace is being updated: not through `show_toast`.
                    struct TaskSetupToast;
                    workspace.show_toast(
                        Toast::new(
                            NotificationId::unique::<TaskSetupToast>(),
                            warnings.join("\n"),
                        )
                        .autohide(),
                        cx,
                    );
                }
                open_task_terminal(
                    workspace,
                    directory,
                    task.name.clone(),
                    env,
                    task_commands(setup, Some(command)),
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
                    let spans = self.include_other_repositories
                        && !self.other_repositories.is_empty();
                    let text = if spans && is_chinese() {
                        format!(
                            "在每个仓库从当前分支新建分支 {new_branch}，工作区都放在 {}",
                            directory.display()
                        )
                    } else if spans {
                        format!(
                            "New branch {new_branch} in each repository from its current branch, worktrees side by side in {}",
                            directory.display()
                        )
                    } else if is_chinese() {
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
                    .when(!self.other_repositories.is_empty(), |this| {
                        let names: Vec<String> = self
                            .other_repositories
                            .iter()
                            .filter_map(|other| other.main_root.file_name())
                            .map(|name| name.to_string_lossy().into_owned())
                            .collect();
                        let label = if is_chinese() {
                            format!("同时在工作区的其他仓库里建：{}", names.join("、"))
                        } else {
                            format!(
                                "Also in the workspace's other repositories: {}",
                                names.join(", ")
                            )
                        };
                        this.child(
                            Checkbox::new(
                                "agent-task-other-repositories",
                                if self.include_other_repositories {
                                    ToggleState::Selected
                                } else {
                                    ToggleState::Unselected
                                },
                            )
                            .label(label)
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(
                                |this, state: &ToggleState, _, cx| {
                                    this.include_other_repositories = state.selected();
                                    cx.notify();
                                },
                            )),
                        )
                    })
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
        /// Offer to delete this task (by [`task_key`], since the list is
        /// reloaded meanwhile), e.g. once it's merged.
        offer_delete: Option<String>,
        /// Offer to ask the agent of this task (by [`task_key`]) to commit.
        offer_commit: Option<String>,
    },
}

/// Lists the agent tasks, to open their terminal, review, merge or delete
/// them.
pub struct AgentTasksModal {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    entries: Option<Result<Vec<AgentTaskEntry>, SharedString>>,
    /// The tasks' pull requests, by branch, once looked up.
    pull_requests: collections::HashMap<String, pull_request::PullRequest>,
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
            let modal = Self {
                workspace: weak_workspace,
                focus_handle: cx.focus_handle(),
                entries: None,
                pull_requests: Default::default(),
                selected: 0,
                active_directory,
                page: Page::List,
                _task: Task::ready(()),
            };
            // Loading reads the workspace's terminals, and the workspace is
            // being updated while the modal is made: load once it isn't.
            cx.defer_in(window, |modal: &mut Self, window, cx| {
                modal.reload(window, cx)
            });
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
                let tasks: Vec<AgentTask> = entries
                    .iter()
                    .flatten()
                    .map(|entry| entry.task.clone())
                    .collect();
                this.entries = Some(entries);
                cx.notify();
                // Looking pull requests up asks GitHub; the list shows first.
                cx.spawn(async move |this, cx| {
                    let found = cx
                        .background_spawn(async move {
                            tasks
                                .into_iter()
                                .filter_map(|task| {
                                    let worktree = task.worktree_path.as_deref()?;
                                    let pull_request =
                                        pull_request::find_pull_request(worktree, &task.branch)
                                            .ok()??;
                                    Some((task_key(&task), pull_request))
                                })
                                .collect::<collections::HashMap<_, _>>()
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        this.pull_requests = found;
                        cx.notify();
                    })
                    .log_err();
                })
                .detach();
            })
            .log_err();
        });
    }

    /// Pushes the task's branch and opens a pull request for it.
    fn open_pull_request(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        self.page =
            Page::Working(tr("Pushing and opening a pull request…", "正在推送并开 PR…").into());
        cx.notify();
        let task = entry.task;
        self._task = cx.spawn_in(window, async move |this, cx| {
            let uncommitted = cx
                .background_spawn({
                    let worktree = task.worktree_path.clone();
                    async move {
                        worktree
                            .and_then(|worktree| git::uncommitted_changes(&worktree).ok())
                            .unwrap_or_default()
                    }
                })
                .await;
            if !uncommitted.is_empty() {
                let message = if is_chinese() {
                    format!(
                        "{} 有 {} 个文件的改动还没提交，PR 里不会有它们。可以让任务的 agent 根据 diff 写提交信息并提交，然后再开 PR。",
                        task.branch,
                        uncommitted.len()
                    )
                } else {
                    format!(
                        "{} has uncommitted changes in {} files, which a pull request wouldn't have. The task's agent can commit them with a message written from the diff; then open the pull request.",
                        task.branch,
                        uncommitted.len()
                    )
                };
                this.update(cx, |this, cx| {
                    this.page = Page::Outcome {
                        message: message.into(),
                        is_error: false,
                        offer_delete: None,
                        offer_commit: Some(task_key(&task)),
                    };
                    cx.notify();
                })
                .log_err();
                return;
            }
            let opened = cx
                .background_spawn(async move { pull_request::open_pull_request(&task) })
                .await;
            let (message, is_error) = match opened {
                Ok(pull_request) => (
                    if is_chinese() {
                        format!("已开 PR #{}：{}", pull_request.number, pull_request.url)
                    } else {
                        format!(
                            "Opened pull request #{}: {}",
                            pull_request.number, pull_request.url
                        )
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
                offer_commit: None,
                };
                this.reload(window, cx);
            })
            .log_err();
        });
    }

    /// Pastes what the pull request's checks and reviewers said into the
    /// task's agent; without one, copies it.
    fn send_pull_request_feedback(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        let Some(pull_request) = self.pull_requests.get(&task_key(&entry.task)).cloned() else {
            return;
        };
        let Some(worktree) = entry.task.worktree_path.clone() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        self.page = Page::Working(
            tr(
                "Reading the pull request's feedback…",
                "正在读取 PR 的反馈…",
            )
            .into(),
        );
        cx.notify();
        self._task = cx.spawn_in(window, async move |this, cx| {
            let number = pull_request.number;
            let feedback = cx
                .background_spawn(async move {
                    pull_request::pull_request_feedback(&worktree, number)
                })
                .await;
            let outcome = this.update_in(cx, |_, window, cx| match feedback {
                Err(error) => (format!("{error:#}"), true),
                Ok(feedback) if feedback.is_empty() => (
                    tr(
                        "No failed checks or comments on the pull request.",
                        "PR 上没有失败的检查，也没有评论。",
                    )
                    .to_string(),
                    false,
                ),
                Ok(feedback) => {
                    let message = pull_request::feedback_message(&pull_request, &feedback);
                    let agent = entry
                        .terminals
                        .iter()
                        .find(|view| crate::agent_context::can_paste_to_agent(view.read(cx), &message, cx))
                        .cloned();
                    match agent {
                        Some(agent) => {
                            agent.update(cx, |agent, cx| {
                                agent.terminal.update(cx, |terminal, _| terminal.paste(&message))
                            });
                            let workspace = workspace.clone();
                            window.defer(cx, move |window, cx| {
                                activate_terminal_view(&workspace, &agent, window, cx)
                            });
                            (
                                tr(
                                    "Sent the failed checks and comments to the task's agent.",
                                    "已把失败的检查和评论发给任务的 agent。",
                                )
                                .to_string(),
                                false,
                            )
                        }
                        None => {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message));
                            (
                                tr(
                                    "No agent is taking pasted text in the task's terminals; copied the failed checks and comments to the clipboard instead.",
                                    "任务的终端里没有能接收粘贴的 agent，已把失败的检查和评论复制到剪贴板。",
                                )
                                .to_string(),
                                false,
                            )
                        }
                    }
                }
            });
            if let Ok((message, is_error)) = outcome {
                this.update(cx, |this, cx| {
                    this.page = Page::Outcome {
                        message: message.into(),
                        is_error,
                        offer_delete: None,
                    offer_commit: None,
                    };
                    cx.notify();
                })
                .log_err();
            }
        });
    }

    /// Asks the task's agent to commit its changes, with a message it writes
    /// from the diff.
    fn ask_agent_to_commit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(index).cloned() else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let message = commit_request(&entry.task);
        let agent = entry
            .terminals
            .iter()
            .find(|view| crate::agent_context::can_paste_to_agent(view.read(cx), &message, cx))
            .cloned();
        let (outcome, is_error) = match agent {
            Some(agent) => {
                agent.update(cx, |agent, cx| {
                    agent
                        .terminal
                        .update(cx, |terminal, _| terminal.paste(&message))
                });
                window.defer(cx, move |window, cx| {
                    activate_terminal_view(&workspace, &agent, window, cx)
                });
                (
                    tr(
                        "Asked the task's agent to commit; press Enter in its terminal to send it.",
                        "已把提交请求粘贴给任务的 agent，在它的终端里按回车发送。",
                    ),
                    false,
                )
            }
            None => (
                tr(
                    "No agent is taking pasted text in the task's terminals. Start one there, or commit yourself.",
                    "任务的终端里没有能接收粘贴的 agent。请先在那里启动一个，或者自己提交。",
                ),
                true,
            ),
        };
        self.page = Page::Outcome {
            message: outcome.into(),
            is_error,
            offer_delete: None,
            offer_commit: None,
        };
        cx.notify();
    }

    fn entry(&self, index: usize) -> Option<&AgentTaskEntry> {
        self.entries.as_ref()?.as_ref().ok()?.get(index)
    }

    /// The terminals deleting the task closes: those in its worktree. A task
    /// spanning repositories keeps its agent in the folder around them while
    /// another repository's part is left.
    fn terminals_closed_by_deleting(
        &self,
        entry: &AgentTaskEntry,
        cx: &App,
    ) -> Vec<Entity<TerminalView>> {
        let Some(group) = &entry.task.group else {
            return entry.terminals.clone();
        };
        let key = task_key(&entry.task);
        let other_parts = self
            .entries
            .as_ref()
            .and_then(|entries| entries.as_ref().ok())
            .is_some_and(|entries| {
                entries.iter().any(|other| {
                    other.task.group.as_ref() == Some(group) && task_key(&other.task) != key
                })
            });
        if !other_parts {
            return entry.terminals.clone();
        }
        entry
            .terminals
            .iter()
            .filter(|view| {
                entry.task.worktree_path.as_deref().is_some_and(|worktree| {
                    terminal_directory(view.read(cx).terminal.read(cx))
                        .is_some_and(|directory| is_within(&directory, worktree))
                })
            })
            .cloned()
            .collect()
    }

    /// The index of the task with `key` (see [`task_key`]).
    fn index_of(&self, key: &str) -> Option<usize> {
        self.entries
            .as_ref()?
            .as_ref()
            .ok()?
            .iter()
            .position(|entry| task_key(&entry.task) == key)
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
                let env = config::TasksConfig::load(&entry.task.repository_root)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .task_env(entry.task.port);
                open_task_terminal(workspace, directory, entry.task.name, env, None, window, cx)
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
                    Some(task_key(&task)),
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
                offer_commit: None,
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
        let closed = self.terminals_closed_by_deleting(&entry, cx);
        let closing = close_terminals(&workspace, &closed, window, cx);
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
                    async move {
                        // The repository's teardown runs first; failing, it
                        // doesn't keep the task.
                        let teardown = config::TasksConfig::load(&task.repository_root)
                            .ok()
                            .flatten()
                            .and_then(|config| config.teardown)
                            .filter(|teardown| !teardown.trim().is_empty());
                        let teardown_error = match (teardown, &task.worktree_path) {
                            (Some(teardown), Some(worktree)) if worktree.exists() => {
                                config::run_teardown(&teardown, worktree).err()
                            }
                            _ => None,
                        };
                        git::delete_task(&task)?;
                        anyhow::Ok(teardown_error)
                    }
                })
                .await;
            let (message, is_error) = match deleted {
                Ok(None) => (
                    if is_chinese() {
                        format!("已删除任务 {} 的工作区和分支。", task.name)
                    } else {
                        format!("Deleted the worktree and branch of {}.", task.name)
                    },
                    false,
                ),
                Ok(Some(teardown_error)) => (
                    if is_chinese() {
                        format!(
                            "已删除任务 {} 的工作区和分支，但清理命令失败：{teardown_error:#}",
                            task.name
                        )
                    } else {
                        format!(
                            "Deleted the worktree and branch of {}, but the teardown failed: {teardown_error:#}",
                            task.name
                        )
                    },
                    true,
                ),
                Err(error) => (format!("{error:#}"), true),
            };
            this.update_in(cx, |this, window, cx| {
                this.page = Page::Outcome {
                    message: message.into(),
                    is_error,
                    offer_delete: None,
                offer_commit: None,
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
                                this.child(
                                    workspace::agent_activity::AgentStateIcon::new(status)
                                        .size(IconSize::Small),
                                )
                            })
                            .child(Label::new(display_name(task)))
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
                            .map(|this| match self.pull_requests.get(&task_key(task)) {
                                Some(pull_request) => {
                                    let url = pull_request.url.clone();
                                    this.child(
                                        Button::new(
                                            ("pull-request", index),
                                            format!("PR #{}", pull_request.number),
                                        )
                                        .style(ButtonStyle::Subtle)
                                        .label_size(LabelSize::Small)
                                        .tooltip(ui::Tooltip::text(url.clone()))
                                        .on_click(move |_, _, cx| cx.open_url(&url)),
                                    )
                                    .child(
                                        Button::new(
                                            ("pull-request-feedback", index),
                                            tr("Send PR Feedback to Agent", "把 PR 反馈发给 agent"),
                                        )
                                        .style(ButtonStyle::Subtle)
                                        .label_size(LabelSize::Small)
                                        .disabled(!has_worktree)
                                        .tooltip(ui::Tooltip::text(tr(
                                            "Paste the failed checks and the comments into the task's agent",
                                            "把失败的检查和评论粘贴给任务的 agent",
                                        )))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.send_pull_request_feedback(index, window, cx)
                                        })),
                                    )
                                }
                                None => this.child(
                                    Button::new(
                                        ("open-pull-request", index),
                                        tr("Push and Open PR", "推送并开 PR"),
                                    )
                                    .style(ButtonStyle::Subtle)
                                    .label_size(LabelSize::Small)
                                    .disabled(!has_worktree)
                                    .tooltip(ui::Tooltip::text(tr(
                                        "Push the branch to origin and open a pull request with gh, titled from its commits",
                                        "把分支推送到 origin，用 gh 开一个 PR，标题和说明取自它的提交",
                                    )))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_pull_request(index, window, cx)
                                    })),
                                ),
                            })
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
                format!(
                    "删除任务“{}”的工作区和分支 {}？",
                    display_name(task),
                    task.branch
                )
            }
            (FinishAction::Delete, false) => format!(
                "Delete the worktree and branch {} of \"{}\"?",
                task.branch,
                display_name(task)
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
                            body = body
                                .child(lines_section(
                                    tr(
                                        "Uncommitted changes in the task, which won't be merged:",
                                        "任务里未提交的改动（不会被合并）：",
                                    )
                                    .to_string(),
                                    &details.uncommitted,
                                    MAX_LISTED_CHANGES,
                                    Color::Warning,
                                ))
                                .child(
                                    h_flex().child(
                                        Button::new(
                                            "agent-task-commit-first",
                                            tr(
                                                "Ask the Agent to Commit Them First",
                                                "先让 agent 提交它们",
                                            ),
                                        )
                                        .style(ButtonStyle::Subtle)
                                        .label_size(LabelSize::Small)
                                        .tooltip(ui::Tooltip::text(tr(
                                            "Paste a request into the task's agent to commit, with a message written from the diff",
                                            "把提交请求粘贴给任务的 agent，让它根据 diff 写提交信息",
                                        )))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.ask_agent_to_commit(index, window, cx)
                                        })),
                                    ),
                                );
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
                        let closed = self.terminals_closed_by_deleting(entry, cx);
                        if !closed.is_empty() {
                            let running: Vec<String> = closed
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
                offer_commit,
            } => {
                let offer_delete = offer_delete.clone();
                let offer_commit = offer_commit.clone();
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
                            .when_some(offer_commit, |this, branch| {
                                this.child(
                                    Button::new(
                                        "agent-task-ask-commit",
                                        tr("Ask the Agent to Commit", "让 agent 提交"),
                                    )
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            if let Some(index) = this.index_of(&branch) {
                                                this.ask_agent_to_commit(index, window, cx)
                                            }
                                        },
                                    )),
                                )
                            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use settings::SettingsStore;
    use util::path;

    /// Opening the task list from a workspace action reads the workspace's
    /// terminals; that panicked while the workspace was being updated.
    #[gpui::test]
    async fn test_manage_agent_tasks_opens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), serde_json::json!({ "a.txt": "a" }))
            .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            AgentTasksModal::toggle(workspace, window, cx);
        });
        cx.run_until_parked();

        workspace.update(cx, |workspace, cx| {
            assert!(workspace.active_modal::<AgentTasksModal>(cx).is_some());
        });
    }
}
