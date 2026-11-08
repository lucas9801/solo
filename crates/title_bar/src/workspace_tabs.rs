//! SOLO: a tab in the title bar for each workspace (project folder) open in the
//! window. Switching tabs swaps the terminals, editors and file tree for that
//! workspace's own; each tab shows what its agents are doing, so one waiting
//! for an answer in the background is seen without switching to it.
//!
//! The tabs are the window's project groups: after a restart only the active
//! workspace is loaded, and the others load when their tab is first clicked.

use std::path::PathBuf;

use gpui::{
    Action, AnyElement, App, ClickEvent, Entity, MouseButton, SharedString, WeakEntity, Window,
    actions,
};
use schemars::JsonSchema;
use serde::Deserialize;
use ui::{ContextMenu, ContextMenuEntry, PopoverMenu, Tooltip, prelude::*, right_click_menu};
use util::i18n::tr;
use workspace::{
    MultiWorkspace, OpenMode, ProjectGroupKey, RemovalIntent, Workspace,
    agent_activity::{self, AgentCounts, AgentState, WorkspaceTask, WorkspaceTerminal},
};
use zed_actions::agent_task::{ManageAgentTasks, NewAgentTask};

use crate::TitleBar;

const MAX_WORKSPACE_NAME_LENGTH: usize = 32;

actions!(
    solo,
    [
        /// Switches to the next workspace tab in the title bar.
        NextWorkspace,
        /// Switches to the previous workspace tab in the title bar.
        PreviousWorkspace,
    ]
);

/// Switches to the workspace tab at `index` (from 0) in the title bar.
#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = solo)]
#[serde(deny_unknown_fields)]
pub struct ActivateWorkspace {
    pub index: usize,
}

pub(crate) fn register_actions(workspace: &mut Workspace) {
    workspace.register_action(|workspace, action: &ActivateWorkspace, window, cx| {
        let index = action.index;
        switch_workspace(workspace, window, cx, move |_, _| Some(index));
    });
    workspace.register_action(|workspace, _: &NextWorkspace, window, cx| {
        switch_workspace(workspace, window, cx, |active, count| {
            Some(active.map_or(0, |active| (active + 1) % count))
        });
    });
    workspace.register_action(|workspace, _: &PreviousWorkspace, window, cx| {
        switch_workspace(workspace, window, cx, |active, count| {
            Some(active.map_or(0, |active| (active + count - 1) % count))
        });
    });
}

/// Switches to the tab `pick` chooses from the active tab's index and the
/// number of tabs.
fn switch_workspace(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
    pick: impl FnOnce(Option<usize>, usize) -> Option<usize> + 'static,
) {
    let Some(multi_workspace) = workspace.multi_workspace().and_then(|mw| mw.upgrade()) else {
        return;
    };
    // The workspace this runs in is being updated; switching reads it.
    window.defer(cx, move |window, cx| {
        let tabs = workspace_tabs(&multi_workspace, cx);
        if tabs.is_empty() {
            return;
        }
        let active = tabs.iter().position(|tab| tab.is_active);
        if let Some(tab) = pick(active, tabs.len()).and_then(|index| tabs.get(index)) {
            activate_tab(&multi_workspace.downgrade(), tab, window, cx);
        }
    });
}

/// A workspace tab: a project group, with its workspace once it's loaded.
#[derive(Clone)]
struct WorkspaceTab {
    /// `None` for a workspace without a folder.
    key: Option<ProjectGroupKey>,
    workspace: Option<Entity<Workspace>>,
    path: Option<PathBuf>,
    is_active: bool,
}

