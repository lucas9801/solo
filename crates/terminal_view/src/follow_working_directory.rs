//! SOLO: narrows the project panel, file finder and project search to the
//! project the active terminal is in, see `terminal.follow_working_directory`.
//!
//! The project root is the git repository around the terminal's working
//! directory, or the directory itself outside one. It is re-evaluated when
//! another terminal becomes active, and when the active terminal's shell shows
//! its prompt in a new directory: the shell only reports its directory (OSC 7)
//! right before the prompt (OSC 133 A), so a `cd` inside a running script
//! never counts.
//!
//! Roots outside the window's own folders become invisible worktrees, so they
//! don't change what the window is saved and reopened as. They're kept for a
//! while after the terminal leaves them, so switching back and forth doesn't
//! restart their language servers.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use collections::HashMap;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, FocusHandle, Global, Subscription, Task,
    WeakEntity, Window,
};
use project::{Fs, Project, Worktree, WorktreeId};
use settings::{Settings as _, SettingsStore};
use terminal::{Event as TerminalEvent, Terminal, terminal_settings::TerminalSettings};
use util::ResultExt as _;
use workspace::Workspace;

use crate::{TerminalView, terminal_panel::TerminalPanel};

/// How long a project the terminal left keeps its worktree (and language
/// servers).
const RETAIN_UNFOCUSED_WORKTREE: Duration = Duration::from_secs(30 * 60);
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|terminal_view: &mut TerminalView, window, cx| {
        let Some(window) = window else {
            return;
        };
        let Some(workspace) = terminal_view.workspace.upgrade() else {
            return;
        };
        let follower = WorkingDirectoryFollower::for_workspace(&workspace, cx);
        let view = cx.entity();
        let terminal = terminal_view.terminal.clone();
        let focus_handle = terminal_view.focus_handle.clone();
        follower.update(cx, |follower, cx| {
            follower.watch(&view, terminal, &focus_handle, window, cx)
        });
    })
    .detach();
}

#[derive(Default)]
struct Followers(HashMap<EntityId, Entity<WorkingDirectoryFollower>>);

impl Global for Followers {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    /// The terminal got focus.
    Activated,
    /// The terminal's shell showed its prompt.
    PromptStarted,
}

/// The directory to narrow the project to after `trigger`, if any.
/// `was_active` tells whether the terminal already was the active one, and
/// `followed` is the directory last followed.
fn directory_to_follow(
    trigger: Trigger,
    was_active: bool,
    followed: Option<&Path>,
    terminal_directory: Option<&Path>,
) -> Option<PathBuf> {
    let directory = terminal_directory?;
    let unchanged = followed == Some(directory);
    let follow = match trigger {
        // Another terminal may have moved the project elsewhere meanwhile.
        Trigger::Activated => !(was_active && unchanged),
        Trigger::PromptStarted => was_active && !unchanged,
    };
    follow.then(|| directory.to_path_buf())
}

/// The directory the shell last reported at its prompt; the process's own
/// directory only for shells that don't report one.
fn terminal_directory(terminal: &Terminal) -> Option<PathBuf> {
    terminal
        .shell_integration()
        .working_directory()
        .cloned()
        .or_else(|| terminal.working_directory())
}

/// The project root of `directory`: the git repository it's in, or the
/// directory itself. `None` for the home directory and filesystem roots,
/// which are too big to load as a project (a repository at home, as some keep
/// their dotfiles, doesn't count either).
async fn project_root(fs: &dyn Fs, directory: &Path, home: &Path) -> Option<PathBuf> {
    let too_broad = |path: &Path| path == home || path.parent().is_none();
    for ancestor in directory.ancestors() {
        if too_broad(ancestor) {
            break;
        }
        if fs
            .metadata(&ancestor.join(".git"))
            .await
            .log_err()
            .flatten()
            .is_some()
        {
            return Some(ancestor.to_path_buf());
        }
    }
    (!too_broad(directory)).then(|| directory.to_path_buf())
}

/// The worktrees, of those `last_focused` lists, that have been out of focus
/// for `retain` by `now`.
fn expired_worktrees(
    last_focused: impl IntoIterator<Item = (WorktreeId, Instant)>,
    focused: Option<WorktreeId>,
    now: Instant,
    retain: Duration,
) -> Vec<WorktreeId> {
    last_focused
        .into_iter()
        .filter(|(worktree_id, last_focused)| {
            Some(*worktree_id) != focused && now.saturating_duration_since(*last_focused) >= retain
        })
        .map(|(worktree_id, _)| worktree_id)
        .collect()
}

