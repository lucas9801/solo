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
use gpui::{
    App, AppContext as _, Entity, EntityId, Global, Hsla, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Task, Transformation, Window, div, percentage,
};
use ui::{
    ActiveTheme as _, Color, Icon, IconName, IconSize, Transformable as _,
    utils::calculate_contrast_ratio,
};
use util::i18n::tr;

use crate::Workspace;

/// What an agent in a terminal is doing. Shown alike everywhere, as a traffic
/// light: red, yellow and green want a look, blue and gray don't; each also
/// has its own shape, for when colors are hard to tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// It asked for permission or an answer.
    NeedsInput,
    /// Running but quiet, likely waiting for the next prompt.
    Idle,
    /// Still running, and finished a turn you haven't looked at yet.
    Finished,
    /// Exited.
    Done,
    Failed,
}

impl AgentState {
    /// Whether the agent is still running.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            AgentState::Working | AgentState::NeedsInput | AgentState::Idle | AgentState::Finished
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentState::Working => tr("Working", "工作中"),
            AgentState::NeedsInput => tr("Waiting for you", "等你回复"),
            AgentState::Idle => tr("Idle", "空闲"),
            AgentState::Finished => tr("Done, not seen yet", "完成待看"),
            AgentState::Done => tr("Exited", "已退出"),
            AgentState::Failed => tr("Failed", "失败"),
        }
    }

    pub fn color(self) -> Color {
        match self {
            AgentState::Failed => Color::Error,
            AgentState::NeedsInput => Color::Warning,
            AgentState::Finished => Color::Success,
            AgentState::Working => Color::Info,
            AgentState::Idle | AgentState::Done => Color::Muted,
        }
    }

    /// Its shape, unanimated: a red filled circle with a cross, a yellow dot
    /// in a faint ring, a green dot, a blue turning ring, a gray hollow circle
    /// and a gray dash.
    pub fn icon(self) -> IconName {
        match self {
            AgentState::Failed => IconName::AgentStatusFailed,
            AgentState::NeedsInput => IconName::AgentStatusNeedsInput,
            AgentState::Finished => IconName::AgentStatusFinished,
            AgentState::Working => IconName::AgentStatusWorking,
            AgentState::Idle => IconName::AgentStatusIdle,
            AgentState::Done => IconName::AgentStatusExited,
        }
    }
}

/// SOLO: an agent's status as its shape in its color (see [`AgentState`]),
/// animated when given a phase: the working ring turns and the ring around
/// a waiting agent breathes.
#[derive(IntoElement)]
pub struct AgentStateIcon {
    state: AgentState,
    size: IconSize,
    phase: Option<f32>,
    background: Option<Hsla>,
}

impl AgentStateIcon {
    pub fn new(state: AgentState) -> Self {
        Self {
            state,
            size: IconSize::Small,
            phase: None,
            background: None,
        }
    }

    /// What it's drawn on, to keep it legible there (see [`legible_on`]);
    /// by default, a popover's background.
    pub fn on(mut self, background: Hsla) -> Self {
        self.background = Some(background);
        self
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    /// How far through its animation it is, from 0 to 1: the working ring's
    /// turn, or the waiting ring's breath.
    pub fn phase(mut self, phase: f32) -> Self {
        self.phase = Some(phase);
        self
    }
}

/// The least contrast a status shape has with what it's drawn on.
pub const MIN_STATUS_CONTRAST: f32 = 3.;

/// `color`, darkened on a light `background` or lightened on a dark one as
/// much as it takes to stand out from it by [`MIN_STATUS_CONTRAST`]: some
/// light themes' status colors are too pale on gray.
pub fn legible_on(color: Hsla, background: Hsla) -> Hsla {
    let mut legible = color;
    let darken = background.l > 0.5;
    for _ in 0..50 {
        if calculate_contrast_ratio(legible, background) >= MIN_STATUS_CONTRAST {
            break;
        }
        legible.l = if darken {
            (legible.l - 0.02).max(0.)
        } else {
            (legible.l + 0.02).min(1.)
        };
    }
    legible
}

/// How visible the breathing ring around a waiting agent is at `phase`.
pub fn breath_opacity(phase: f32) -> f32 {
    0.15 + 0.5 * (0.5 - 0.5 * (phase * std::f32::consts::TAU).cos())
}

impl RenderOnce for AgentStateIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let background = self
            .background
            .unwrap_or(cx.theme().colors().elevated_surface_background);
        let color = Color::Custom(legible_on(self.state.color().color(cx), background));
        match (self.state, self.phase) {
            (AgentState::Working, Some(phase)) => Icon::new(self.state.icon())
                .size(self.size)
                .color(color)
                .transform(Transformation::rotate(percentage(phase)))
                .into_any_element(),
            (AgentState::NeedsInput, Some(phase)) => {
                let ring = color.color(cx).opacity(breath_opacity(phase));
                div()
                    .relative()
                    .flex_none()
                    .size(self.size.rems())
                    .child(
                        Icon::new(IconName::AgentStatusNeedsInputRing)
                            .size(self.size)
                            .color(Color::Custom(ring)),
                    )
                    .child(
                        div().absolute().top_0().left_0().child(
                            Icon::new(IconName::AgentStatusNeedsInputDot)
                                .size(self.size)
                                .color(color),
                        ),
                    )
                    .into_any_element()
            }
            _ => Icon::new(self.state.icon())
                .size(self.size)
                .color(color)
                .into_any_element(),
        }
    }
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

/// How many of a workspace's agents want a look or work, for its tab; idle
/// and exited ones aren't counted.
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::rgb;

    #[test]
    fn test_legible_on() {
        // One Light's success color on its inactive tabs: too pale, darkened.
        let (success, tab) = (Hsla::from(rgb(0x669f59)), Hsla::from(rgb(0xebebec)));
        assert!(calculate_contrast_ratio(success, tab) < MIN_STATUS_CONTRAST);
        let legible = legible_on(success, tab);
        assert!(calculate_contrast_ratio(legible, tab) >= MIN_STATUS_CONTRAST);
        assert!(legible.l < success.l && (legible.h - success.h).abs() < 0.01);
        // Ayu Dark's: left as it is.
        let (success, tab) = (Hsla::from(rgb(0xaad84c)), Hsla::from(rgb(0x1f2127)));
        assert_eq!(legible_on(success, tab), success);
    }
}