impl TitleBar {
    /// The workspace tabs, then `+` to open a folder as another workspace.
    /// The active tab is named `active_name` and holds `branch`.
    pub(crate) fn render_workspace_tabs(
        &self,
        multi_workspace: Entity<MultiWorkspace>,
        active_name: Option<SharedString>,
        mut branch: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tabs = workspace_tabs(&multi_workspace, cx);
        let can_close = tabs.len() > 1;
        let colors = cx.theme().colors();
        let (selected, hover) = (colors.element_selected, colors.element_hover);

        let mut row = h_flex().h_full().gap_0p5();
        for (ix, tab) in tabs.into_iter().enumerate() {
            let is_active = tab.is_active;
            let name = active_name
                .clone()
                .filter(|_| is_active)
                .unwrap_or_else(|| tab_name(&tab));
            let name: SharedString =
                util::truncate_and_trailoff(&name, MAX_WORKSPACE_NAME_LENGTH).into();
            let counts = tab
                .workspace
                .as_ref()
                .map(|workspace| AgentCounts::of(&agent_activity::terminals(workspace, cx)))
                .unwrap_or_default();
            let group = SharedString::from(format!("workspace-tab-{ix}"));

            let element = h_flex()
                .id(("workspace-tab", ix))
                .group(group.clone())
                .h(rems_from_px(24_f32))
                .pl_1p5()
                .pr_0p5()
                .gap_1()
                .rounded_sm()
                .cursor_pointer()
                .when(is_active, |element| element.bg(selected))
                .when(!is_active, |element| element.hover(|style| style.bg(hover)))
                .child(
                    Label::new(name)
                        .size(LabelSize::Small)
                        .color(if is_active {
                            Color::Default
                        } else {
                            Color::Muted
                        })
                        .untranslated(),
                )
                .children(render_counts(counts))
                .when(is_active, |element| element.children(branch.take()))
                .child(div().visible_on_hover(group).child(render_menu_button(
                    ix,
                    &multi_workspace,
                    &tab,
                    can_close,
                )))
                .when_some(tab.path.clone(), |element, path| {
                    element.tooltip(Tooltip::text(path.to_string_lossy().into_owned()))
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click({
                    let multi_workspace = multi_workspace.downgrade();
                    let tab = tab.clone();
                    move |_: &ClickEvent, window, cx| {
                        activate_tab(&multi_workspace, &tab, window, cx);
                    }
                });

            let menu_multi_workspace = multi_workspace.clone();
            row = row.child(
                right_click_menu(("workspace-tab-menu", ix))
                    .trigger(move |_, _, _| element)
                    .menu(move |window, cx| {
                        workspace_menu(
                            menu_multi_workspace.clone(),
                            tab.clone(),
                            can_close,
                            window,
                            cx,
                        )
                    }),
            );
        }

        row.child(
            IconButton::new("open-workspace", IconName::Plus)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text(tr(
                    "Open a Folder as Another Workspace",
                    "打开文件夹作为新的工作区",
                )))
                .on_click(|_, window, cx| {
                    window.dispatch_action(
                        Box::new(workspace::Open {
                            create_new_window: Some(false),
                        }),
                        cx,
                    );
                }),
        )
        .into_any_element()
    }
}

/// The window's project groups in order, with the active workspace first when
/// it has no folder (and so no group).
fn workspace_tabs(multi_workspace: &Entity<MultiWorkspace>, cx: &App) -> Vec<WorkspaceTab> {
    let multi_workspace = multi_workspace.read(cx);
    let active = multi_workspace.workspace().clone();
    let groups = multi_workspace.project_groups(cx);
    // The active workspace may not be in its group's list yet, as right after
    // a restart.
    let active_key = active.read(cx).project_group_key(cx);
    let is_active_group = |group: &workspace::ProjectGroup| {
        group.key == active_key || group.workspaces.contains(&active)
    };
    let mut tabs = Vec::with_capacity(groups.len() + 1);
    if !groups.iter().any(is_active_group) {
        tabs.push(WorkspaceTab {
            key: None,
            path: active
                .read(cx)
                .root_paths(cx)
                .first()
                .map(|path| path.to_path_buf()),
            workspace: Some(active.clone()),
            is_active: true,
        });
    }
    for group in groups {
        let is_active = is_active_group(&group);
        let workspace = if is_active {
            Some(active.clone())
        } else {
            multi_workspace
                .last_active_workspace_for_group(&group.key, cx)
                .or_else(|| group.workspaces.first().cloned())
        };
        let path = group.key.path_list().ordered_paths().next().cloned();
        tabs.push(WorkspaceTab {
            path,
            key: Some(group.key),
            workspace,
            is_active,
        });
    }
    tabs
}

/// Shows the tab's workspace, loading it first if it isn't yet.
fn activate_tab(
    multi_workspace: &WeakEntity<MultiWorkspace>,
    tab: &WorkspaceTab,
    window: &mut Window,
    cx: &mut App,
) {
    let tab = tab.clone();
    multi_workspace
        .update(cx, |multi_workspace, cx| match (tab.workspace, tab.key) {
            (Some(workspace), _) => multi_workspace.activate(workspace, None, window, cx),
            (None, Some(key)) => multi_workspace
                .find_or_create_local_workspace(
                    key.path_list().clone(),
                    Some(key),
                    None,
                    OpenMode::Activate,
                    None,
                    window,
                    cx,
                )
                .detach_and_log_err(cx),
            (None, None) => {}
        })
        .ok();
}

fn render_menu_button(
    ix: usize,
    multi_workspace: &Entity<MultiWorkspace>,
    tab: &WorkspaceTab,
    can_close: bool,
) -> impl IntoElement {
    let multi_workspace = multi_workspace.clone();
    let tab = tab.clone();
    PopoverMenu::new(("workspace-tab-agents", ix))
        .trigger(
            IconButton::new(("workspace-tab-agents-button", ix), IconName::ChevronDown)
                .icon_size(IconSize::XSmall)
                .icon_color(Color::Muted),
        )
        .menu(move |window, cx| {
            Some(workspace_menu(
                multi_workspace.clone(),
                tab.clone(),
                can_close,
                window,
                cx,
            ))
        })
        .anchor(gpui::Anchor::TopLeft)
}

/// How many agents work, wait for an answer and failed, as colored counts.
fn render_counts(counts: AgentCounts) -> impl Iterator<Item = AnyElement> {
    [
        (counts.needs_input, IconName::Bell, Color::Warning),
        (counts.working, IconName::LoadCircle, Color::Accent),
        (counts.failed, IconName::XCircle, Color::Error),
    ]
    .into_iter()
    .filter(|(count, ..)| *count > 0)
    .map(|(count, icon, color)| {
        h_flex()
            .gap_0p5()
            .child(Icon::new(icon).size(IconSize::XSmall).color(color))
            .child(
                Label::new(count.to_string())
                    .size(LabelSize::XSmall)
                    .color(color)
                    .untranslated(),
            )
            .into_any_element()
    })
}

/// The workspace's terminals, to show one, then what to do with the workspace.
fn workspace_menu(
    multi_workspace: Entity<MultiWorkspace>,
    tab: WorkspaceTab,
    can_close: bool,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    // Tasks are listed as last seen, then again: the menu updates once they are.
    if let Some(workspace) = &tab.workspace {
        agent_activity::refresh_tasks(workspace, cx);
    }
    let builder_tab = tab.clone();
    let menu = ContextMenu::build_persistent(window, cx, move |menu, _, cx| {
        build_workspace_menu(menu, &multi_workspace, &builder_tab, can_close, cx)
    });
    if tab.workspace.is_some() {
        let activity = agent_activity::AgentActivity::global(cx);
        menu.update(cx, |_, cx| {
            cx.observe_in(
                &activity,
                window,
                |menu: &mut ContextMenu, _, window, cx| menu.rebuild(window, cx),
            )
            .detach();
        });
    }
    menu
}

fn terminal_entry(
    terminal: &WorkspaceTerminal,
    multi_workspace: &Entity<MultiWorkspace>,
    tab: &WorkspaceTab,
    indent: bool,
) -> ContextMenuEntry {
    let (icon, color) = match terminal.agent {
        Some(AgentState::Working) => (IconName::LoadCircle, Color::Accent),
        Some(AgentState::NeedsInput) => (IconName::Bell, Color::Warning),
        Some(AgentState::Idle) => (IconName::Circle, Color::Muted),
        Some(AgentState::Done) => (IconName::Check, Color::Success),
        Some(AgentState::Failed) => (IconName::XCircle, Color::Error),
        None => (IconName::Terminal, Color::Muted),
    };
    let multi_workspace = multi_workspace.downgrade();
    let tab = tab.clone();
    let id = terminal.id;
    let title = if indent {
        SharedString::from(format!("    {}", terminal.title))
    } else {
        terminal.title.clone()
    };
    ContextMenuEntry::new(title)
        .icon(icon)
        .icon_color(color)
        .handler(move |window, cx| {
            activate_tab(&multi_workspace, &tab, window, cx);
            if let Some(workspace) = &tab.workspace {
                agent_activity::activate_terminal(workspace, id, window, cx);
            }
        })
}

/// What the task changed so far, for its menu entry.
fn task_label(task: &WorkspaceTask) -> SharedString {
    let chinese = util::i18n::is_chinese();
    let mut parts = Vec::new();
    if task.worktree.is_none() {
        parts.push(tr("worktree gone", "worktree 已不在").to_string());
    }
    if task.changed_files > 0 {
        parts.push(if chinese {
            format!("{} 个文件未提交", task.changed_files)
        } else if task.changed_files == 1 {
            "1 file uncommitted".to_string()
        } else {
            format!("{} files uncommitted", task.changed_files)
        });
    }
    if task.unmerged_commits > 0 {
        parts.push(if chinese {
            format!("{} 个提交未合并", task.unmerged_commits)
        } else if task.unmerged_commits == 1 {
            "1 commit unmerged".to_string()
        } else {
            format!("{} commits unmerged", task.unmerged_commits)
        });
    }
    if parts.is_empty() {
        task.name.clone()
    } else {
        format!("{} · {}", task.name, parts.join(" · ")).into()
    }
}

/// The workspace's terminals, its agent tasks with theirs, then what to do
/// with the workspace.
fn build_workspace_menu(
    mut menu: ContextMenu,
    multi_workspace: &Entity<MultiWorkspace>,
    tab: &WorkspaceTab,
    can_close: bool,
    cx: &mut App,
) -> ContextMenu {
    menu = menu.keep_open_on_confirm(false);
    let terminals = tab
        .workspace
        .as_ref()
        .map(|workspace| agent_activity::terminals(workspace, cx))
        .unwrap_or_default();
    let tasks = tab
        .workspace
        .as_ref()
        .map(|workspace| agent_activity::tasks(workspace, cx))
        .unwrap_or_default();
    let in_a_task = |terminal: &WorkspaceTerminal| {
        tasks
            .iter()
            .any(|task| task.terminals.contains(&terminal.id))
    };
    match &tab.workspace {
        Some(_) if terminals.is_empty() => {
            menu = menu.label(tr("No terminals", "没有终端"));
        }
        Some(_) => {}
        None => {
            menu = menu.label(tr("Opens when you switch to it", "切换过去时才会打开"));
        }
    }
    for terminal in terminals.iter().filter(|terminal| !in_a_task(terminal)) {
        menu = menu.item(terminal_entry(terminal, multi_workspace, tab, false));
    }
    if let Some(workspace) = &tab.workspace {
        if !tasks.is_empty() {
            menu = menu.separator().header(tr("Agent Tasks", "agent 任务"));
        }
        for task in &tasks {
            let multi_workspace_handle = multi_workspace.downgrade();
            let entry_tab = tab.clone();
            let workspace = workspace.clone();
            let open = task.clone();
            menu = menu.item(
                ContextMenuEntry::new(task_label(task))
                    .icon(IconName::GitBranch)
                    .icon_color(Color::Muted)
                    .disabled(task.worktree.is_none() && task.terminals.is_empty())
                    .handler(move |window, cx| {
                        activate_tab(&multi_workspace_handle, &entry_tab, window, cx);
                        agent_activity::open_task(&workspace, &open, window, cx);
                    }),
            );
            for terminal in terminals
                .iter()
                .filter(|terminal| task.terminals.contains(&terminal.id))
            {
                menu = menu.item(terminal_entry(terminal, multi_workspace, tab, true));
            }
        }
        menu = menu.separator();
        for (label, action) in [
            (
                tr("New Agent Task…", "新建 agent 任务…"),
                NewAgentTask.boxed_clone(),
            ),
            (
                tr("Manage Agent Tasks…", "管理 agent 任务…"),
                ManageAgentTasks.boxed_clone(),
            ),
        ] {
            let multi_workspace_handle = multi_workspace.downgrade();
            let entry_tab = tab.clone();
            menu = menu.entry(label, None, move |window, cx| {
                activate_tab(&multi_workspace_handle, &entry_tab, window, cx);
                let action = action.boxed_clone();
                // Once the tab is active, so the task lands in its workspace.
                window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
            });
        }
    }
    menu = menu.separator();
    if let Some(path) = tab.path.clone() {
        menu = menu.entry(
            tr("Reveal in File Manager", "在文件管理器中显示"),
            None,
            move |_, cx| cx.reveal_path(&path),
        );
    }
    if can_close {
        let multi_workspace = multi_workspace.downgrade();
        let tab = tab.clone();
        menu = menu.entry(
            tr("Close Workspace", "关闭工作区"),
            None,
            move |window, cx| {
                multi_workspace
                    .update(cx, |multi_workspace, cx| {
                        let task = match (&tab.key, &tab.workspace) {
                            (Some(key), _) => multi_workspace.remove_project_group(key, window, cx),
                            (None, Some(workspace)) => multi_workspace.remove(
                                [workspace.clone()],
                                RemovalIntent::CloseProject,
                                window,
                                cx,
                            ),
                            (None, None) => return,
                        };
                        task.detach_and_log_err(cx);
                    })
                    .ok();
            },
        );
    }
    menu
}

fn tab_name(tab: &WorkspaceTab) -> SharedString {
    tab.path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| SharedString::from(name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| tr("Empty", "空").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_label() {
        util::i18n::set_language(util::i18n::Language::English);
        let task = |changed_files, unmerged_commits, worktree: Option<&str>| WorkspaceTask {
            name: "fix-login".into(),
            branch: "solo/fix-login".into(),
            worktree: worktree.map(PathBuf::from),
            changed_files,
            unmerged_commits,
            terminals: Vec::new(),
        };
        assert_eq!(task_label(&task(0, 0, Some("/w"))).as_ref(), "fix-login");
        assert_eq!(
            task_label(&task(3, 1, Some("/w"))).as_ref(),
            "fix-login · 3 files uncommitted · 1 commit unmerged"
        );
        assert_eq!(
            task_label(&task(0, 0, None)).as_ref(),
            "fix-login · worktree gone"
        );
    }
}
