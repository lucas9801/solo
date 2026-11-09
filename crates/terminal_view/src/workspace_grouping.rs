//! SOLO: keeps each terminal in the workspace tab of the project it's in.
//!
//! A terminal belongs in the tab with a folder its directory is in, or with a
//! folder in the same git repository; a linked worktree, like an agent task's,
//! counts as its main repository. The tab with the most specific match wins.
//!
//! - Terminals restored from before there were workspace tabs, when one
//!   workspace held all of a window's terminals, move once to their
//!   repository's tab, which opens in the background if it isn't open.
//! - When a terminal's shell shows its prompt in a project another tab has,
//!   Solo offers to move the terminal there. A folder in several tabs
//!   belongs to the one it's the first folder of.
//! - A workspace holding unrelated projects (several repositories, say from
//!   when following the terminal added folders) is offered to be split into
//!   a tab each, once; its terminals go with their folders.
//!
//! Neither happens while `terminal.follow_working_directory` is on: then one
//! workspace follows its terminals instead.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use futures::{FutureExt as _, future::Shared};
use gpui::{App, AppContext as _, Context, DismissEvent, Entity, Global, Task, Window};
use project::Fs;
use settings::{Settings as _, SettingsStore};
use terminal::{Event as TerminalEvent, terminal_settings::TerminalSettings};
use util::ResultExt as _;
use workspace::{
    MultiWorkspace, OpenMode, ProjectGroupKey, Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

use crate::{
    TerminalView, command_blocks::activate_terminal_view, follow_working_directory,
    terminal_panel::TerminalPanel,
};

/// Set once restored terminals were grouped, the first time Solo ran with
/// workspace tabs.
const GROUPED_KEY: &str = "solo_terminals_grouped";

/// How long to wait for a workspace that just opened to have its terminal
/// panel.
const PANEL_TIMEOUT: Duration = Duration::from_secs(10);

/// What a terminal's grouping remembers.
#[derive(Default)]
pub(crate) struct GroupingState {
    /// The directory its shell showed its last prompt in.
    directory: Option<PathBuf>,
    /// The tabs (by their first folder) the user chose not to move it to.
    kept_out_of: HashSet<PathBuf>,
}

/// Whether terminals restored in this run still get grouped: `None` until
/// that's known.
#[derive(Default)]
struct GroupRestoredTerminals(Option<bool>);

impl Global for GroupRestoredTerminals {}

/// Workspaces being opened for terminals to move to, by their folder, so
/// terminals of the same project share one.
#[derive(Default)]
struct OpeningWorkspaces(HashMap<PathBuf, Shared<Task<Option<Entity<Workspace>>>>>);

impl Global for OpeningWorkspaces {}

struct MoveTerminalNotification;
struct SplitWorkspaceNotification;

/// Prefix of the keys remembering the workspaces kept together.
const KEPT_TOGETHER_KEY: &str = "solo_workspace_kept_together:";
/// How long after a workspace opens its folders are looked at, so its
/// worktrees are there.
const SPLIT_CHECK_DELAY: Duration = Duration::from_secs(5);

pub(crate) fn init(cx: &mut App) {
    if cx.has_global::<db::AppDatabase>() {
        let kvp = KeyValueStore::global(cx);
        cx.spawn(async move |cx| {
            let grouped = cx
                .background_spawn({
                    let kvp = kvp.clone();
                    async move { kvp.read_kvp(GROUPED_KEY) }
                })
                .await
                .log_err()
                .flatten()
                .is_some();
            if !grouped {
                cx.background_spawn(async move {
                    kvp.write_kvp(GROUPED_KEY.to_string(), "1".to_string())
                        .await
                })
                .await
                .log_err();
            }
            cx.update(|cx| cx.set_global(GroupRestoredTerminals(Some(!grouped))));
        })
        .detach();
    }

    cx.observe_new(|_: &mut Workspace, window, cx| {
        if let Some(window) = window {
            check_unrelated_folders(window, cx);
        }
    })
    .detach();

    cx.observe_new(|view: &mut TerminalView, window, cx| {
        let Some(window) = window else {
            return;
        };
        let terminal = view.terminal.clone();
        cx.subscribe_in(&terminal, window, |view, _, event, window, cx| {
            if *event == TerminalEvent::PromptStarted {
                prompt_started(view, window, cx);
            }
        })
        .detach();
    })
    .detach();
}

/// A folder of a workspace, and the repository it's in.
#[derive(Clone, Debug, PartialEq)]
struct Root {
    path: PathBuf,
    repository: Option<PathBuf>,
}

/// How well `roots` fit `directory` in `repository`: the more path
/// components the matching folder or repository has, the better; between
/// equals, the workspace whose first folder it is. `None` when none fit.
fn fit(roots: &[Root], directory: &Path, repository: Option<&Path>) -> Option<usize> {
    let directory = comparable(directory);
    let repository = repository.map(comparable);
    roots
        .iter()
        .enumerate()
        .filter_map(|(ix, root)| {
            let first = usize::from(ix == 0);
            let path = comparable(&root.path);
            if directory.starts_with(&path) {
                Some(path.components().count() * 2 + first)
            } else {
                let root_repository = root.repository.as_deref().map(comparable);
                (repository.is_some() && root_repository == repository).then(|| {
                    repository
                        .as_ref()
                        .map_or(0, |path| path.components().count())
                        * 2
                        + first
                })
            }
        })
        .max()
}

/// Of the `others`' roots, the index of the one that fits `directory` better
/// than `current` does.
fn better_fit(
    current: &[Root],
    others: &[Vec<Root>],
    directory: &Path,
    repository: Option<&Path>,
) -> Option<usize> {
    let current = fit(current, directory, repository);
    others
        .iter()
        .enumerate()
        .filter_map(|(ix, roots)| Some((ix, fit(roots, directory, repository)?)))
        .filter(|(_, fit)| current.is_none_or(|current| *fit > current))
        .max_by_key(|(_, fit)| *fit)
        .map(|(ix, _)| ix)
}

/// Paths compared as Windows does, ignoring case.
fn comparable(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

/// `path` with `.` and `..` resolved, without touching the file system.
fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component),
        }
    }
    result
}

