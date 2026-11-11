//! SOLO: keeps each terminal in the workspace tab of the project it's in.
//!
//! A terminal belongs in the tab with a folder its directory is in, or with a
//! folder in the same git repository; a linked worktree, like an agent task's,
//! counts as its main repository. The tab with the most specific match wins.
//!
//! - Terminals restored from before there were workspace tabs, when one
//!   workspace held all of a window's terminals, move once to their
//!   repository's tab, which opens in the background if it isn't open.
//! - A terminal whose directory (its agent's, when it said) is in a project
//!   another tab has, or in no tab's folder at all, gets a bar on top saying
//!   so, with a button to move it there (opening a tab when there's none);
//!   one in no folder of its tab also gets a mark on its tab. Nothing moves
//!   without a click. A folder in several tabs belongs to the one it's the
//!   first folder of.
//! - A workspace holding unrelated projects (several repositories, say from
//!   when following the terminal added folders) is offered to be split into
//!   a tab each, once; its terminals go with their folders, and those in none
//!   of them where the user picks.
//!
//! Neither happens while `terminal.follow_working_directory` is on: then one
//! workspace follows its terminals instead.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use futures::{FutureExt as _, future::Shared};
use gpui::{
    Action, AnyElement, App, AppContext as _, Context, Entity, EntityId, Global, PathPromptOptions,
    Task, Window, actions,
};
use project::Fs;
use schemars::JsonSchema;
use serde::Deserialize;
use settings::{Settings as _, SettingsStore};
use terminal::{Event as TerminalEvent, terminal_settings::TerminalSettings};
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use util::i18n::tr;
use workspace::{
    MultiWorkspace, OpenMode, ProjectGroupKey, Workspace,
    item::{ItemEvent, TabContextEntry},
};

use crate::{
    TerminalView,
    command_blocks::activate_terminal_view,
    follow_working_directory,
    split_workspace_modal::{Choice, LooseTerminal, SplitWorkspaceModal},
    terminal_panel::TerminalPanel,
};

actions!(
    terminal,
    [
        /// Moves the terminal to the workspace tab of the project it's in,
        /// opening one if there's none.
        MoveToOwnTab,
        /// Moves the terminal to a new workspace tab of a folder to choose.
        MoveToNewTab,
    ]
);

/// Moves the terminal to the workspace tab whose first folder is `path`.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = terminal)]
#[serde(deny_unknown_fields)]
pub struct MoveToTab {
    pub path: PathBuf,
}

/// Set once restored terminals were grouped, the first time Solo ran with
/// workspace tabs.
const GROUPED_KEY: &str = "solo_terminals_grouped";

/// How long to wait for a workspace that just opened to have its terminal
/// panel.
const PANEL_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a terminal looks whether it's in the right tab: its directory,
/// its tab's folders or the other tabs may have changed.
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Prefix of the keys remembering, by a terminal's host, where the user
/// chose to keep it out of; such a terminal outlives Solo.
const KEPT_OUT_OF_KEY: &str = "solo_terminal_kept_out_of:";

/// What a terminal's grouping remembers.
#[derive(Default)]
pub(crate) struct GroupingState {
    /// Where the user chose not to move it to: the first folder of the tab
    /// (comparable), or of the tab it would get.
    kept_out_of: HashSet<PathBuf>,
    /// What it was last checked against.
    checked: Option<CheckKey>,
    /// Where it is when that isn't its tab.
    pub(crate) misplaced: Option<Misplaced>,
    /// The bar about it was closed, for the destination (comparable) it
    /// offered; it shows again next time Solo starts.
    bar_closed_for: Option<PathBuf>,
    /// Keeps checking, while the terminal is open.
    watch: Option<Task<()>>,
}

/// What a terminal's place was checked against.
#[derive(Clone, PartialEq)]
struct CheckKey {
    directory: PathBuf,
    workspace: EntityId,
    folders: Vec<PathBuf>,
    tabs: Vec<Vec<PathBuf>>,
    kept: usize,
}

/// A terminal whose directory belongs in another tab, or in none.
#[derive(Clone, PartialEq)]
pub(crate) struct Misplaced {
    /// Where it is: its agent's directory, else its shell's.
    pub(crate) directory: PathBuf,
    /// The folder it's in, for the user: its repository's, else its own.
    pub(crate) name: String,
    /// Its tab's name.
    pub(crate) tab_name: String,
    destination: Destination,
    /// Whether its directory is in its tab's folders (or the tab has none),
    /// another tab just fits it better.
    in_tab: bool,
    /// The user chose to keep it here.
    pub(crate) kept: bool,
}

impl Misplaced {
    /// The destination, by its first folder, comparable.
    fn destination_path(&self) -> Option<PathBuf> {
        self.destination.paths.first().map(|path| comparable(path))
    }

    /// Whether its tab gets the mark: its directory is in no folder of the
    /// tab, and the user didn't choose to keep it.
    pub(crate) fn marks_tab(&self) -> bool {
        !self.in_tab && !self.kept
    }

