//! SOLO: "Run Workflow…" in the terminal column's "+" menu: the saved
//! workflows and the project's tasks in one list. A workflow fills a new
//! terminal's input with its command, its first `{{argument}}` selected; a
//! task runs the way "Spawn Task" runs it.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Render, SharedString, Task, WeakEntity, Window, actions,
};
use picker::{Picker, PickerDelegate};
use project::TaskSourceKind;
use task::{ResolvedTask, RevealStrategy};
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::{ResultExt as _, i18n::tr};
use workspace::{ModalView, Workspace};

use crate::{
    TerminalView, command_blocks::terminal_views, default_working_directory, input,
    terminal_panel::TerminalPanel,
};

actions!(
    terminal_panel,
    [
        /// Lists the saved workflows and the project's tasks, to run one.
        RunWorkflow
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &RunWorkflow, window, cx| {
            RunWorkflowModal::toggle(workspace, window, cx);
        });
    })
    .detach();
}

/// The project's tasks, as "Spawn Task" lists them: the ones run before
/// first.
pub(crate) fn project_tasks(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) -> Task<Vec<(TaskSourceKind, ResolvedTask)>> {
    let Some(inventory) = workspace
        .project()
        .read(cx)
        .task_store()
        .read(cx)
        .task_inventory()
        .cloned()
    else {
        return Task::ready(Vec::new());
    };
    let contexts = tasks_ui::task_contexts(workspace, window, cx);
    cx.spawn(async move |cx| {
        let contexts = Arc::new(contexts.await);
        let listed = inventory.update(cx, |inventory, cx| {
            inventory.used_and_current_resolved_tasks(contexts, cx)
        });
        let (used, current) = listed.await;
        used.into_iter().chain(current).collect()
    })
}

/// Runs a project task the way "Spawn Task" does.
pub(crate) fn run_project_task(
    workspace: &WeakEntity<Workspace>,
    kind: TaskSourceKind,
    task: ResolvedTask,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            workspace.schedule_resolved_task(kind, task, false, window, cx)
        })
        .log_err();
}

/// Opens a new terminal in the terminal column with `command` in its input,
/// its first `{{argument}}` selected, once its shell shows its prompt.
pub(crate) fn run_workflow_in_new_terminal(
    workspace: &Entity<Workspace>,
    command: String,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(panel) = workspace.read(cx).panel::<TerminalPanel>(cx) else {
        return;
    };
    let directory = default_working_directory(workspace.read(cx), cx);
    let terminal = panel.update(cx, |panel, cx| {
        panel.add_terminal_shell(false, directory, RevealStrategy::Always, window, cx)
    });
    let workspace = workspace.downgrade();
    window
        .spawn(cx, async move |cx| {
            let terminal = terminal.await?;
            cx.update(|_, cx| {
                let (Some(workspace), Some(terminal)) = (workspace.upgrade(), terminal.upgrade())
                else {
                    return;
                };
                if let Some(view) = terminal_views(&workspace, cx)
                    .into_iter()
                    .find(|view| view.read(cx).terminal() == &terminal)
                {
                    view.update(cx, |view, cx| view.fill_input_when_ready(command, cx));
                }
            })
        })
        .detach_and_log_err(cx);
}

#[derive(Clone)]
enum Item {
    Workflow(input::workflows::Workflow),
    Task(TaskSourceKind, ResolvedTask),
    EditWorkflows,
}

impl Item {
    fn name(&self) -> SharedString {
        match self {
            Item::Workflow(workflow) => workflow.name.clone().into(),
            Item::Task(_, task) => task.resolved_label.clone().into(),
            Item::EditWorkflows => tr("Edit Workflows…", "编辑工作流…").into(),
        }
    }

    fn command(&self) -> Option<SharedString> {
        match self {
            Item::Workflow(workflow) => Some(workflow.command.clone().into()),
            Item::Task(_, task) => Some(task.resolved.command_label.clone().into()),
            Item::EditWorkflows => None,
        }
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.name().to_lowercase().contains(&query)
            || self
                .command()
                .is_some_and(|command| command.to_lowercase().contains(&query))
    }
}

enum Row {
    Header(SharedString),
    Item(usize),
}

pub struct RunWorkflowModal {
    picker: Entity<Picker<RunWorkflowDelegate>>,
}

