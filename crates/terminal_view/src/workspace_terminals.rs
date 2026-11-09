//! SOLO: lists each workspace's terminals and what their agents are doing, for
//! the workspace tabs in the title bar (see `workspace::agent_activity`).

use gpui::{App, AppContext as _, Entity, EntityId, Task, Window};
use workspace::{
    Workspace,
    agent_activity::{self, AgentState, WorkspaceTask, WorkspaceTerminal},
};

use crate::{
    agent_status::AgentStatus,
    agent_tasks::{self, git},
    command_blocks::{activate_terminal_view, terminal_views},
};

pub(crate) fn init(cx: &mut App) {
    agent_activity::register(list, activate, cx);
    agent_activity::register_tasks(list_tasks, open_task, cx);
}

/// The workspace's agent tasks, with what each changed so far.
fn list_tasks(workspace: &Entity<Workspace>, cx: &mut App) -> Task<Vec<WorkspaceTask>> {
    let entries = agent_tasks::agent_tasks(workspace, cx);
    cx.spawn(async move |cx| {
        let entries = match entries.await {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("failed to list the agent tasks: {error:#}");
                return Vec::new();
            }
        };
        cx.background_spawn(async move {
            entries
                .into_iter()
                .map(|entry| {
                    let task = &entry.task;
                    let changed_files = task
                        .worktree_path
                        .as_deref()
                        .and_then(|worktree| git::uncommitted_changes(worktree).ok())
                        .map_or(0, |changes| changes.len());
                    let unmerged_commits =
                        git::unmerged_commits(task).map_or(0, |commits| commits.len());
                    WorkspaceTask {
                        name: agent_tasks::display_name(task).into(),
                        branch: task.branch.clone().into(),
                        // Where its terminal opens: for a task spanning
                        // repositories, the folder with all of them.
                        worktree: task
                            .worktree_path
                            .as_ref()
                            .map(|worktree| task.group.clone().unwrap_or_else(|| worktree.clone())),
                        changed_files,
                        unmerged_commits,
                        terminals: entry
                            .terminals
                            .iter()
                            .map(|view| view.entity_id())
                            .collect(),
                    }
                })
                .collect()
        })
        .await
    })
}

fn open_task(
    workspace: &Entity<Workspace>,
    task: &WorkspaceTask,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(id) = task.terminals.first() {
        activate(workspace, *id, window, cx);
        return;
    }
    let Some(worktree) = task.worktree.clone() else {
        return;
    };
    let name = task.name.to_string();
    agent_tasks::open_task_terminal_in(workspace, worktree, name, window, cx);
}

fn list(workspace: &Entity<Workspace>, cx: &App) -> Vec<WorkspaceTerminal> {
    terminal_views(workspace, cx)
        .into_iter()
        .map(|entity| {
            let view = entity.read(cx);
            WorkspaceTerminal {
                id: entity.entity_id(),
                title: view.smart_title(0, true, cx).into(),
                agent: view.agent_status().map(|(_, status)| agent_state(status)),
            }
        })
        .collect()
}

fn activate(workspace: &Entity<Workspace>, id: EntityId, window: &mut Window, cx: &mut App) {
    if let Some(view) = terminal_views(workspace, cx)
        .into_iter()
        .find(|view| view.entity_id() == id)
    {
        activate_terminal_view(workspace, &view, window, cx);
    }
}

fn agent_state(status: AgentStatus) -> AgentState {
    match status {
        AgentStatus::Working => AgentState::Working,
        AgentStatus::NeedsInput => AgentState::NeedsInput,
        AgentStatus::Idle => AgentState::Idle,
        AgentStatus::Finished => AgentState::Finished,
        AgentStatus::Done => AgentState::Done,
        AgentStatus::Failed => AgentState::Failed,
    }
}