struct RetainedWorktree {
    /// Invisible worktrees are dropped by the project as soon as nothing
    /// holds them.
    worktree: Entity<Worktree>,
    last_focused: Instant,
}

pub(crate) struct WorkingDirectoryFollower {
    project: WeakEntity<Project>,
    workspace: WeakEntity<Workspace>,
    active_terminal: Option<EntityId>,
    followed_directory: Option<PathBuf>,
    /// Worktrees this follower created.
    retained: HashMap<WorktreeId, RetainedWorktree>,
    watched_views: HashMap<EntityId, Vec<Subscription>>,
    enabled: bool,
    pending_follow: Task<()>,
    _prune: Task<()>,
    _settings_subscription: Subscription,
}

impl WorkingDirectoryFollower {
    fn for_workspace(workspace: &Entity<Workspace>, cx: &mut App) -> Entity<Self> {
        let workspace_id = workspace.entity_id();
        if let Some(follower) = cx.default_global::<Followers>().0.get(&workspace_id) {
            return follower.clone();
        }
        let project = workspace.read(cx).project().downgrade();
        let weak_workspace = workspace.downgrade();
        let follower = cx.new(|cx| Self::new(project, weak_workspace, cx));
        cx.default_global::<Followers>()
            .0
            .insert(workspace_id, follower.clone());
        cx.observe_release(workspace, move |_, cx| {
            cx.default_global::<Followers>().0.remove(&workspace_id);
        })
        .detach();
        follower
    }

    fn new(
        project: WeakEntity<Project>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let prune = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PRUNE_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        this.prune(Instant::now(), RETAIN_UNFOCUSED_WORKTREE, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            project,
            workspace,
            active_terminal: None,
            followed_directory: None,
            retained: HashMap::default(),
            watched_views: HashMap::default(),
            enabled: Self::enabled_in_settings(cx),
            pending_follow: Task::ready(()),
            _prune: prune,
            _settings_subscription: cx.observe_global::<SettingsStore>(Self::settings_changed),
        }
    }