impl RunWorkflowModal {
    pub(crate) fn toggle(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let shell = workspace
            .active_item(cx)
            .and_then(|item| item.downcast::<TerminalView>())
            .or_else(|| {
                workspace.panel::<TerminalPanel>(cx).and_then(|panel| {
                    panel
                        .read(cx)
                        .active_pane
                        .read(cx)
                        .active_item()?
                        .downcast::<TerminalView>()
                })
            })
            .and_then(|view| {
                view.read(cx)
                    .terminal()
                    .read(cx)
                    .shell_integration()
                    .shell()
                    .map(str::to_string)
            });
        let workflows = input::workflows::load_for_shell(shell.as_deref());
        let tasks = project_tasks(workspace, window, cx);
        let weak_workspace = workspace.weak_handle();
        workspace.toggle_modal(window, cx, move |window, cx| {
            Self::new(weak_workspace, workflows, tasks, window, cx)
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        workflows: Vec<input::workflows::Workflow>,
        tasks: Task<Vec<(TaskSourceKind, ResolvedTask)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut delegate = RunWorkflowDelegate {
            modal: cx.entity().downgrade(),
            workspace,
            workflows,
            tasks: Vec::new(),
            rows: Vec::new(),
            selected_index: 0,
            query: String::new(),
        };
        delegate.update_rows();
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        cx.spawn_in(window, async move |this, cx| {
            let tasks = tasks.await;
            this.update_in(cx, |this, window, cx| {
                this.picker.update(cx, |picker, cx| {
                    picker.delegate.tasks = tasks;
                    let query = picker.query(cx);
                    picker.update_matches(query, window, cx);
                });
            })
            .log_err();
        })
        .detach();
        Self { picker }
    }

    #[cfg(test)]
    pub(crate) fn picker(&self) -> &Entity<Picker<RunWorkflowDelegate>> {
        &self.picker
    }
}

impl Render for RunWorkflowModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RunWorkflow")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

impl Focusable for RunWorkflowModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for RunWorkflowModal {}
impl ModalView for RunWorkflowModal {}

pub struct RunWorkflowDelegate {
    modal: WeakEntity<RunWorkflowModal>,
    workspace: WeakEntity<Workspace>,
    workflows: Vec<input::workflows::Workflow>,
    tasks: Vec<(TaskSourceKind, ResolvedTask)>,
    rows: Vec<Row>,
    selected_index: usize,
    query: String,
}

impl RunWorkflowDelegate {
    fn items(&self) -> Vec<Item> {
        self.workflows
            .iter()
            .cloned()
            .map(Item::Workflow)
            .chain(
                self.tasks
                    .iter()
                    .map(|(kind, task)| Item::Task(kind.clone(), task.clone())),
            )
            .chain([Item::EditWorkflows])
            .collect()
    }

    /// The rows for the query: "Workflows", then "Project Tasks" (each only
    /// with matches), then "Edit Workflows…".
    fn update_rows(&mut self) {
        let items = self.items();
        let mut rows = Vec::new();
        let workflows = self.workflows.len();
        let tasks = workflows + self.tasks.len();
        for (header, range) in [
            (tr("Workflows", "工作流"), 0..workflows),
            (tr("Project Tasks", "项目任务"), workflows..tasks),
        ] {
            let matching: Vec<usize> = range.filter(|&ix| items[ix].matches(&self.query)).collect();
            if !matching.is_empty() {
                rows.push(Row::Header(header.into()));
                rows.extend(matching.into_iter().map(Row::Item));
            }
        }
        rows.push(Row::Item(tasks));
        self.rows = rows;
        self.selected_index = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Item(_)))
            .unwrap_or(0);
    }