/// The git repository `directory` is in; a linked worktree's is its main
/// repository. `None` outside one, and for the home directory (some keep their
/// dotfiles in a repository there).
async fn repository_root(fs: &dyn Fs, directory: &Path, home: &Path) -> Option<PathBuf> {
    for ancestor in directory.ancestors() {
        if ancestor == home || ancestor.parent().is_none() {
            return None;
        }
        let dot_git = ancestor.join(".git");
        let Some(metadata) = fs.metadata(&dot_git).await.ok().flatten() else {
            continue;
        };
        if metadata.is_dir {
            return Some(ancestor.to_path_buf());
        }
        return Some(
            main_repository(fs, ancestor, &dot_git)
                .await
                .unwrap_or_else(|| ancestor.to_path_buf()),
        );
    }
    None
}

/// The main repository of the linked worktree at `worktree`, whose `.git` file
/// names its git directory there.
async fn main_repository(fs: &dyn Fs, worktree: &Path, dot_git: &Path) -> Option<PathBuf> {
    let contents = fs.load(dot_git).await.ok()?;
    let git_dir = normalized(&worktree.join(contents.trim().strip_prefix("gitdir:")?.trim()));
    let common_dir = fs.load(&git_dir.join("commondir")).await.ok()?;
    let common_dir = normalized(&git_dir.join(common_dir.trim()));
    if common_dir.file_name()? != ".git" {
        return None;
    }
    common_dir.parent().map(Path::to_path_buf)
}