    /// The note under its directory, in its tab's card.
    pub(crate) fn card_note(&self) -> Option<String> {
        (!self.in_tab).then(|| not_in_tab_note(&self.tab_name))
    }

    /// The bar's move button.
    pub(crate) fn move_label(&self) -> String {
        if self.destination.key.is_some() {
            if util::i18n::is_chinese() {
                format!("移到 {}", self.destination.name())
            } else {
                format!("Move to {}", self.destination.name())
            }
        } else if util::i18n::is_chinese() {
            format!("在新标签中打开 {} 并移过去", self.name)
        } else {
            format!("Open {} in a New Tab and Move There", self.name)
        }
    }

    /// The first of its terminal tab's menu entries for it.
    pub(crate) fn menu_label(&self) -> String {
        if self.destination.key.is_some() {
            if util::i18n::is_chinese() {
                format!("移到 {} 的标签", self.destination.name())
            } else {
                format!("Move to the {} Tab", self.destination.name())
            }
        } else if util::i18n::is_chinese() {
            format!("移到 {} 的新标签", self.name)
        } else {
            format!("Move to a New {} Tab", self.name)
        }
    }

    /// What the bar says.
    pub(crate) fn message(&self) -> String {
        let directory = self.directory.display();
        if self.in_tab {
            if util::i18n::is_chinese() {
                format!(
                    "这个终端在 {directory}，它有自己的标签 {}。",
                    self.destination.name()
                )
            } else {
                format!(
                    "This terminal is in {directory}, which has a tab of its own: {}.",
                    self.destination.name()
                )
            }
        } else if util::i18n::is_chinese() {
            format!(
                "这个终端在 {directory}，不在 {} 标签的文件夹里。",
                self.tab_name
            )
        } else {
            format!(
                "This terminal is in {directory}, not in a folder of the {} tab.",
                self.tab_name
            )
        }
    }
}

fn not_in_tab_note(tab_name: &str) -> String {
    if util::i18n::is_chinese() {
        format!("不在 {tab_name} 标签的文件夹里")
    } else {
        format!("Not in a folder of the {tab_name} tab")
    }
}

