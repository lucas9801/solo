//! SOLO: lists each workspace's terminals and what their agents are doing, for
//! the workspace tabs in the title bar (see `workspace::agent_activity`), and
//! keeps the focus in the terminal when switching between workspaces.

use gpui::{App, AppContext as _, Entity, EntityId, Focusable as _, Task, Window};
use workspace::{
    MultiWorkspace, MultiWorkspaceEvent, Workspace,
    agent_activity::{self, WorkspaceTask, WorkspaceTerminal},
};

use crate::{
    TerminalView,
    agent_tasks::{self, git},
    command_blocks::{activate_terminal_view, terminal_views},
    terminal_panel::TerminalPanel,
};

pub(crate) fn init(cx: &mut App) {
    agent_activity::register(list, activate, cx);
    agent_activity::register_tasks(list_tasks, open_task, cx);
    cx.observe_new(|_: &mut MultiWorkspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        cx.subscribe_in(
            &cx.entity(),
            window,
            |multi_workspace, _, event: &MultiWorkspaceEvent, window, cx| {
                if let MultiWorkspaceEvent::ActiveWorkspaceChanged { .. } = event {
                    focus_terminal_after_switch(multi_workspace.workspace(), window, cx);
                }
            },
        )
        .detach();
    })
    .detach();
}

/// Switching to a workspace focuses its center pane; with the terminal panel
/// open, its active terminal gets the focus instead. Left alone if something
/// moved the focus since the switch, as showing a picked terminal does.
fn focus_terminal_after_switch(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let workspace = workspace.read(cx);
    let center = workspace.active_pane().focus_handle(cx);
    if !center.is_focused(window) || workspace.fallback_focus_handle(window, cx) != center {
        return;
    }
    if let Some(view) = shown_panel_terminal(workspace, cx) {
        window.focus(&view.focus_handle(cx), cx);
    }
}

/// The active terminal of the terminal panel, if the panel is open.
fn shown_panel_terminal(workspace: &Workspace, cx: &App) -> Option<Entity<TerminalView>> {
    let panel = workspace.panel::<TerminalPanel>(cx)?;
    let shown = workspace.all_docks().iter().any(|dock| {
        dock.read(cx)
            .visible_panel()
            .is_some_and(|shown| shown.panel_id() == panel.entity_id())
    });
    if !shown {
        return None;
    }
    panel
        .read(cx)
        .active_pane
        .read(cx)
        .active_item()?
        .downcast::<TerminalView>()
}