async fn roots_of(fs: &dyn Fs, paths: Vec<PathBuf>, home: &Path) -> Vec<Root> {
    let mut roots = Vec::with_capacity(paths.len());
    for path in paths {
        let repository = repository_root(fs, &path, home).await;
        roots.push(Root { path, repository });
    }
    roots
}

/// Another workspace tab a terminal can move to.
#[derive(Clone)]
struct Destination {
    /// `None` for a tab to open, of a repository no tab has.
    key: Option<ProjectGroupKey>,
    paths: Vec<PathBuf>,
    workspace: Option<Entity<Workspace>>,
}

impl Destination {
    fn name(&self) -> String {
        self.paths
            .first()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

fn prompt_started(view: &mut TerminalView, window: &mut Window, cx: &mut Context<TerminalView>) {
    if !cx.has_global::<SettingsStore>()
        || TerminalSettings::get_global(cx).follow_working_directory
    {
        return;
    }
    let Some(directory) = follow_working_directory::terminal_directory(view.terminal.read(cx))
    else {
        return;
    };
    let restored = std::mem::take(&mut view.restored_at_startup);
    if view.grouping.directory.as_ref() == Some(&directory) {
        return;
    }
    view.grouping.directory = Some(directory.clone());
    let group_restored = restored
        && cx
            .try_global::<GroupRestoredTerminals>()
            .is_some_and(|group| group.0 == Some(true));

    let Some(workspace) = view.workspace.upgrade() else {
        return;
    };
    // Only the workspace's own terminals, not one shown elsewhere.
    let view_handle = cx.entity();
    if !crate::command_blocks::terminal_views(&workspace, cx).contains(&view_handle) {
        return;
    }
    let Some(multi_workspace) = workspace
        .read(cx)
        .multi_workspace()
        .and_then(|multi_workspace| multi_workspace.upgrade())
    else {
        return;
    };
    let project = workspace.read(cx).project().read(cx);
    if !project.is_local() {
        return;
    }
    let fs = project.fs().clone();
    let current_key = workspace.read(cx).project_group_key(cx);
    let current_paths: Vec<PathBuf> = current_key.path_list().ordered_paths().cloned().collect();
    let destinations: Vec<Destination> = {
        let multi_workspace = multi_workspace.read(cx);
        multi_workspace
            .project_groups(cx)
            .into_iter()
            .filter(|group| group.key != current_key && !group.workspaces.contains(&workspace))
            .filter(|group| !group.key.path_list().paths().is_empty())
            .map(|group| Destination {
                workspace: multi_workspace
                    .last_active_workspace_for_group(&group.key, cx)
                    .or_else(|| group.workspaces.first().cloned()),
                paths: group.key.path_list().ordered_paths().cloned().collect(),
                key: Some(group.key),
            })
            .collect()
    };
    let kept_out_of = view.grouping.kept_out_of.clone();

    cx.spawn_in(window, async move |view, cx| {
        let home = paths::home_dir().clone();
        let repository = repository_root(fs.as_ref(), &directory, &home).await;
        let current = roots_of(fs.as_ref(), current_paths, &home).await;
        let mut others = Vec::with_capacity(destinations.len());
        for destination in &destinations {
            others.push(roots_of(fs.as_ref(), destination.paths.clone(), &home).await);
        }
        let destination = match better_fit(&current, &others, &directory, repository.as_deref()) {
            Some(ix) => destinations[ix].clone(),
            // Restored terminals get a tab of their own repository.
            None if group_restored
                && fit(&current, &directory, repository.as_deref()).is_none() =>
            {
                let Some(repository) = repository else {
                    return;
                };
                Destination {
                    key: None,
                    paths: vec![repository],
                    workspace: None,
                }
            }
            None => return,
        };
        let Some(view) = view.upgrade() else {
            return;
        };
        cx.update(|window, cx| {
            if group_restored {
                move_terminal(view, multi_workspace, destination, false, window, cx);
            } else if !destination
                .paths
                .first()
                .is_some_and(|path| kept_out_of.contains(path))
            {
                offer_move(view, &workspace, multi_workspace, destination, cx);
            }
        })
        .ok();
    })
    .detach();
}

/// The folders of `roots` unrelated to the first one: neither in it nor in
/// its repository, nor in a repository another of them is in.
fn unrelated_roots(roots: &[Root]) -> Vec<Root> {
    let Some((first, rest)) = roots.split_first() else {
        return Vec::new();
    };
    let first_path = comparable(&first.path);
    let first_repository = first.repository.as_deref().map(comparable);
    rest.iter()
        .filter(|root| {
            let path = comparable(&root.path);
            let repository = root.repository.as_deref().map(comparable);
            !path.starts_with(&first_path)
                && !first_path.starts_with(&path)
                && (repository.is_none() || repository != first_repository)
        })
        .cloned()
        .collect()
}

/// Offers, once, to split a workspace whose folders are unrelated projects
/// into a tab each.
fn check_unrelated_folders(window: &mut Window, cx: &mut Context<Workspace>) {
    if !cx.has_global::<db::AppDatabase>() || !cx.has_global::<SettingsStore>() {
        return;
    }
    let kvp = KeyValueStore::global(cx);
    cx.spawn_in(window, async move |workspace, cx| {
        cx.background_executor().timer(SPLIT_CHECK_DELAY).await;
        let Some((fs, paths)) = workspace
            .update(cx, |workspace, cx| {
                if TerminalSettings::get_global(cx).follow_working_directory {
                    return None;
                }
                let project = workspace.project().read(cx);
                if !project.is_local() {
                    return None;
                }
                let paths: Vec<PathBuf> = project
                    .visible_worktrees(cx)
                    .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                    .collect();
                (paths.len() > 1).then(|| (project.fs().clone(), paths))
            })
            .ok()
            .flatten()
        else {
            return;
        };
        let home = paths::home_dir().clone();
        let roots = roots_of(fs.as_ref(), paths.clone(), &home).await;
        let unrelated = unrelated_roots(&roots);
        if unrelated.is_empty() {
            return;
        }
        let key = format!(
            "{KEPT_TOGETHER_KEY}{}",
            paths
                .iter()
                .map(|path| comparable(path).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("|")
        );
        let kept = cx
            .background_spawn({
                let kvp = kvp.clone();
                let key = key.clone();
                async move { kvp.read_kvp(&key) }
            })
            .await
            .log_err()
            .flatten()
            .is_some();
        if kept {
            return;
        }
        workspace
            .update_in(cx, |_, window, cx| {
                offer_split(cx.entity(), roots, unrelated, key, kvp, window, cx)
            })
            .ok();
    })
    .detach();
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn offer_split(
    workspace: Entity<Workspace>,
    roots: Vec<Root>,
    unrelated: Vec<Root>,
    key: String,
    kvp: KeyValueStore,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let all = roots
        .iter()
        .map(|root| folder_name(&root.path))
        .collect::<Vec<_>>()
        .join(util::i18n::tr(", ", "、"));
    let split = unrelated
        .iter()
        .map(|root| folder_name(&root.path))
        .collect::<Vec<_>>()
        .join(util::i18n::tr(", ", "、"));
    let message = if util::i18n::is_chinese() {
        format!(
            "这个工作区里有互不相关的项目：{all}。要把 {split} 拆成各自的工作区标签吗？它们的终端会一起移过去，正在运行的程序不受影响。"
        )
    } else {
        format!(
            "This workspace holds unrelated projects: {all}. Split {split} into workspace tabs of their own? Their terminals go with them, and what runs in them carries on."
        )
    };
    let id = NotificationId::composite::<SplitWorkspaceNotification>((
        "split-workspace",
        workspace.entity_id().as_u64() as usize,
    ));
    workspace.update(cx, |workspace, cx| {
        workspace.show_notification(id, cx, |cx| {
            cx.new(|cx| {
                MessageNotification::new(message, cx)
                    .primary_message(util::i18n::tr("Split", "拆分"))
                    .primary_on_click({
                        let unrelated = unrelated.clone();
                        move |window, cx| {
                            split_workspace_now(&unrelated, window, cx);
                            cx.emit(DismissEvent);
                        }
                    })
                    .secondary_message(util::i18n::tr("Keep Together", "保持现状"))
                    .secondary_on_click(move |_, cx| {
                        let kvp = kvp.clone();
                        let key = key.clone();
                        cx.background_spawn(async move {
                            kvp.write_kvp(key, "1".to_string()).await.log_err();
                        })
                        .detach();
                        cx.emit(DismissEvent);
                    })
            })
        });
    });
}

/// Splits `unrelated` off the active workspace of the window: each becomes
/// a tab of its own (or joins the tab it's the first folder of), with the
/// terminals in it.
fn split_workspace_now(unrelated: &[Root], window: &mut Window, cx: &mut App) {
    let Some(multi_workspace) = window.root::<MultiWorkspace>().flatten() else {
        return;
    };
    let workspace = multi_workspace.read(cx).workspace().clone();
    let views = crate::command_blocks::terminal_views(&workspace, cx);
    let groups = multi_workspace.read(cx).project_groups(cx);
    for root in unrelated {
        let path = comparable(&root.path);
        // The tab it's the first folder of, if there is one.
        let existing = groups.iter().find(|group| {
            !group.workspaces.contains(&workspace)
                && group
                    .key
                    .path_list()
                    .ordered_paths()
                    .next()
                    .is_some_and(|first| comparable(first) == path)
        });
        let destination = match existing {
            Some(group) => Destination {
                workspace: multi_workspace
                    .read(cx)
                    .last_active_workspace_for_group(&group.key, cx)
                    .or_else(|| group.workspaces.first().cloned()),
                paths: group.key.path_list().ordered_paths().cloned().collect(),
                key: Some(group.key.clone()),
            },
            None => Destination {
                key: None,
                paths: vec![root.path.clone()],
                workspace: None,
            },
        };
        // Its terminals: those whose directory is in it.
        let moving: Vec<Entity<TerminalView>> = views
            .iter()
            .filter(|view| {
                follow_working_directory::terminal_directory(view.read(cx).terminal.read(cx))
                    .is_some_and(|directory| comparable(&directory).starts_with(&path))
            })
            .cloned()
            .collect();
        if moving.is_empty() {
            let opening = destination_workspace(&multi_workspace, &destination, false, window, cx);
            cx.background_spawn(opening).detach();
        }
        for view in moving {
            move_terminal(
                view,
                multi_workspace.clone(),
                destination.clone(),
                false,
                window,
                cx,
            );
        }
        // The folder leaves this workspace.
        workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                let worktree = project
                    .visible_worktrees(cx)
                    .find(|worktree| comparable(&worktree.read(cx).abs_path()) == path)
                    .map(|worktree| worktree.read(cx).id());
                if let Some(worktree) = worktree {
                    project.remove_worktree(worktree, cx);
                }
            })
        });
    }
}

