//! SOLO: lists each workspace's terminals and what their agents are doing, for
//! the workspace tabs in the title bar (see `workspace::agent_activity`).

use gpui::{App, Entity, EntityId, Window};
use workspace::{
    Workspace,
    agent_activity::{self, AgentState, WorkspaceTerminal},
};

use crate::{
    agent_status::AgentStatus,
    command_blocks::{activate_terminal_view, terminal_views},
};

pub(crate) fn init(cx: &mut App) {
    agent_activity::register(list, activate, cx);
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
        AgentStatus::Done => AgentState::Done,
        AgentStatus::Failed => AgentState::Failed,
    }
}