    fn item(&self, ix: usize) -> Option<Item> {
        match self.rows.get(ix)? {
            Row::Item(item) => {
                let workflows = self.workflows.len();
                let tasks = workflows + self.tasks.len();
                Some(if *item < workflows {
                    Item::Workflow(self.workflows[*item].clone())
                } else if *item < tasks {
                    let (kind, task) = &self.tasks[*item - workflows];
                    Item::Task(kind.clone(), task.clone())
                } else {
                    Item::EditWorkflows
                })
            }
            Row::Header(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn row_labels(&self) -> Vec<String> {
        (0..self.rows.len())
            .map(|ix| match &self.rows[ix] {
                Row::Header(header) => format!("# {header}"),
                Row::Item(_) => self
                    .item(ix)
                    .map(|item| item.name().to_string())
                    .unwrap_or_default(),
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn select_named(&mut self, name: &str) {
        if let Some(ix) = (0..self.rows.len()).find(|&ix| {
            self.item(ix)
                .is_some_and(|item| item.name().as_ref() == name)
        }) {
            self.selected_index = ix;
        }
    }
}

impl PickerDelegate for RunWorkflowDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "run workflow"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        tr("Search", "搜索").into()
    }

    fn match_count(&self) -> usize {
        self.rows.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn can_select(&self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) -> bool {
        matches!(self.rows.get(ix), Some(Row::Item(_)))
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        if self.rows.len() > 1 {
            vec![self.rows.len() - 2]
        } else {
            Vec::new()
        }
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.query = query;
        self.update_rows();
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(item) = self.item(self.selected_index) else {
            return;
        };
        self.dismissed(window, cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        match item {
            Item::Workflow(workflow) => {
                run_workflow_in_new_terminal(&workspace, workflow.command, window, cx)
            }
            Item::Task(kind, task) => {
                run_project_task(&self.workspace, kind, task, window, cx);
            }
            Item::EditWorkflows => workspace.update(cx, |workspace, cx| {
                crate::open_workflows_file(workspace, &input::OpenWorkflowsFile, window, cx)
            }),
        }
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        if let Row::Header(header) = self.rows.get(ix)? {
            return Some(
                h_flex()
                    .px_2()
                    .pt_1()
                    .child(
                        Label::new(header.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
        }
        let item = self.item(ix)?;
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .w_full()
                        .gap_4()
                        .justify_between()
                        .child(Label::new(item.name()).truncate())
                        .when_some(item.command(), |this, command| {
                            this.child(
                                div().min_w_0().child(
                                    Label::new(command)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .buffer_font(cx)
                                        .truncate(),
                                ),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use settings::SettingsStore;
    use util::path;

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    fn open_modal(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<Picker<RunWorkflowDelegate>> {
        workspace.update_in(cx, |workspace, window, cx| {
            RunWorkflowModal::toggle(workspace, window, cx)
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_modal::<RunWorkflowModal>(cx)
                .expect("the Run Workflow list")
                .read(cx)
                .picker()
                .clone()
        })
    }

    // SOLO: "Run Workflow…" lists the workflows, then the project's
    // tasks, then "Edit Workflows…"; a task picked there runs as Spawn Task
    // runs it.
    #[gpui::test]
    async fn test_run_workflow_lists_workflows_and_tasks(cx: &mut TestAppContext) {
        init(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            serde_json::json!({
                ".solo": {
                    "tasks.json": r#"[{ "label": "build", "command": "cargo build" }]"#,
                },
                "a.txt": "a",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.run_until_parked();

        let picker = open_modal(&workspace, cx);
        let labels = picker.read_with(cx, |picker, _| picker.delegate.row_labels());
        let workflows = labels
            .iter()
            .position(|label| label == "# Workflows")
            .expect("the workflows group");
        let tasks = labels
            .iter()
            .position(|label| label == "# Project Tasks")
            .expect("the project tasks group");
        assert!(workflows < tasks);
        assert!(labels.contains(&"Undo the last git commit, keeping its changes".to_string()));
        assert_eq!(labels[tasks + 1], "build");
        assert_eq!(labels.last().map(String::as_str), Some("Edit Workflows…"));

        // The search narrows both groups; one with nothing left goes.
        picker.update_in(cx, |picker, window, cx| {
            picker.set_query("cargo", window, cx)
        });
        cx.run_until_parked();
        let labels = picker.read_with(cx, |picker, _| picker.delegate.row_labels());
        assert_eq!(labels, vec!["# Project Tasks", "build", "Edit Workflows…"]);

        // Picking the task runs it.
        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.select_named("build");
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();
        let scheduled = project.read_with(cx, |project, cx| {
            project
                .task_store()
                .read(cx)
                .task_inventory()
                .and_then(|inventory| inventory.read(cx).last_scheduled_task(None))
                .map(|(_, task)| task.resolved_label)
        });
        assert_eq!(scheduled.as_deref(), Some("build"));
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.active_modal::<RunWorkflowModal>(cx).is_none());
        });
    }

    // SOLO: with no project tasks there's no "Project Tasks" group.
    #[gpui::test]
    async fn test_run_workflow_without_tasks(cx: &mut TestAppContext) {
        init(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), serde_json::json!({ "a.txt": "a" }))
            .await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        cx.run_until_parked();
        let picker = open_modal(&workspace, cx);
        let labels = picker.read_with(cx, |picker, _| picker.delegate.row_labels());
        assert_eq!(labels.first().map(String::as_str), Some("# Workflows"));
        assert!(!labels.contains(&"# Project Tasks".to_string()));
    }
}