/// The bar's line on what runs in it.
fn carries_on_note(agent: &str) -> String {
    if util::i18n::is_chinese() {
        format!("移过去不会打断正在运行的 {agent}。")
    } else {
        format!("Moving it doesn't interrupt {agent}, which is running.")
    }
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
        view.grouping.watch = Some(cx.spawn_in(window, async move |view, cx| {
            load_kept_out_of(&view, cx).await;
            loop {
                cx.background_executor().timer(CHECK_INTERVAL).await;
                if view
                    .update_in(cx, |view, window, cx| check(view, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    })
    .detach();
}

/// Where the user chose to keep the terminal of `view` out of, saved for
/// its host by [`save_kept_out_of`].
async fn load_kept_out_of(
    view: &gpui::WeakEntity<TerminalView>,
    cx: &mut gpui::AsyncWindowContext,
) {
    let Some((kvp, key)) = view
        .update(cx, |view, cx| {
            let pipe = view.terminal.read(cx).host_pipe()?.to_string();
            cx.has_global::<db::AppDatabase>().then(|| {
                (
                    KeyValueStore::global(cx),
                    format!("{KEPT_OUT_OF_KEY}{pipe}"),
                )
            })
        })
        .ok()
        .flatten()
    else {
        return;
    };
    let saved = cx
        .background_spawn(async move { kvp.read_kvp(&key) })
        .await
        .log_err()
        .flatten();
    let Some(saved) = saved else {
        return;
    };
    view.update(cx, |view, _| {
        view.grouping.kept_out_of.extend(
            saved
                .lines()
                .filter(|line| !line.is_empty())
                .map(PathBuf::from),
        );
    })
    .ok();
}

fn save_kept_out_of(view: &TerminalView, cx: &App) {
    let Some(pipe) = view.terminal.read(cx).host_pipe() else {
        return;
    };
    if !cx.has_global::<db::AppDatabase>() {
        return;
    }
    let kvp = KeyValueStore::global(cx);
    let key = format!("{KEPT_OUT_OF_KEY}{pipe}");
    let value = view
        .grouping
        .kept_out_of
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    cx.background_spawn(async move { kvp.write_kvp(key, value).await.log_err() })
        .detach();
}

/// A folder of a workspace, and the repository it's in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Root {
    pub(crate) path: PathBuf,
    pub(crate) repository: Option<PathBuf>,
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

/// How a terminal in `directory` doesn't fit the tab of `current`.
#[derive(Debug, PartialEq)]
struct Misfit {
    /// Of the `others`, the one it fits better; `None` if none does.
    other: Option<usize>,
    /// Whether it's in a folder of `current` (or in its repository), or
    /// `current` has none.
    in_tab: bool,
}

/// How a terminal in `directory` in `repository` doesn't fit the tab of
/// `current`, with the `others` there are; `None` if it does. A directory
/// as broad as `home` and those above it fit anywhere no other tab fits
/// better.
fn misfit(
    current: &[Root],
    others: &[Vec<Root>],
    directory: &Path,
    repository: Option<&Path>,
    home: &Path,
) -> Option<Misfit> {
    let other = better_fit(current, others, directory, repository);
    let in_tab = current.is_empty() || fit(current, directory, repository).is_some();
    let broad = directory.parent().is_none() || comparable(home).starts_with(comparable(directory));
    (other.is_some() || !(in_tab || broad)).then_some(Misfit { other, in_tab })
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
#[derive(Clone, PartialEq)]
pub(crate) struct Destination {
    /// `None` for a tab to open, of a repository no tab has.
    pub(crate) key: Option<ProjectGroupKey>,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) workspace: Option<Entity<Workspace>>,
}

impl Destination {
    pub(crate) fn name(&self) -> String {
        self.paths
            .first()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// A terminal's tab and the others it could move to.
struct TabContext {
    workspace: Entity<Workspace>,
    multi_workspace: Entity<MultiWorkspace>,
    fs: Arc<dyn Fs>,
    current_paths: Vec<PathBuf>,
    destinations: Vec<Destination>,
}

/// The tab of `view` and the other tabs of its window; `None` for a terminal
/// not in a workspace's own panes, or of a remote project.
fn tab_context(view: &TerminalView, cx: &App) -> Option<TabContext> {
    let workspace = view.workspace.upgrade()?;
    // Only the workspace's own terminals, not one shown elsewhere.
    let view_handle = view.self_handle.upgrade()?;
    if !crate::command_blocks::terminal_views(&workspace, cx).contains(&view_handle) {
        return None;
    }
    let multi_workspace = workspace.read(cx).multi_workspace()?.upgrade()?;
    let project = workspace.read(cx).project().read(cx);
    if !project.is_local() {
        return None;
    }
    let fs = project.fs().clone();
    let current_key = workspace.read(cx).project_group_key(cx);
    let current_paths: Vec<PathBuf> = current_key.path_list().ordered_paths().cloned().collect();
    let destinations = other_tabs(&multi_workspace, &workspace, cx);
    Some(TabContext {
        workspace,
        multi_workspace,
        fs,
        current_paths,
        destinations,
    })
}

/// The tabs of `multi_workspace` other than that of `workspace`, that have
/// a folder.
fn other_tabs(
    multi_workspace: &Entity<MultiWorkspace>,
    workspace: &Entity<Workspace>,
    cx: &App,
) -> Vec<Destination> {
    let current_key = workspace.read(cx).project_group_key(cx);
    let multi_workspace = multi_workspace.read(cx);
    multi_workspace
        .project_groups(cx)
        .into_iter()
        .filter(|group| group.key != current_key && !group.workspaces.contains(workspace))
        .filter(|group| !group.key.path_list().paths().is_empty())
        .map(|group| Destination {
            workspace: multi_workspace
                .last_active_workspace_for_group(&group.key, cx)
                .or_else(|| group.workspaces.first().cloned()),
            paths: group.key.path_list().ordered_paths().cloned().collect(),
            key: Some(group.key),
        })
        .collect()
}

/// Where the terminal of `view` is: where its agent said it works, else
/// where its shell is.
fn placement_directory(view: &TerminalView, cx: &App) -> Option<PathBuf> {
    view.agent_reported_directory()
        .cloned()
        .or_else(|| follow_working_directory::terminal_directory(view.terminal.read(cx)))
}

fn prompt_started(view: &mut TerminalView, window: &mut Window, cx: &mut Context<TerminalView>) {
    if !cx.has_global::<SettingsStore>()
        || TerminalSettings::get_global(cx).follow_working_directory
    {
        return;
    }
    let restored = std::mem::take(&mut view.restored_at_startup);
    let group_restored = restored
        && cx
            .try_global::<GroupRestoredTerminals>()
            .is_some_and(|group| group.0 == Some(true));
    if group_restored {
        group_restored_terminal(view, window, cx);
    } else {
        check(view, window, cx);
    }
}

/// Moves a terminal restored from before there were workspace tabs to the
/// tab of its project, opening one for its repository if there's none.
fn group_restored_terminal(
    view: &mut TerminalView,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    let Some(directory) = follow_working_directory::terminal_directory(view.terminal.read(cx))
    else {
        return;
    };
    let Some(TabContext {
        multi_workspace,
        fs,
        current_paths,
        destinations,
        ..
    }) = tab_context(view, cx)
    else {
        return;
    };
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
            None if fit(&current, &directory, repository.as_deref()).is_none() => {
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
            move_terminal(view, multi_workspace, destination, false, window, cx)
        })
        .ok();
    })
    .detach();
}

/// Looks again whether the terminal of `view` is in the right tab, if its
/// directory or the tabs changed since it last did.
pub(crate) fn check(view: &mut TerminalView, window: &mut Window, cx: &mut Context<TerminalView>) {
    if !cx.has_global::<SettingsStore>() {
        return;
    }
    let context = (!TerminalSettings::get_global(cx).follow_working_directory
        && view.terminal.read(cx).task().is_none())
    .then(|| Some((placement_directory(view, cx)?, tab_context(view, cx)?)))
    .flatten();
    let Some((directory, context)) = context else {
        view.grouping.checked = None;
        set_misplaced(view, None, cx);
        return;
    };
    let key = CheckKey {
        directory: directory.clone(),
        workspace: context.workspace.entity_id(),
        folders: context.current_paths.clone(),
        tabs: context
            .destinations
            .iter()
            .map(|destination| destination.paths.clone())
            .collect(),
        kept: view.grouping.kept_out_of.len(),
    };
    if view.grouping.checked.as_ref() == Some(&key) {
        return;
    }
    view.grouping.checked = Some(key.clone());
    let kept_out_of = view.grouping.kept_out_of.clone();
    cx.spawn_in(window, async move |view, cx| {
        let home = paths::home_dir().clone();
        let misplaced = misplaced(
            context.fs.as_ref(),
            &home,
            &directory,
            context.current_paths,
            &context.destinations,
            &kept_out_of,
        )
        .await;
        view.update(cx, |view, cx| {
            // Unless it changed meanwhile.
            if view.grouping.checked.as_ref() == Some(&key) {
                set_misplaced(view, misplaced, cx);
            }
        })
        .ok();
    })
    .detach();
}

fn set_misplaced(
    view: &mut TerminalView,
    misplaced: Option<Misplaced>,
    cx: &mut Context<TerminalView>,
) {
    if view.grouping.misplaced != misplaced {
        view.grouping.misplaced = misplaced;
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }
}

/// Where a terminal in `directory` is, when that isn't the tab of
/// `current_paths`: in another of the `destinations`, or in none. Kept
/// there when its destination is in `kept_out_of`.
async fn misplaced(
    fs: &dyn Fs,
    home: &Path,
    directory: &Path,
    current_paths: Vec<PathBuf>,
    destinations: &[Destination],
    kept_out_of: &HashSet<PathBuf>,
) -> Option<Misplaced> {
    let repository = repository_root(fs, directory, home).await;
    let tab_name = current_paths
        .first()
        .map(|path| folder_name(path))
        .unwrap_or_else(|| tr("Empty", "空").to_string());
    let current = roots_of(fs, current_paths, home).await;
    let mut others = Vec::with_capacity(destinations.len());
    for destination in destinations {
        others.push(roots_of(fs, destination.paths.clone(), home).await);
    }
    let misfit = misfit(&current, &others, directory, repository.as_deref(), home)?;
    let project = repository.unwrap_or_else(|| directory.to_path_buf());
    let destination = match misfit.other {
        Some(ix) => destinations[ix].clone(),
        None => Destination {
            key: None,
            paths: vec![project.clone()],
            workspace: None,
        },
    };
    let kept = destination
        .paths
        .first()
        .is_some_and(|path| kept_out_of.contains(&comparable(path)));
    Some(Misplaced {
        directory: directory.to_path_buf(),
        name: folder_name(&project),
        tab_name,
        destination,
        in_tab: misfit.in_tab,
        kept,
    })
}

impl TerminalView {
    /// Moves the terminal where it belongs and shows it there; if `all`,
    /// with the others of its tab that belong there too.
    fn move_where_it_belongs(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(misplaced) = self.grouping.misplaced.clone() else {
            return;
        };
        let Some(context) = tab_context(self, cx) else {
            return;
        };
        let Some(this) = self.self_handle.upgrade() else {
            return;
        };
        let target = misplaced.destination_path();
        let others: Vec<Entity<TerminalView>> = if all {
            crate::command_blocks::terminal_views(&context.workspace, cx)
                .into_iter()
                .filter(|other| *other != this)
                .filter(|other| {
                    other
                        .read(cx)
                        .grouping
                        .misplaced
                        .as_ref()
                        .is_some_and(|other| !other.kept && other.destination_path() == target)
                })
                .collect()
        } else {
            Vec::new()
        };
        move_terminal(
            this,
            context.multi_workspace.clone(),
            misplaced.destination.clone(),
            true,
            window,
            cx,
        );
        for other in others {
            move_terminal(
                other,
                context.multi_workspace.clone(),
                misplaced.destination.clone(),
                false,
                window,
                cx,
            );
        }
    }

    /// Keeps the terminal in its tab: it isn't offered to move there again.
    fn keep_here(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .grouping
            .misplaced
            .as_ref()
            .and_then(Misplaced::destination_path)
        else {
            return;
        };
        self.grouping.kept_out_of.insert(path);
        save_kept_out_of(self, cx);
        if let Some(misplaced) = self.grouping.misplaced.as_mut() {
            misplaced.kept = true;
        }
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    fn close_misplaced_bar(&mut self, cx: &mut Context<Self>) {
        self.grouping.bar_closed_for = self
            .grouping
            .misplaced
            .as_ref()
            .and_then(Misplaced::destination_path);
        cx.notify();
    }

    pub(crate) fn move_to_own_tab(
        &mut self,
        _: &MoveToOwnTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_where_it_belongs(false, window, cx);
    }

    pub(crate) fn move_to_tab(
        &mut self,
        action: &MoveToTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(context) = tab_context(self, cx) else {
            return;
        };
        let Some(this) = self.self_handle.upgrade() else {
            return;
        };
        let path = comparable(&action.path);
        let destination = context
            .destinations
            .into_iter()
            .find(|destination| {
                destination
                    .paths
                    .first()
                    .is_some_and(|first| comparable(first) == path)
            })
            .unwrap_or_else(|| Destination {
                key: None,
                paths: vec![action.path.clone()],
                workspace: None,
            });
        move_terminal(this, context.multi_workspace, destination, true, window, cx);
    }

    pub(crate) fn move_to_new_tab(
        &mut self,
        _: &MoveToNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr("Move Here", "移到这里").into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let Some(path) = chosen
                .await
                .ok()
                .and_then(|chosen| chosen.log_err().flatten())
                .and_then(|paths| paths.into_iter().next())
            else {
                return;
            };
            view.update_in(cx, |view, window, cx| {
                view.move_to_tab(&MoveToTab { path }, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// The tab menu's entries to move the terminal: where it belongs, if
    /// that isn't its tab, and to any other tab of its window or a new one.
    pub(crate) fn move_menu_entries(&self, cx: &App) -> Vec<TabContextEntry> {
        let Some(context) = tab_context(self, cx) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        if let Some(misplaced) = &self.grouping.misplaced {
            entries.push(TabContextEntry::Action(
                misplaced.menu_label().into(),
                Box::new(MoveToOwnTab),
            ));
        }
        let mut tabs: Vec<(SharedString, Box<dyn Action>)> = context
            .destinations
            .iter()
            .filter_map(|destination| {
                let path = destination.paths.first()?.clone();
                Some((
                    SharedString::from(destination.name()),
                    Box::new(MoveToTab { path }) as Box<dyn Action>,
                ))
            })
            .collect();
        tabs.push((tr("New Tab…", "新标签…").into(), Box::new(MoveToNewTab)));
        entries.push(TabContextEntry::Submenu(
            tr("Move to Tab", "移到标签").into(),
            tabs,
        ));
        entries
    }

    /// The mark on the tab of a terminal in no folder of its tab: the folder
    /// it's in, its full path on hover.
    pub(crate) fn render_misplaced_mark(&self, cx: &App) -> Option<AnyElement> {
        let misplaced = self
            .grouping
            .misplaced
            .as_ref()
            .filter(|misplaced| misplaced.marks_tab())?;
        let path = misplaced.directory.display().to_string();
        Some(
            h_flex()
                .id("misplaced-terminal-mark")
                .debug_selector(|| "misplaced-terminal-mark".into())
                // A narrow tab cuts its title first, then this, to its icon.
                .flex_shrink(1.)
                .min_w(rems_from_px(18_f32))
                .overflow_hidden()
                .gap_0p5()
                .px_1()
                .rounded_sm()
                .border_1()
                .border_dashed()
                .border_color(cx.theme().colors().border)
                .child(
                    Icon::new(IconName::Folder)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    div().min_w_0().child(
                        Label::new(misplaced.name.clone())
                            .untranslated()
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .single_line()
                            .truncate(),
                    ),
                )
                .tooltip(Tooltip::text(path))
                .into_any_element(),
        )
    }

    /// The bar on top of a terminal not in its tab, unless the user chose
    /// to keep it there or closed the bar.
    pub(crate) fn render_misplaced_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let misplaced = self
            .grouping
            .misplaced
            .as_ref()
            .filter(|misplaced| !misplaced.kept)?;
        if self.grouping.bar_closed_for.is_some()
            && self.grouping.bar_closed_for == misplaced.destination_path()
        {
            return None;
        }
        let agent = self
            .agent_status()
            .filter(|(_, status)| status.is_running())
            .map(|(name, _)| name.to_string());
        Some(
            h_flex()
                .id("misplaced-terminal-bar")
                .debug_selector(|| "misplaced-terminal-bar".into())
                .w_full()
                .flex_none()
                .gap_2()
                .px_2()
                .py_1()
                .bg(cx.theme().colors().editor_subheader_background)
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Icon::new(IconName::Folder)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .child(
                            Label::new(misplaced.message())
                                .untranslated()
                                .size(LabelSize::Small)
                                .truncate(),
                        )
                        .when_some(agent, |this, agent| {
                            this.child(
                                Label::new(carries_on_note(&agent))
                                    .untranslated()
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        }),
                )
                .child(
                    Button::new("move-misplaced-terminal", misplaced.move_label())
                        .style(ButtonStyle::Filled)
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.move_where_it_belongs(true, window, cx)
                        })),
                )
                .child(
                    Button::new("keep-misplaced-terminal", tr("Keep It Here", "留在这里"))
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|view, _, _, cx| view.keep_here(cx))),
                )
                .child(
                    IconButton::new("close-misplaced-terminal-bar", IconName::Close)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(tr(
                            "Close for now; asks again next time Solo starts",
                            "这次先关掉，下次启动 Solo 再提示",
                        )))
                        .on_click(cx.listener(|view, _, _, cx| view.close_misplaced_bar(cx))),
                )
                .into_any_element(),
        )
    }
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
        let Some(workspace_entity) = workspace.upgrade() else {
            return;
        };
        let loose = loose_terminals(&workspace_entity, fs.as_ref(), &home, &roots, cx).await;
        workspace
            .update_in(cx, |workspace, window, cx| {
                // Not over a dialog the user has open; it's offered again
                // next time.
                if workspace.has_active_modal(window, cx) {
                    return;
                }
                let weak = cx.entity().downgrade();
                workspace.toggle_modal(window, cx, |_, cx| {
                    SplitWorkspaceModal::new(weak, roots, unrelated, loose, key, cx)
                });
            })
            .ok();
    })
    .detach();
}

