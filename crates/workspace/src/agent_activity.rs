//! SOLO: what the agents and terminals of each workspace are doing, for the
//! workspace tabs in the title bar.
//!
//! The terminal crate knows its terminals and their agents but the title bar
//! can't depend on it, so it registers how to list and activate them here, and
//! calls [`notify`] when what it lists changes. Title bars observe
//! [`AgentActivity::global`] to re-render.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use collections::HashMap;
use gpui::{App, AppContext as _, Entity, EntityId, Global, SharedString, Task, Window};

use crate::Workspace;

/// What an agent in a terminal is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// It asked for permission or an answer.
    NeedsInput,
    /// Running but quiet, likely waiting for the next prompt.
    Idle,
    /// Still running, and finished a turn you haven't looked at yet.
    Finished,
    Done,
    Failed,
}

/// A terminal of a workspace, with the agent it runs.
#[derive(Clone, Debug)]
pub struct WorkspaceTerminal {
    /// The terminal's view, to activate it with.
    pub id: EntityId,
    pub title: SharedString,
    /// `None` for a terminal that runs no agent.
    pub agent: Option<AgentState>,
}

/// How many of a workspace's agents are doing what, for its tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentCounts {
    pub working: usize,
    pub needs_input: usize,
    /// Finished a turn you haven't looked at yet.
    pub finished: usize,
    pub failed: usize,
}

impl AgentCounts {
    pub fn of(terminals: &[WorkspaceTerminal]) -> Self {
        let mut counts = Self::default();
        for terminal in terminals {
            match terminal.agent {
                Some(AgentState::Working) => counts.working += 1,
                Some(AgentState::NeedsInput) => counts.needs_input += 1,
                Some(AgentState::Finished) => counts.finished += 1,
                Some(AgentState::Failed) => counts.failed += 1,
                Some(AgentState::Idle | AgentState::Done) | None => {}
            }
        }
        counts
    }
}

/// An agent task of a workspace's repositories: an agent's own worktree and
/// branch.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceTask {
    pub name: SharedString,
    pub branch: SharedString,
    /// `None` if the worktree is gone.
    pub worktree: Option<PathBuf>,
    /// Files changed in the worktree and not committed.
    pub changed_files: usize,
    /// Commits on the branch not merged into the one it started from.
    pub unmerged_commits: usize,
    /// The terminals in its worktree.
    pub terminals: Vec<EntityId>,
}

type ListTerminals = fn(&Entity<Workspace>, &App) -> Vec<WorkspaceTerminal>;
type ActivateTerminal = fn(&Entity<Workspace>, EntityId, &mut Window, &mut App);
type ListTasks = fn(&Entity<Workspace>, &mut App) -> Task<Vec<WorkspaceTask>>;
type OpenTask = fn(&Entity<Workspace>, &WorkspaceTask, &mut Window, &mut App);

/// How long listed tasks are taken as current: listing runs git.
const TASKS_FRESH_FOR: Duration = Duration::from_secs(3);

/// The registered terminal provider; observe it for changes.
pub struct AgentActivity {
    list: Option<ListTerminals>,
    activate: Option<ActivateTerminal>,
    list_tasks: Option<ListTasks>,
    open_task: Option<OpenTask>,
    /// Each workspace's tasks as last listed, and when.
    tasks: HashMap<EntityId, (Instant, Vec<WorkspaceTask>)>,
    listing_tasks: HashMap<EntityId, Task<()>>,
}

struct GlobalAgentActivity(Entity<AgentActivity>);

impl Global for GlobalAgentActivity {}

impl AgentActivity {
    pub fn global(cx: &mut App) -> Entity<AgentActivity> {
        if let Some(global) = cx.try_global::<GlobalAgentActivity>() {
            return global.0.clone();
        }
        let entity = cx.new(|_| AgentActivity {
            list: None,
            activate: None,
            list_tasks: None,
            open_task: None,
            tasks: HashMap::default(),
            listing_tasks: HashMap::default(),
        });
        cx.set_global(GlobalAgentActivity(entity.clone()));
        entity
    }
}