    fn enabled_in_settings(cx: &App) -> bool {
        cx.has_global::<SettingsStore>()
            && TerminalSettings::get_global(cx).follow_working_directory
    }

    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let enabled = Self::enabled_in_settings(cx);
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        // Follow again from scratch once re-enabled.
        self.active_terminal = None;
        self.followed_directory = None;
        if !enabled {
            self.pending_follow = Task::ready(());
            self.set_focus(None, cx);
        }
    }

    fn watch(
        &mut self,
        view: &Entity<TerminalView>,
        terminal: Entity<Terminal>,
        focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view_id = view.entity_id();
        let subscriptions = vec![
            cx.on_focus_in(focus_handle, window, {
                let terminal = terminal.downgrade();
                move |this, _window, cx| {
                    if let Some(terminal) = terminal.upgrade()
                        && this.is_workspace_terminal(view_id, cx)
                    {
                        this.terminal_triggered(&terminal, Trigger::Activated, cx);
                    }
                }
            }),
            cx.subscribe(&terminal, |this, terminal, event: &TerminalEvent, cx| {
                if *event == TerminalEvent::PromptStarted {
                    this.terminal_triggered(&terminal, Trigger::PromptStarted, cx);
                }
            }),
            cx.observe_release(view, move |this, _, _| {
                this.watched_views.remove(&view_id);
            }),
        ];
        self.watched_views.insert(view_id, subscriptions);
    }

    /// Terminals elsewhere, like an agent's, don't move the project.
    fn is_workspace_terminal(&self, view_id: EntityId, cx: &App) -> bool {
        let Some(workspace) = self.workspace.upgrade() else {
            return false;
        };
        let workspace = workspace.read(cx);
        let terminal_panel = workspace.panel::<TerminalPanel>(cx);
        let terminal_panes = terminal_panel
            .as_ref()
            .map(|panel| panel.read(cx).panes())
            .unwrap_or_default();
        workspace
            .panes()
            .iter()
            .chain(terminal_panes)
            .any(|pane| pane.read(cx).items().any(|item| item.item_id() == view_id))
    }

    fn terminal_triggered(
        &mut self,
        terminal: &Entity<Terminal>,
        trigger: Trigger,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled {
            return;
        }
        let terminal_id = terminal.entity_id();
        let was_active = self.active_terminal == Some(terminal_id);
        if trigger == Trigger::Activated {
            self.active_terminal = Some(terminal_id);
        }
        let directory = terminal_directory(terminal.read(cx));
        if let Some(directory) = directory_to_follow(
            trigger,
            was_active,
            self.followed_directory.as_deref(),
            directory.as_deref(),
        ) {
            self.follow(directory, cx);
        }
    }

    /// Narrows the project to the project root of `directory`, adding it as a
    /// worktree if needed.
    fn follow(&mut self, directory: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        if !project.read(cx).is_local() {
            return;
        }
        self.followed_directory = Some(directory.clone());
        let fs = project.read(cx).fs().clone();
        let home = paths::home_dir().clone();
        // Replacing the task cancels a follow still in flight, so a slower
        // earlier one can't win.
        self.pending_follow = cx.spawn(async move |this, cx| {
            let worktree = match project_root(fs.as_ref(), &directory, &home).await {
                Some(root) => {
                    let worktree = project
                        .update(cx, |project, cx| {
                            project.find_or_create_worktree(&root, false, cx)
                        })
                        .await;
                    match worktree {
                        Ok((worktree, _)) => Some(worktree),
                        Err(error) => {
                            log::error!("failed to open {root:?} for the terminal: {error:#}");
                            return;
                        }
                    }
                }
                None => None,
            };
            this.update(cx, |this, cx| this.set_focus(worktree, cx))
                .log_err();
        });
    }

    fn set_focus(&mut self, worktree: Option<Entity<Worktree>>, cx: &mut Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let now = Instant::now();
        if let Some(previous) = project.read(cx).focused_worktree(cx)
            && let Some(retained) = self.retained.get_mut(&previous)
        {
            retained.last_focused = now;
        }
        let worktree_id = worktree.as_ref().map(|worktree| worktree.read(cx).id());
        if let Some(worktree) = worktree
            && !worktree.read(cx).is_visible()
        {
            self.retained.insert(
                worktree.read(cx).id(),
                RetainedWorktree {
                    worktree,
                    last_focused: now,
                },
            );
        }
        project.update(cx, |project, cx| {
            project.set_focused_worktree(worktree_id, cx)
        });
    }

    /// Removes the worktrees this follower created that have been out of
    /// focus for `retain`, unless a file in them is still open.
    fn prune(&mut self, now: Instant, retain: Duration, cx: &mut Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let focused = project.read(cx).focused_worktree(cx);
        let expired = expired_worktrees(
            self.retained
                .iter()
                .map(|(worktree_id, retained)| (*worktree_id, retained.last_focused)),
            focused,
            now,
            retain,
        );
        if expired.is_empty() {
            return;
        }
        let open_in: Vec<WorktreeId> = project
            .read(cx)
            .opened_buffers(cx)
            .iter()
            .filter_map(|buffer| Some(buffer.read(cx).file()?.worktree_id(cx)))
            .collect();
        for worktree_id in expired {
            if open_in.contains(&worktree_id) {
                continue;
            }
            if let Some(retained) = self.retained.remove(&worktree_id)
                && !retained.worktree.read(cx).is_visible()
            {
                project.update(cx, |project, cx| project.remove_worktree(worktree_id, cx));
            }
        }
    }

    /// Drops the worktrees this follower created in `directory` right away,
    /// for when the directory is about to be deleted.
    fn release_directory(&mut self, directory: &Path, cx: &mut Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let released: Vec<WorktreeId> = self
            .retained
            .iter()
            .filter(|(_, retained)| {
                crate::agent_tasks::is_within(&retained.worktree.read(cx).abs_path(), directory)
            })
            .map(|(worktree_id, _)| *worktree_id)
            .collect();
        if released.is_empty() {
            return;
        }
        if project
            .read(cx)
            .focused_worktree(cx)
            .is_some_and(|focused| released.contains(&focused))
        {
            self.followed_directory = None;
            project.update(cx, |project, cx| project.set_focused_worktree(None, cx));
        }
        for worktree_id in released {
            if let Some(retained) = self.retained.remove(&worktree_id)
                && !retained.worktree.read(cx).is_visible()
            {
                project.update(cx, |project, cx| project.remove_worktree(worktree_id, cx));
            }
        }
    }
}

