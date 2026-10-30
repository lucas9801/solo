//! SOLO: opens files that change on disk while a command runs in the focused
//! terminal, so edits made by an AI agent in the terminal show up in the editor.

use gpui::{Context, Entity, TaskExt as _, Window};
use project::{PathChange, Project, ProjectPath, UpdatedEntriesSet, WorktreeId};
use settings::Settings;
use terminal::terminal_settings::TerminalSettings;

use super::TerminalPanel;
use crate::TerminalView;

/// More changes than this at once are a bulk operation (a checkout, a code
/// generator), not edits worth following.
const MAX_CHANGES: usize = 20;
/// Files opened per update; each one replaces the preview tab anyway.
const MAX_OPENED: usize = 3;

impl TerminalPanel {
    pub(super) fn handle_project_event(
        &mut self,
        project: &Entity<Project>,
        event: &project::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project::Event::WorktreeUpdatedEntries(worktree_id, changes) = event else {
            return;
        };
        if !TerminalSettings::get_global(cx).open_changed_files
            || changes.len() > MAX_CHANGES
            || !self.focused_terminal_runs_command(window, cx)
        {
            return;
        }
        let paths = changed_files(project, *worktree_id, changes, cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            for path in paths {
                workspace
                    .open_path_preview(path, None, false, true, true, window, cx)
                    .detach_and_log_err(cx);
            }
        });
    }

    fn focused_terminal_runs_command(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.center.panes().into_iter().any(|pane| {
            let pane = pane.read(cx);
            pane.has_focus(window, cx)
                && pane
                    .active_item()
                    .and_then(|item| item.downcast::<TerminalView>())
                    .is_some_and(|view| {
                        view.read(cx)
                            .terminal()
                            .read(cx)
                            .shell_integration()
                            .blocks()
                            .last()
                            .is_some_and(|block| block.is_running())
                    })
        })
    }
}

fn changed_files(
    project: &Entity<Project>,
    worktree_id: WorktreeId,
    changes: &UpdatedEntriesSet,
    cx: &Context<TerminalPanel>,
) -> Vec<ProjectPath> {
    let Some(worktree) = project.read(cx).worktree_for_id(worktree_id, cx) else {
        return Vec::new();
    };
    let worktree = worktree.read(cx);
    changes
        .iter()
        .filter(|(_, _, change)| {
            matches!(
                change,
                PathChange::Added | PathChange::Updated | PathChange::AddedOrUpdated
            )
        })
        .filter_map(|(path, entry_id, _)| {
            let entry = worktree.entry_for_id(*entry_id)?;
            (entry.is_file() && !entry.is_ignored && !entry.is_hidden && !entry.is_private).then(
                || ProjectPath {
                    worktree_id,
                    path: path.clone(),
                },
            )
        })
        .take(MAX_OPENED)
        .collect()
}