/// Sets how to list a workspace's terminals and activate one of them.
pub fn register(list: ListTerminals, activate: ActivateTerminal, cx: &mut App) {
    AgentActivity::global(cx).update(cx, |activity, cx| {
        activity.list = Some(list);
        activity.activate = Some(activate);
        cx.notify();
    });
}

/// Sets how to list a workspace's agent tasks and open one of them.
pub fn register_tasks(list: ListTasks, open: OpenTask, cx: &mut App) {
    AgentActivity::global(cx).update(cx, |activity, cx| {
        activity.list_tasks = Some(list);
        activity.open_task = Some(open);
        cx.notify();
    });
}

/// The agent tasks of `workspace`, as last listed by [`refresh_tasks`].
pub fn tasks(workspace: &Entity<Workspace>, cx: &App) -> Vec<WorkspaceTask> {
    cx.try_global::<GlobalAgentActivity>()
        .and_then(|global| {
            global
                .0
                .read(cx)
                .tasks
                .get(&workspace.entity_id())
                .map(|(_, tasks)| tasks.clone())
        })
        .unwrap_or_default()
}

/// Lists the agent tasks of `workspace` again, unless they just were;
/// [`AgentActivity::global`] notifies once they're listed.
pub fn refresh_tasks(workspace: &Entity<Workspace>, cx: &mut App) {
    let activity = AgentActivity::global(cx);
    let id = workspace.entity_id();
    let (list, fresh, listing) = {
        let activity = activity.read(cx);
        (
            activity.list_tasks,
            activity
                .tasks
                .get(&id)
                .is_some_and(|(listed_at, _)| listed_at.elapsed() < TASKS_FRESH_FOR),
            activity.listing_tasks.contains_key(&id),
        )
    };
    let Some(list) = list else {
        return;
    };
    if fresh || listing {
        return;
    }
    let listed = list(workspace, cx);
    let workspace = workspace.downgrade();
    activity.update(cx, |activity, cx| {
        let task = cx.spawn(async move |activity, cx| {
            let tasks = listed.await;
            activity
                .update(cx, |activity, cx| {
                    activity.listing_tasks.remove(&id);
                    if workspace.upgrade().is_some() {
                        activity.tasks.insert(id, (Instant::now(), tasks));
                    } else {
                        activity.tasks.remove(&id);
                    }
                    cx.notify();
                })
                .ok();
        });
        activity.listing_tasks.insert(id, task);
    });
}

/// Shows a terminal of `task`, opening one in its worktree if it has none.
pub fn open_task(
    workspace: &Entity<Workspace>,
    task: &WorkspaceTask,
    window: &mut Window,
    cx: &mut App,
) {
    let open = cx
        .try_global::<GlobalAgentActivity>()
        .and_then(|global| global.0.read(cx).open_task);
    if let Some(open) = open {
        open(workspace, task, window, cx);
    }
}

/// What a workspace's terminals list changed: an agent started, stopped or
/// changed what it's doing, or a terminal was renamed.
pub fn notify(cx: &mut App) {
    if let Some(global) = cx.try_global::<GlobalAgentActivity>() {
        global.0.clone().update(cx, |_, cx| cx.notify());
    }
}

/// The terminals of `workspace`, in tab order.
pub fn terminals(workspace: &Entity<Workspace>, cx: &App) -> Vec<WorkspaceTerminal> {
    cx.try_global::<GlobalAgentActivity>()
        .and_then(|global| global.0.read(cx).list)
        .map(|list| list(workspace, cx))
        .unwrap_or_default()
}

/// Shows the terminal `id` of `workspace` and focuses it.
pub fn activate_terminal(
    workspace: &Entity<Workspace>,
    id: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let activate = cx
        .try_global::<GlobalAgentActivity>()
        .and_then(|global| global.0.read(cx).activate);
    if let Some(activate) = activate {
        activate(workspace, id, window, cx);
    }
}