/// Offers to move the terminal of `view` to `destination`, in a notification.
fn offer_move(
    view: Entity<TerminalView>,
    workspace: &Entity<Workspace>,
    multi_workspace: Entity<MultiWorkspace>,
    destination: Destination,
    cx: &mut App,
) {
    let name = destination.name();
    let message = if util::i18n::is_chinese() {
        format!("这个终端现在在 {name} 里，它有自己的工作区标签。要把终端移过去吗？")
    } else {
        format!(
            "This terminal is now in {name}, which has a workspace tab of its own. Move the terminal there?"
        )
    };
    let id = NotificationId::composite::<MoveTerminalNotification>((
        "move-terminal",
        view.entity_id().as_u64() as usize,
    ));
    workspace.update(cx, |workspace, cx| {
        workspace.show_notification(id, cx, |cx| {
            cx.new(|cx| {
                let move_view = view.clone();
                let keep_view = view.clone();
                let kept_path = destination.paths.first().cloned();
                MessageNotification::new(message, cx)
                    .primary_message(util::i18n::tr("Move to That Tab", "移到那个标签"))
                    .primary_on_click(move |window, cx| {
                        move_terminal(
                            move_view.clone(),
                            multi_workspace.clone(),
                            destination.clone(),
                            true,
                            window,
                            cx,
                        );
                        cx.emit(DismissEvent);
                    })
                    .secondary_message(util::i18n::tr("Keep It Here", "留在这里"))
                    .secondary_on_click(move |_, cx| {
                        if let Some(path) = kept_path.clone() {
                            keep_view.update(cx, |view, _| {
                                view.grouping.kept_out_of.insert(path);
                            });
                        }
                        cx.emit(DismissEvent);
                    })
            })
        });
    });
}