/// The terminals of `workspace` whose directory is in none of its folders,
/// `roots`, nor so broad as the home directory.
async fn loose_terminals(
    workspace: &Entity<Workspace>,
    fs: &dyn Fs,
    home: &Path,
    roots: &[Root],
    cx: &mut gpui::AsyncWindowContext,
) -> Vec<LooseTerminal> {
    let Some((terminals, destinations)) = cx
        .update(|_, cx| {
            let terminals: Vec<_> = crate::command_blocks::terminal_views(workspace, cx)
                .into_iter()
                .filter(|view| view.read(cx).terminal.read(cx).task().is_none())
                .filter_map(|view| {
                    let directory = placement_directory(view.read(cx), cx)?;
                    let view_ref = view.read(cx);
                    let card = view_ref.tab_card(cx);
                    let title = card
                        .as_ref()
                        .and_then(|card| card.agent.as_ref()?.session_title.clone())
                        .or_else(|| card.map(|card| card.title))
                        .unwrap_or_default();
                    let agent = view_ref
                        .agent_status()
                        .map(|(name, status)| (name.to_string(), status.is_running()));
                    Some((view.downgrade(), title, agent, directory))
                })
                .collect();
            let destinations = workspace
                .read(cx)
                .multi_workspace()
                .and_then(|multi_workspace| multi_workspace.upgrade())
                .map(|multi_workspace| other_tabs(&multi_workspace, workspace, cx))
                .unwrap_or_default();
            (terminals, destinations)
        })
        .ok()
    else {
        return Vec::new();
    };
    let mut others = Vec::with_capacity(destinations.len());
    for destination in &destinations {
        others.push(roots_of(fs, destination.paths.clone(), home).await);
    }
    let mut loose = Vec::new();
    for (view, title, agent, directory) in terminals {
        let repository = repository_root(fs, &directory, home).await;
        let Some(misfit) = misfit(roots, &others, &directory, repository.as_deref(), home)
            .filter(|misfit| !misfit.in_tab)
        else {
            continue;
        };
        let project = repository.unwrap_or_else(|| directory.clone());
        let own = match misfit.other {
            Some(ix) => destinations[ix].clone(),
            None => Destination {
                key: None,
                paths: vec![project.clone()],
                workspace: None,
            },
        };
        loose.push(LooseTerminal {
            view,
            title,
            agent,
            directory,
            own,
            name: folder_name(&project),
        });
    }
    loose
}