/// Shows the terminal `view` in whichever workspace of the window holds it,
/// and focuses it.
pub(crate) fn show_terminal(view: &Entity<TerminalView>, window: &mut Window, cx: &mut App) {
    let workspace = window
        .root::<MultiWorkspace>()
        .flatten()
        .and_then(|multi_workspace| {
            multi_workspace
                .read(cx)
                .workspaces()
                .find(|workspace| terminal_views(workspace, cx).contains(view))
                .cloned()
        });
    match workspace {
        Some(workspace) => activate_terminal_view(&workspace, view, window, cx),
        None => window.focus(&view.focus_handle(cx), cx),
    }
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
                agent: view.agent_status().map(|(_, status)| status),
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

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext, WindowHandle,
    };
    use project::{FakeFs, Project};
    use settings::SettingsStore;
    use terminal::terminal_settings::CursorShape;
    use util::paths::PathStyle;
    use workspace::{MultiWorkspace, Workspace, agent_activity};

    use crate::{TerminalView, terminal_panel::TerminalPanel};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut store = SettingsStore::test(cx);
            store.update_user_settings(cx, |settings| {
                settings.terminal.get_or_insert_default().starts_open = Some(false);
            });
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    /// A workspace whose open terminal panel holds two terminals, the second
    /// one active.
    struct TestWorkspace {
        workspace: Entity<Workspace>,
        terminals: Vec<Entity<TerminalView>>,
    }

    fn add_workspace(
        window_handle: WindowHandle<MultiWorkspace>,
        project: Entity<Project>,
        first: bool,
        cx: &mut TestAppContext,
    ) -> TestWorkspace {
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                let workspace = if first {
                    multi_workspace.workspace().clone()
                } else {
                    multi_workspace.test_add_workspace(project.clone(), window, cx)
                };
                let panel = workspace.update(cx, |workspace, cx| {
                    let panel = cx.new(|cx| TerminalPanel::new(workspace, window, cx));
                    workspace.add_panel(panel.clone(), window, cx);
                    panel
                });
                let pane = panel.read(cx).active_pane.clone();
                let terminals = (0..2)
                    .map(|_| {
                        let terminal = cx.new(|cx| {
                            terminal::TerminalBuilder::new_display_only(
                                CursorShape::default(),
                                terminal::terminal_settings::AlternateScroll::On,
                                None,
                                0,
                                cx.background_executor(),
                                PathStyle::local(),
                            )
                            .subscribe(cx)
                        });
                        let view = cx.new(|cx| {
                            TerminalView::new(
                                terminal,
                                workspace.downgrade(),
                                None,
                                project.downgrade(),
                                window,
                                cx,
                            )
                        });
                        pane.update(cx, |pane, cx| {
                            pane.add_item(Box::new(view.clone()), true, false, None, window, cx)
                        });
                        view
                    })
                    .collect();
                // With terminals in it, so opening it doesn't start a shell.
                workspace.update(cx, |workspace, cx| {
                    workspace.open_panel::<TerminalPanel>(window, cx)
                });
                TestWorkspace {
                    workspace,
                    terminals,
                }
            })
            .unwrap()
    }

    /// Two workspaces, A and B; B is active.
    async fn two_workspaces(
        cx: &mut TestAppContext,
    ) -> (WindowHandle<MultiWorkspace>, TestWorkspace, TestWorkspace) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project_a = Project::test(fs.clone(), [], cx).await;
        let project_b = Project::test(fs, [], cx).await;
        let window_handle =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project_a.clone(), window, cx));
        let a = add_workspace(window_handle, project_a, true, cx);
        let b = add_workspace(window_handle, project_b, false, cx);
        cx.run_until_parked();
        (window_handle, a, b)
    }

    fn activate(
        window_handle: WindowHandle<MultiWorkspace>,
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) {
        window_handle
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.activate(workspace.clone(), None, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
    }

    fn focused(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| view.focus_handle(cx).contains_focused(window, cx))
    }

    fn finish_unseen(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
        view.update(cx, |view, _| view.set_unseen_finish_for_test("claude"));
    }

    fn has_unseen_finish(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> bool {
        view.read_with(cx, |view, _| view.has_unseen_finish())
    }

    #[gpui::test]
    async fn test_switching_workspace_focuses_its_terminal(cx: &mut TestAppContext) {
        let (window_handle, a, b) = two_workspaces(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        activate(window_handle, &a.workspace, cx);
        assert!(
            focused(&a.terminals[1], cx),
            "A's active terminal has focus"
        );
        finish_unseen(&b.terminals[1], cx);
        activate(window_handle, &b.workspace, cx);
        assert!(
            focused(&b.terminals[1], cx),
            "B's active terminal has focus"
        );
        assert!(
            !has_unseen_finish(&b.terminals[1], cx),
            "and its finished turn is seen"
        );
    }

    #[gpui::test]
    async fn test_switching_workspace_with_terminal_panel_closed(cx: &mut TestAppContext) {
        let (window_handle, a, b) = two_workspaces(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        b.workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_panel::<TerminalPanel>(window, cx)
        });
        activate(window_handle, &a.workspace, cx);
        finish_unseen(&b.terminals[1], cx);
        activate(window_handle, &b.workspace, cx);
        b.workspace.update_in(cx, |workspace, window, cx| {
            assert!(
                workspace
                    .active_pane()
                    .focus_handle(cx)
                    .contains_focused(window, cx),
                "the center pane has focus, as before"
            );
            assert!(
                workspace
                    .all_docks()
                    .iter()
                    .all(|dock| dock.read(cx).visible_panel().is_none()),
                "the terminal panel stays closed"
            );
        });
        assert!(
            has_unseen_finish(&b.terminals[1], cx),
            "the finished turn isn't seen"
        );
    }

    #[gpui::test]
    async fn test_picking_a_terminal_of_another_workspace(cx: &mut TestAppContext) {
        let (window_handle, a, b) = two_workspaces(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        activate(window_handle, &a.workspace, cx);
        // Not B's active terminal.
        let picked = b.terminals[0].clone();
        finish_unseen(&picked, cx);
        // As the workspace tab's menu does: switch to the tab, then show the
        // terminal.
        cx.update(|window, cx| {
            let multi_workspace = window.root::<MultiWorkspace>().flatten().unwrap();
            multi_workspace.update(cx, |multi_workspace, cx| {
                multi_workspace.activate(b.workspace.clone(), None, window, cx)
            });
            agent_activity::activate_terminal(&b.workspace, picked.entity_id(), window, cx);
        });
        cx.run_until_parked();
        assert!(focused(&picked, cx), "the picked terminal has focus");
        assert!(
            !has_unseen_finish(&picked, cx),
            "and its finished turn is seen"
        );
    }

    #[gpui::test]
    async fn test_showing_a_terminal_switches_to_its_workspace(cx: &mut TestAppContext) {
        let (window_handle, a, b) = two_workspaces(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        b.workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_panel::<TerminalPanel>(window, cx)
        });
        activate(window_handle, &a.workspace, cx);
        let shown = b.terminals[0].clone();
        finish_unseen(&shown, cx);
        // As clicking its notification does.
        cx.update(|window, cx| super::show_terminal(&shown, window, cx));
        cx.run_until_parked();
        window_handle
            .read_with(cx, |multi_workspace, _| {
                assert_eq!(multi_workspace.workspace(), &b.workspace)
            })
            .unwrap();
        assert!(focused(&shown, cx), "the terminal has focus");
        assert!(
            !has_unseen_finish(&shown, cx),
            "and its finished turn is seen"
        );
    }

    #[gpui::test]
    async fn test_window_activation_sees_the_focused_terminal(cx: &mut TestAppContext) {
        let (window_handle, _a, b) = two_workspaces(cx).await;
        let cx = &mut VisualTestContext::from_window(window_handle.into(), cx);

        let view = b.terminals[1].clone();
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        cx.run_until_parked();
        assert!(focused(&view, cx));

        cx.deactivate_window();
        finish_unseen(&view, cx);
        cx.run_until_parked();
        assert!(has_unseen_finish(&view, cx));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(
            !has_unseen_finish(&view, cx),
            "coming back to the window sees the finished turn"
        );
    }
}