/// The workspace of `destination`, opened if it isn't yet: in the background
/// unless `activate`.
fn destination_workspace(
    multi_workspace: &Entity<MultiWorkspace>,
    destination: &Destination,
    activate: bool,
    window: &mut Window,
    cx: &mut App,
) -> Shared<Task<Option<Entity<Workspace>>>> {
    if let Some(workspace) = destination.workspace.clone() {
        return Task::ready(Some(workspace)).shared();
    }
    let Some(path) = destination.paths.first().cloned() else {
        return Task::ready(None).shared();
    };
    if let Some(task) = cx.default_global::<OpeningWorkspaces>().0.get(&path) {
        return task.clone();
    }
    let path_list = destination.key.as_ref().map_or_else(
        || util::path_list::PathList::new(&destination.paths),
        |key| key.path_list().clone(),
    );
    let open = multi_workspace.update(cx, |multi_workspace, cx| {
        multi_workspace.find_or_create_local_workspace(
            path_list,
            destination.key.clone(),
            None,
            if activate {
                OpenMode::Activate
            } else {
                OpenMode::Add
            },
            None,
            window,
            cx,
        )
    });
    let task = cx
        .spawn({
            let path = path.clone();
            async move |cx| {
                let workspace = open.await.log_err();
                cx.update(|cx| cx.default_global::<OpeningWorkspaces>().0.remove(&path));
                workspace
            }
        })
        .shared();
    cx.default_global::<OpeningWorkspaces>()
        .0
        .insert(path, task.clone());
    task
}