pub(crate) fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Remembers, by `key`, the user chose to keep a workspace's folders
/// together.
pub(crate) fn keep_together(key: String, cx: &App) {
    if !cx.has_global::<db::AppDatabase>() {
        return;
    }
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        kvp.write_kvp(key, "1".to_string()).await.log_err();
    })
    .detach();
}

/// Splits `unrelated` off `workspace`: each becomes a tab of its own (or
/// joins the tab it's the first folder of), with the terminals in it. The
/// `loose` terminals, in none of its folders, go as chosen.
pub(crate) fn split_workspace_now(
    workspace: &Entity<Workspace>,
    unrelated: &[Root],
    loose: Vec<(Entity<TerminalView>, Destination, Choice)>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(multi_workspace) = workspace
        .read(cx)
        .multi_workspace()
        .and_then(|multi_workspace| multi_workspace.upgrade())
    else {
        return;
    };
    let views = crate::command_blocks::terminal_views(workspace, cx);
    let groups = multi_workspace.read(cx).project_groups(cx);
    let mut split_destinations = Vec::with_capacity(unrelated.len());
    for root in unrelated {
        let path = comparable(&root.path);
        // The tab it's the first folder of, if there is one.
        let existing = groups.iter().find(|group| {
            !group.workspaces.contains(workspace)
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
        split_destinations.push(destination.clone());
        // Its terminals: those whose directory is in it.
        let moving: Vec<Entity<TerminalView>> = views
            .iter()
            .filter(|view| {
                follow_working_directory::terminal_directory(view.read(cx).terminal.read(cx))
                    .is_some_and(|directory| comparable(&directory).starts_with(&path))
            })
            .cloned()
            .collect();
        let goes_to_it = loose
            .iter()
            .any(|(_, _, choice)| *choice == Choice::SplitOff(split_destinations.len() - 1));
        if moving.is_empty() && !goes_to_it {
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
    for (view, own, choice) in loose {
        let destination = match choice {
            Choice::Own => own,
            Choice::Stay => continue,
            Choice::SplitOff(ix) => match split_destinations.get(ix) {
                Some(destination) => destination.clone(),
                None => continue,
            },
        };
        move_terminal(
            view,
            multi_workspace.clone(),
            destination,
            false,
            window,
            cx,
        );
    }
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

    /// Workspace 2 on 2026-11-10: only godot_engine, with terminals in
    /// DragonNest, which no tab has.
    #[test]
    fn test_misfit() {
        let home = Path::new("/users/admin");
        let godot = vec![root(
            "/work/Godot/godot_engine",
            Some("/work/Godot/godot_engine"),
        )];
        let others = vec![
            vec![root("/work/TCFrameWork", Some("/work/TCFrameWork"))],
            vec![root("/work/solo", Some("/work/solo"))],
        ];
        let dragon_nest = Some(Path::new("/work/DragonNest"));
        let misfit = |current: &[Root], others: &[Vec<Root>], directory: &str, repository| {
            super::misfit(current, others, Path::new(directory), repository, home)
        };

        // In no tab: marked, a tab of its own offered.
        assert_eq!(
            misfit(&godot, &others, "/work/DragonNest", dragon_nest),
            Some(Misfit {
                other: None,
                in_tab: false
            })
        );
        assert_eq!(
            misfit(&godot, &others, "/work/DragonNest/Client", dragon_nest),
            Some(Misfit {
                other: None,
                in_tab: false
            })
        );
        // In the tab: fine.
        assert_eq!(
            misfit(
                &godot,
                &others,
                "/work/Godot/godot_engine/scene",
                Some(Path::new("/work/Godot/godot_engine"))
            ),
            None
        );
        // Once DragonNest has a tab: that one.
        let mut with_dragon_nest = others.clone();
        with_dragon_nest.push(vec![root("/work/DragonNest", Some("/work/DragonNest"))]);
        assert_eq!(
            misfit(&godot, &with_dragon_nest, "/work/DragonNest", dragon_nest),
            Some(Misfit {
                other: Some(2),
                in_tab: false
            })
        );
        // In the tab, but another fits it better: no mark, but offered.
        let solo_crates = vec![root("/work/solo/crates", Some("/work/solo"))];
        assert_eq!(
            misfit(
                &[root("/work", None)],
                std::slice::from_ref(&solo_crates),
                "/work/solo/crates/terminal",
                Some(Path::new("/work/solo"))
            ),
            Some(Misfit {
                other: Some(0),
                in_tab: true
            })
        );
        // The home directory and above fit anywhere.
        assert_eq!(misfit(&godot, &others, "/users/admin", None), None);
        assert_eq!(misfit(&godot, &others, "/users", None), None);
        assert_eq!(misfit(&godot, &others, "/", None), None);
        // A tab without folders: nothing to mark.
        assert_eq!(misfit(&[], &others, "/work/DragonNest", dragon_nest), None);
        // The folders being split, for the split dialog: DragonNest is in
        // none of them.
        let mixed = vec![
            root("/work/player", Some("/work/dnvfx")),
            root("/work/Dragon_Art", Some("/work/Dragon_Art")),
            root("/work/Godot/godot_engine", Some("/work/Godot/godot_engine")),
        ];
        assert_eq!(
            misfit(&mixed, &[], "/work/DragonNest", dragon_nest),
            Some(Misfit {
                other: None,
                in_tab: false
            })
        );
        assert_eq!(
            misfit(
                &mixed,
                &[],
                "/work/Dragon_Art/Textures",
                Some(Path::new("/work/Dragon_Art"))
            ),
            None
        );
    }

    /// The paths as they were on the user's machine.
    #[cfg(windows)]
    #[test]
    fn test_misfit_windows_paths() {
        let home = Path::new(r"C:\Users\Admin");
        let godot = vec![root(
            r"G:\GitWorkProject\Godot\godot_engine",
            Some(r"G:\GitWorkProject\Godot\godot_engine"),
        )];
        let others = vec![vec![root(r"G:\GitWorkProject\DragonNest", None)]];
        // Case doesn't matter.
        assert_eq!(
            misfit(
                &godot,
                &others,
                Path::new(r"g:\gitworkproject\dragonnest\Client"),
                None,
                home
            ),
            Some(Misfit {
                other: Some(0),
                in_tab: false
            })
        );
        // The other DragonNest, under 龙之谷, is another directory: no tab
        // has it.
        assert_eq!(
            misfit(
                &godot,
                &others,
                Path::new(r"G:\GitWorkProject\龙之谷\DragonNest"),
                None,
                home
            ),
            Some(Misfit {
                other: None,
                in_tab: false
            })
        );
        assert_eq!(
            misfit(&godot, &others, Path::new(r"C:\Users\Admin"), None, home),
            None
        );
        assert_eq!(misfit(&godot, &others, Path::new(r"G:\"), None, home), None);
    }

    #[gpui::test]
    async fn test_misplaced(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work"),
            json!({
                "Godot": { "godot_engine": { ".git": {}, "scene": {} } },
                "DragonNest": { ".git": {}, "Client": {} },
                "龙之谷": { "DragonNest": { ".git": {} } },
                "TCFrameWork": { ".git": {} },
                "solo": { ".git": {} },
            }),
        )
        .await;
        let home = Path::new(path!("/users/admin"));
        let godot_engine = PathBuf::from(path!("/work/Godot/godot_engine"));
        let dragon_nest = PathBuf::from(path!("/work/DragonNest"));
        let tab = |path: &str| Destination {
            key: None,
            paths: vec![PathBuf::from(path)],
            workspace: None,
        };
        let others = vec![tab(path!("/work/TCFrameWork")), tab(path!("/work/solo"))];
        let mut kept = HashSet::default();
        let misplaced = async |directory: &str, kept: &HashSet<PathBuf>| {
            super::misplaced(
                fs.as_ref(),
                home,
                Path::new(directory),
                vec![godot_engine.clone()],
                &others,
                kept,
            )
            .await
        };

        // The claude terminal in DragonNest, below its repository.
        let claude = misplaced(path!("/work/DragonNest/Client"), &kept)
            .await
            .expect("not in the godot_engine tab");
        assert_eq!(claude.name, "DragonNest");
        assert_eq!(claude.tab_name, "godot_engine");
        assert_eq!(claude.destination.paths, std::slice::from_ref(&dragon_nest));
        assert!(claude.destination.key.is_none());
        assert!(claude.marks_tab());
        assert_eq!(
            claude.card_note().as_deref(),
            Some(tr(
                "Not in a folder of the godot_engine tab",
                "不在 godot_engine 标签的文件夹里"
            ))
        );
        assert_eq!(
            claude.move_label(),
            tr(
                "Open DragonNest in a New Tab and Move There",
                "在新标签中打开 DragonNest 并移过去"
            )
        );
        assert_eq!(
            claude.menu_label(),
            tr("Move to a New DragonNest Tab", "移到 DragonNest 的新标签")
        );
        let directory = Path::new(path!("/work/DragonNest/Client")).display();
        assert_eq!(
            claude.message(),
            if util::i18n::is_chinese() {
                format!("这个终端在 {directory}，不在 godot_engine 标签的文件夹里。")
            } else {
                format!("This terminal is in {directory}, not in a folder of the godot_engine tab.")
            }
        );
        // The shell next to it goes to the same tab.
        let shell = misplaced(path!("/work/DragonNest"), &kept).await.unwrap();
        assert_eq!(shell.destination_path(), claude.destination_path());

        // The godot_engine terminals stay.
        assert!(
            misplaced(path!("/work/Godot/godot_engine/scene"), &kept)
                .await
                .is_none()
        );
        assert!(
            misplaced(path!("/work/Godot/godot_engine"), &kept)
                .await
                .is_none()
        );

        // The DragonNest under 龙之谷 is another project.
        let other_dragon_nest = misplaced(path!("/work/龙之谷/DragonNest"), &kept)
            .await
            .unwrap();
        assert_eq!(other_dragon_nest.name, "DragonNest");
        assert_ne!(
            other_dragon_nest.destination_path(),
            claude.destination_path()
        );

        // Kept here: no mark, but the card still says where it is.
        kept.insert(comparable(&dragon_nest));
        let kept_claude = misplaced(path!("/work/DragonNest/Client"), &kept)
            .await
            .unwrap();
        assert!(kept_claude.kept && !kept_claude.marks_tab());
        assert!(kept_claude.card_note().is_some());
        assert!(
            !misplaced(path!("/work/龙之谷/DragonNest"), &kept)
                .await
                .unwrap()
                .kept
        );
    }

    #[test]
    fn test_carries_on_note() {
        assert_eq!(
            carries_on_note("claude"),
            tr(
                "Moving it doesn't interrupt claude, which is running.",
                "移过去不会打断正在运行的 claude。"
            )
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