/// SOLO: lets go of the worktrees the project panel followed a terminal into
/// inside `directory`, which is about to be deleted.
pub(crate) fn release_directory(workspace: &Entity<Workspace>, directory: &Path, cx: &mut App) {
    let follower = cx
        .try_global::<Followers>()
        .and_then(|followers| followers.0.get(&workspace.entity_id()).cloned());
    if let Some(follower) = follower {
        follower.update(cx, |follower, cx| follower.release_directory(directory, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, UpdateGlobal as _};
    use project::FakeFs;
    use serde_json::json;
    use util::path;

    fn worktree_paths(project: &Entity<Project>, cx: &mut TestAppContext) -> Vec<PathBuf> {
        project.read_with(cx, |project, cx| {
            project
                .worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect()
        })
    }

    fn scoped_paths(project: &Entity<Project>, cx: &mut TestAppContext) -> Vec<PathBuf> {
        project.read_with(cx, |project, cx| {
            project
                .scoped_worktrees(cx)
                .iter()
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect()
        })
    }

    #[test]
    fn test_directory_to_follow() {
        let solo = Path::new("/repos/solo");
        let godot = Path::new("/repos/godot");

        // Switching terminals follows the new one, even to the same directory.
        assert_eq!(
            directory_to_follow(Trigger::Activated, false, Some(solo), Some(solo)),
            Some(solo.to_path_buf())
        );
        assert_eq!(
            directory_to_follow(Trigger::Activated, false, Some(solo), Some(godot)),
            Some(godot.to_path_buf())
        );
        // Focusing the active terminal again changes nothing.
        assert_eq!(
            directory_to_follow(Trigger::Activated, true, Some(solo), Some(solo)),
            None
        );
        // A prompt in a new directory moves the project, the same one doesn't.
        assert_eq!(
            directory_to_follow(Trigger::PromptStarted, true, Some(solo), Some(godot)),
            Some(godot.to_path_buf())
        );
        assert_eq!(
            directory_to_follow(Trigger::PromptStarted, true, Some(solo), Some(solo)),
            None
        );
        // Background terminals' prompts don't count.
        assert_eq!(
            directory_to_follow(Trigger::PromptStarted, false, Some(solo), Some(godot)),
            None
        );
        // Nothing to follow without a directory.
        assert_eq!(
            directory_to_follow(Trigger::Activated, false, Some(solo), None),
            None
        );
        assert_eq!(
            directory_to_follow(Trigger::PromptStarted, true, None, Some(solo)),
            Some(solo.to_path_buf())
        );
    }

    #[test]
    fn test_expired_worktrees() {
        let now = Instant::now();
        let retain = Duration::from_secs(60);
        let first = WorktreeId::from_usize(1);
        let second = WorktreeId::from_usize(2);
        let third = WorktreeId::from_usize(3);
        let last_focused = [
            (first, now - Duration::from_secs(120)),
            (second, now - Duration::from_secs(10)),
            (third, now - Duration::from_secs(120)),
        ];
        assert_eq!(
            expired_worktrees(last_focused, Some(third), now, retain),
            vec![first]
        );
        assert_eq!(
            expired_worktrees(last_focused, None, now + retain, retain).len(),
            3
        );
    }

    #[gpui::test]
    async fn test_project_root(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/home/user"),
            json!({
                ".git": {},
                "notes": { "today": {} },
                "repos": {
                    "solo": { ".git": {}, "crates": { "terminal": {} } },
                    "worktree": { ".git": "gitdir: elsewhere" },
                    "plain": { "inner": {} },
                },
            }),
        )
        .await;
        let home = Path::new(path!("/home/user"));
        let root =
            async |directory: &str| project_root(fs.as_ref(), Path::new(directory), home).await;

        assert_eq!(
            root(path!("/home/user/repos/solo/crates/terminal")).await,
            Some(PathBuf::from(path!("/home/user/repos/solo")))
        );
        assert_eq!(
            root(path!("/home/user/repos/solo")).await,
            Some(PathBuf::from(path!("/home/user/repos/solo")))
        );
        // A linked git worktree has a `.git` file.
        assert_eq!(
            root(path!("/home/user/repos/worktree")).await,
            Some(PathBuf::from(path!("/home/user/repos/worktree")))
        );
        // Outside a repository, the directory itself; the dotfiles repository
        // at home doesn't make all of home the project.
        assert_eq!(
            root(path!("/home/user/repos/plain/inner")).await,
            Some(PathBuf::from(path!("/home/user/repos/plain/inner")))
        );
        assert_eq!(
            root(path!("/home/user/notes/today")).await,
            Some(PathBuf::from(path!("/home/user/notes/today")))
        );
        assert_eq!(root(path!("/home/user")).await, None);
        assert_eq!(root(path!("/")).await, None);
    }

    #[gpui::test]
    async fn test_following_switches_worktrees(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            // SOLO: off by default, now that each workspace has a tab.
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .terminal
                        .get_or_insert_default()
                        .follow_working_directory = Some(true);
                });
            });
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/repos"),
            json!({
                "solo": { ".git": {}, "crates": { "a.rs": "" } },
                "godot": { ".git": {}, "core": { "b.cpp": "" } },
                "notes": { "today.md": "" },
            }),
        )
        .await;
        let project = Project::test(fs, [Path::new(path!("/repos/solo"))], cx).await;
        let follower = cx.new(|cx| {
            WorkingDirectoryFollower::new(project.downgrade(), WeakEntity::new_invalid(), cx)
        });
        let follow = |directory: &str, cx: &mut TestAppContext| {
            follower.update(cx, |follower, cx| {
                follower.follow(PathBuf::from(directory), cx)
            });
            cx.run_until_parked();
        };
        let solo = PathBuf::from(path!("/repos/solo"));
        let godot = PathBuf::from(path!("/repos/godot"));

        // Without a terminal, everything the window opened is in scope.
        assert_eq!(scoped_paths(&project, cx), vec![solo.clone()]);

        follow(path!("/repos/godot/core"), cx);
        assert_eq!(
            worktree_paths(&project, cx),
            vec![solo.clone(), godot.clone()]
        );
        assert_eq!(scoped_paths(&project, cx), vec![godot.clone()]);
        let godot_worktree = project.read_with(cx, |project, cx| project.focused_worktree(cx));
        // The added project isn't one the window is saved with.
        assert_eq!(
            project.read_with(cx, |project, cx| project.visible_worktrees(cx).count()),
            1
        );

        follow(path!("/repos/solo/crates"), cx);
        assert_eq!(scoped_paths(&project, cx), vec![solo.clone()]);
        assert_eq!(
            worktree_paths(&project, cx),
            vec![solo.clone(), godot]
        );

        // Coming back reuses the kept worktree, so its language servers keep
        // running.
        follow(path!("/repos/godot"), cx);
        assert_eq!(
            project.read_with(cx, |project, cx| project.focused_worktree(cx)),
            godot_worktree
        );
        assert_eq!(worktree_paths(&project, cx).len(), 2);

        // Directories outside a repository are projects of their own.
        follow(path!("/repos/notes"), cx);
        assert_eq!(
            scoped_paths(&project, cx),
            vec![PathBuf::from(path!("/repos/notes"))]
        );

        // Worktrees out of focus long enough are removed; the focused one and
        // the window's own folders stay.
        follower.update(cx, |follower, cx| {
            follower.prune(Instant::now() + Duration::from_secs(1), Duration::ZERO, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            worktree_paths(&project, cx),
            vec![solo, PathBuf::from(path!("/repos/notes"))]
        );

        // Disabling the setting widens the scope back.
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .terminal
                        .get_or_insert_default()
                        .follow_working_directory = Some(false);
                });
            })
        });
        cx.run_until_parked();
        assert_eq!(
            project.read_with(cx, |project, cx| project.focused_worktree(cx)),
            None
        );
        assert_eq!(scoped_paths(&project, cx).len(), 1);
    }

    #[gpui::test]
    async fn test_scope_with_root_inside_window_folder(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/work"),
            json!({ "scratch": { "deeper": {} }, "other": {} }),
        )
        .await;
        let project = Project::test(fs, [Path::new(path!("/work"))], cx).await;
        let follower = cx.new(|cx| {
            WorkingDirectoryFollower::new(project.downgrade(), WeakEntity::new_invalid(), cx)
        });
        follower.update(cx, |follower, cx| {
            follower.follow(PathBuf::from(path!("/work/scratch/deeper")), cx)
        });
        cx.run_until_parked();
        // A folder inside one the window opened stays in that folder's worktree.
        assert_eq!(
            worktree_paths(&project, cx),
            vec![PathBuf::from(path!("/work"))]
        );
        assert_eq!(
            scoped_paths(&project, cx),
            vec![PathBuf::from(path!("/work"))]
        );
    }
}