/// Moves the terminal of `view` to the terminal panel of `destination`, and
/// shows it there if `activate`.
fn move_terminal(
    view: Entity<TerminalView>,
    multi_workspace: Entity<MultiWorkspace>,
    destination: Destination,
    activate: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace = destination_workspace(&multi_workspace, &destination, activate, window, cx);
    window
        .spawn(cx, async move |cx| {
            let Some(workspace) = workspace.await else {
                return;
            };
            // A workspace that just opened adds its panels a moment later.
            let started = std::time::Instant::now();
            while workspace
                .read_with(cx, |workspace, cx| workspace.panel::<TerminalPanel>(cx))
                .is_none()
            {
                if started.elapsed() > PANEL_TIMEOUT {
                    log::warn!("no terminal panel to move the terminal to");
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            }
            cx.update(|window, cx| {
                move_to_panel(&view, &workspace, &multi_workspace, activate, window, cx)
            })
            .ok();
        })
        .detach();
}

fn move_to_panel(
    view: &Entity<TerminalView>,
    destination: &Entity<Workspace>,
    multi_workspace: &Entity<MultiWorkspace>,
    activate: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(source_workspace) = view.read(cx).workspace.upgrade() else {
        return;
    };
    if &source_workspace == destination {
        return;
    }
    let Some(panel) = destination.read(cx).panel::<TerminalPanel>(cx) else {
        return;
    };
    // The pane it's in.
    let source = {
        let source_workspace = source_workspace.read(cx);
        let panel_panes = source_workspace
            .panel::<TerminalPanel>(cx)
            .map(|panel| panel.read(cx).center.panes().into_iter().cloned().collect())
            .unwrap_or_else(Vec::new);
        let center_panes = source_workspace.panes().to_vec();
        panel_panes
            .into_iter()
            .chain(center_panes)
            .find(|pane| pane.read(cx).index_for_item(view).is_some())
    };
    // Closed meanwhile.
    let Some(source) = source else {
        return;
    };
    let destination_pane = panel.read(cx).active_pane.clone();

    view.update(cx, |view, cx| view.set_workspace(destination, window, cx));
    let (terminal, focus_handle) = {
        let view = view.read(cx);
        (view.terminal.clone(), view.focus_handle.clone())
    };
    follow_working_directory::watch(view, terminal, focus_handle, destination, window, cx);
    // Its pane goes when it leaves it empty, as when it's closed.
    source.update(cx, |pane, cx| {
        pane.remove_item(view.entity_id(), false, true, window, cx)
    });
    destination_pane.update(cx, |pane, cx| {
        pane.add_item(Box::new(view.clone()), activate, activate, None, window, cx)
    });
    if activate {
        multi_workspace.update(cx, |multi_workspace, cx| {
            multi_workspace.activate(destination.clone(), None, window, cx)
        });
        activate_terminal_view(destination, view, window, cx);
        let focus_handle = view.read(cx).focus_handle.clone();
        window.focus(&focus_handle, cx);
    }
    workspace::agent_activity::notify(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::FakeFs;
    use serde_json::json;
    use util::path;

    fn root(path: &str, repository: Option<&str>) -> Root {
        Root {
            path: PathBuf::from(path),
            repository: repository.map(PathBuf::from),
        }
    }

    #[test]
    fn test_a_folder_belongs_to_the_tab_it_comes_first_in() {
        // Dragon_Art's workspace also holds godot_engine; godot_engine has a
        // tab of its own.
        let mixed = vec![
            root("/repos/dragon_art", Some("/repos/dragon_art")),
            root("/repos/godot", Some("/repos/godot")),
        ];
        let godot = vec![root("/repos/godot", Some("/repos/godot"))];
        let repository = Some(Path::new("/repos/godot"));
        assert_eq!(
            better_fit(
                &mixed,
                std::slice::from_ref(&godot),
                Path::new("/repos/godot/src"),
                repository
            ),
            Some(0)
        );
        // And stays there.
        assert_eq!(
            better_fit(
                &godot,
                std::slice::from_ref(&mixed),
                Path::new("/repos/godot/src"),
                repository
            ),
            None
        );
    }

    #[test]
    fn test_unrelated_roots() {
        let paths = |roots: Vec<Root>| -> Vec<PathBuf> {
            unrelated_roots(&roots)
                .into_iter()
                .map(|root| root.path)
                .collect()
        };
        // Three repositories: the last two are unrelated to the first.
        assert_eq!(
            paths(vec![
                root("/work/player", Some("/work/dnvfx")),
                root("/work/dragon_art", Some("/work/dragon_art")),
                root("/work/godot", Some("/work/godot")),
            ]),
            [
                PathBuf::from("/work/dragon_art"),
                PathBuf::from("/work/godot")
            ]
        );
        // A folder of the same repository, or inside the first: related.
        assert!(
            paths(vec![
                root("/work/solo", Some("/work/solo")),
                root("/work/solo-task", Some("/work/solo")),
                root("/work/solo/docs", Some("/work/solo")),
            ])
            .is_empty()
        );
        assert!(paths(vec![root("/work/solo", None)]).is_empty());
    }

    #[test]
    fn test_better_fit() {
        let solo = vec![root("/repos/solo", Some("/repos/solo"))];
        let godot = vec![root("/repos/godot", Some("/repos/godot"))];
        let solo_crates = vec![root("/repos/solo/crates", Some("/repos/solo"))];
        let repos = vec![root("/repos", None)];
        let empty: Vec<Root> = Vec::new();
        let fits = |current: &[Root], others: &[Vec<Root>], directory: &str, repository| {
            better_fit(current, others, Path::new(directory), repository)
        };
        let solo_repository = Some(Path::new("/repos/solo"));

        // In the current tab's folder.
        assert_eq!(
            fits(
                &solo,
                std::slice::from_ref(&godot),
                "/repos/solo/src",
                solo_repository
            ),
            None
        );
        // In another tab's folder.
        assert_eq!(
            fits(
                &godot,
                std::slice::from_ref(&solo),
                "/repos/solo/src",
                solo_repository
            ),
            Some(0)
        );
        // In another tab's repository, outside its folder.
        assert_eq!(
            fits(
                &godot,
                std::slice::from_ref(&solo_crates),
                "/repos/solo/docs",
                solo_repository
            ),
            Some(0)
        );
        // The current tab has a folder around it, another the repository
        // itself.
        assert_eq!(
            fits(
                &repos,
                std::slice::from_ref(&solo),
                "/repos/solo/src",
                solo_repository
            ),
            Some(0)
        );
        // The current tab is the repository; another has a folder inside it.
        assert_eq!(
            fits(
                &solo,
                std::slice::from_ref(&solo_crates),
                "/repos/solo/docs",
                solo_repository
            ),
            None
        );
        // No tab has it.
        assert_eq!(
            fits(
                &godot,
                std::slice::from_ref(&repos),
                "/elsewhere/x",
                Some(Path::new("/elsewhere/x"))
            ),
            None
        );
        // An empty workspace fits nothing.
        assert_eq!(
            fits(
                &empty,
                &[godot.clone(), solo],
                "/repos/solo",
                solo_repository
            ),
            Some(1)
        );
        // Outside a repository.
        assert_eq!(
            fits(&godot, std::slice::from_ref(&repos), "/repos/notes", None),
            Some(0)
        );
    }

    #[test]
    fn test_normalized() {
        assert_eq!(
            normalized(Path::new("/repos/solo/.git/worktrees/fix/../..")),
            PathBuf::from("/repos/solo/.git")
        );
    }

    #[gpui::test]
    async fn test_repository_root(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/home/user"),
            json!({
                ".git": {},
                "notes": { "today": {} },
                "repos": {
                    "solo": {
                        ".git": { "worktrees": { "fix": { "commondir": "../..\n" } } },
                        "crates": { "terminal": {} },
                    },
                    "tasks": {
                        "fix": {
                            ".git": format!("gitdir: {}\n", path!("/home/user/repos/solo/.git/worktrees/fix")),
                            "src": {},
                        },
                        "broken": { ".git": "gitdir: elsewhere" },
                    },
                },
            }),
        )
        .await;
        let home = Path::new(path!("/home/user"));
        let repository =
            async |directory: &str| repository_root(fs.as_ref(), Path::new(directory), home).await;

        assert_eq!(
            repository(path!("/home/user/repos/solo/crates/terminal")).await,
            Some(PathBuf::from(path!("/home/user/repos/solo")))
        );
        // A linked worktree counts as its main repository.
        assert_eq!(
            repository(path!("/home/user/repos/tasks/fix/src")).await,
            Some(PathBuf::from(path!("/home/user/repos/solo")))
        );
        // Or as itself, when its main repository can't be found.
        assert_eq!(
            repository(path!("/home/user/repos/tasks/broken")).await,
            Some(PathBuf::from(path!("/home/user/repos/tasks/broken")))
        );
        // The dotfiles repository at home doesn't count.
        assert_eq!(repository(path!("/home/user/notes/today")).await, None);
        assert_eq!(repository(path!("/home/user")).await, None);
    }
}
