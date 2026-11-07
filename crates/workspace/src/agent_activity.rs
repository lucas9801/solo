//! SOLO: what the agents and terminals of each workspace are doing, for the
//! workspace tabs in the title bar.
//!
//! The terminal crate knows its terminals and their agents but the title bar
//! can't depend on it, so it registers how to list and activate them here, and
//! calls [`notify`] when what it lists changes. Title bars observe
//! [`AgentActivity::global`] to re-render.

use gpui::{App, AppContext as _, Entity, EntityId, Global, SharedString, Window};

use crate::Workspace;

/// What an agent in a terminal is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// It asked for permission or an answer.
    NeedsInput,
    /// Running but quiet, likely waiting for the next prompt.
    Idle,
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
    pub failed: usize,
}

impl AgentCounts {
    pub fn of(terminals: &[WorkspaceTerminal]) -> Self {
        let mut counts = Self::default();
        for terminal in terminals {
            match terminal.agent {
                Some(AgentState::Working) => counts.working += 1,
                Some(AgentState::NeedsInput) => counts.needs_input += 1,
                Some(AgentState::Failed) => counts.failed += 1,
                Some(AgentState::Idle | AgentState::Done) | None => {}
            }
        }
        counts
    }
}

type ListTerminals = fn(&Entity<Workspace>, &App) -> Vec<WorkspaceTerminal>;
type ActivateTerminal = fn(&Entity<Workspace>, EntityId, &mut Window, &mut App);

/// The registered terminal provider; observe it for changes.
pub struct AgentActivity {
    list: Option<ListTerminals>,
    activate: Option<ActivateTerminal>,
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
