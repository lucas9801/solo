use editor::{Editor, EditorEvent};
use gpui::{Action, AppContext, Entity, EventEmitter, FocusHandle, Focusable, Task, actions};

use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use ui::{SharedString, prelude::*};
use workspace::notifications::NotifyResultExt;
use workspace::{Item, Pane, Workspace};

use crate::parser::EditorState;

pub use crate::table_view::{CellSelection, PerformanceMetrics, TableView};

mod parser;
mod renderer;
mod settings;
mod table_data_engine;
mod table_view;
pub mod types;

actions!(tabular_data, [OpenPreview, OpenPreviewToTheSide]);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NavigationDirection {
    #[default]
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Deserialize, PartialEq, JsonSchema, Action)]
#[action(namespace = tabular_data)]
pub struct MoveFocusedCell {
    pub direction: NavigationDirection,
}

/// Editor-backed adapter: watches an [`Editor`], parses its buffer into a [`crate::types::TableLikeContent`],
/// and feeds the result to an embedded [`TableView`] that owns all grid rendering.
pub struct TabularDataPreviewPane {
    /// The reusable tabular viewer this adapter drives.
    pub(crate) table: Entity<TableView>,
    active_editor_state: EditorState,
    pub(crate) parsing_task: Option<Task<anyhow::Result<()>>>,
    pub(crate) is_parsing: bool,
    pub(crate) parse_error: Option<SharedString>,
    /// Time when the last parsing operation ended, used for smart debouncing
    pub(crate) last_parse_end_time: Option<std::time::Instant>,
    /// Forwards the table's notifications so observers of this pane (e.g. `cx.condition` in
    /// tests, or anything watching for the mapping/list-state to settle) see them too.
    _table_subscription: gpui::Subscription,
}

pub use solo_actions::preview::tabular_data::OpenPreviewForPath;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        TabularDataPreviewPane::register(workspace);
    })
    .detach()
}

impl TabularDataPreviewPane {
    pub fn register(workspace: &mut Workspace) {
        workspace.register_action(|workspace, action: &OpenPreviewForPath, window, cx| {
            Self::open_for_path(action.path.clone(), workspace, window, cx);
        });
        workspace.register_action_renderer(|div, _, _, cx| {
            div.on_action(cx.listener(|workspace, _: &OpenPreview, window, cx| {
                if let Some(editor) =
                    Self::resolve_active_item_as_tabular_data_editor(workspace, cx)
                {
                    let pane = workspace.active_pane().clone();
                    Self::open_preview_in_pane(editor, pane, window, cx);
                }
            }))
            .on_action(cx.listener(
                |workspace, _: &OpenPreviewToTheSide, window, cx| {
                    if let Some(editor) =
                        Self::resolve_active_item_as_tabular_data_editor(workspace, cx)
                    {
                        let pane = workspace.active_pane().clone();
                        Self::open_preview_to_the_side_of_pane(workspace, editor, pane, window, cx);
                    }
                },
            ))
        });
    }

    /// Opens the table preview of the file at `path`, which may be outside
    /// the project's folders, in the active pane (or shows the one already
    /// open there).
    pub fn open_for_path(
        path: PathBuf,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let project = workspace.project().clone();
        let open_buffer = project.update(cx, |project, cx| {
            match project.find_project_path(&path, cx) {
                Some(project_path) => project.open_buffer(project_path, cx),
                None => project.open_local_buffer(&path, cx),
            }
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(buffer) = open_buffer
                .await
                .notify_workspace_async_err(workspace.clone(), cx)
            else {
                return;
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    let editor =
                        cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx));
                    let pane = workspace.active_pane().clone();
                    Self::open_preview_in_pane(editor, pane, window, cx);
                })
                .ok();
        })
        .detach();
    }

    pub fn open_preview_in_pane(
        editor: Entity<Editor>,
        pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::activate_or_add_preview(editor, pane, true, window, cx);
    }

    pub fn open_preview_to_the_side_of_pane(
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        origin_pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let target_pane = workspace.adjacent_pane_of(&origin_pane, window, cx);
        Self::activate_or_add_preview(editor.clone(), target_pane, false, window, cx);
        editor.focus_handle(cx).focus(window, cx);
    }

    fn activate_or_add_preview(
        editor: Entity<Editor>,
        pane: Entity<Pane>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let existing_view_idx = Self::find_existing_preview_item_idx(pane.read(cx), &editor, cx);
        if let Some(existing_view_idx) = existing_view_idx {
            pane.update(cx, |pane, cx| {
                pane.activate_item(existing_view_idx, focus, focus, window, cx);
            });
        } else {
            let preview_pane = Self::new(&editor, window, cx);
            pane.update(cx, |pane, cx| {
                pane.add_item(Box::new(preview_pane), focus, focus, None, window, cx);
            });
        }
        cx.notify();
    }

    fn find_existing_preview_item_idx(
        pane: &Pane,
        editor: &Entity<Editor>,
        cx: &App,
    ) -> Option<usize> {
        let buffer_id = editor
            .read(cx)
            .buffer()
            .read(cx)
            .as_singleton()
            .map(|buffer| buffer.entity_id());
        pane.items_of_type::<TabularDataPreviewPane>()
            .find(|view| {
                let shown = &view.read(cx).active_editor_state.editor;
                shown == editor
                    || buffer_id.is_some()
                        && shown
                            .read(cx)
                            .buffer()
                            .read(cx)
                            .as_singleton()
                            .map(|buffer| buffer.entity_id())
                            == buffer_id
            })
            .and_then(|view| pane.index_for_item(&view))
    }

    fn new(editor: &Entity<Editor>, window: &Window, cx: &mut Context<Workspace>) -> Entity<Self> {
        cx.new(|cx| {
            let subscription = cx.subscribe(
                editor,
                |this: &mut TabularDataPreviewPane, _editor, event: &EditorEvent, cx| {
                    match event {
                        EditorEvent::Edited { .. } | EditorEvent::DirtyChanged => {
                            this.parse_from_active_editor(true, cx);
                        }
                        _ => {}
                    };
                },
            );

            let table = cx.new(|cx| TableView::new(window, cx));
            let table_subscription = cx.observe(&table, |_, _, cx| cx.notify());

            let mut view = TabularDataPreviewPane {
                active_editor_state: EditorState {
                    editor: editor.clone(),
                    _subscription: subscription,
                },
                table,
                parsing_task: None,
                is_parsing: false,
                parse_error: None,
                last_parse_end_time: None,
                _table_subscription: table_subscription,
            };

            view.parse_from_active_editor(false, cx);
            view
        })
    }

    pub(crate) fn editor_state(&self) -> &EditorState {
        &self.active_editor_state
    }

    pub fn resolve_active_item_as_tabular_data_editor(
        workspace: &Workspace,
        cx: &mut Context<Workspace>,
    ) -> Option<Entity<Editor>> {
        let editor = workspace
            .active_item(cx)
            .and_then(|item| item.act_as::<Editor>(cx))?;
        Self::is_tabular_data_file(&editor, cx).then_some(editor)
    }

    pub fn is_tabular_data_file(editor: &Entity<Editor>, cx: &App) -> bool {
        parser::TabularFormat::from_editor(editor, cx).is_some()
    }
}

impl Focusable for TabularDataPreviewPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.table.read(cx).focus_handle(cx)
    }
}

impl Render for TabularDataPreviewPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(match &self.parse_error {
            Some(error) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(cx.theme().status().error)
                .child(error.clone())
                .into_any_element(),
            None => self.table.clone().into_any_element(),
        })
    }
}

impl EventEmitter<()> for TabularDataPreviewPane {}

impl Item for TabularDataPreviewPane {
    type Event = ();

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Table))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.editor_state()
            .editor
            .read(cx)
            .buffer()
            .read(cx)
            .as_singleton()
            .and_then(|b| {
                let file = b.read(cx).file()?;
                let local_file = file.as_local()?;
                local_file
                    .abs_path(cx)
                    .file_name()
                    .map(|name| format!("Preview {}", name.to_string_lossy()).into())
            })
            .unwrap_or_else(|| SharedString::from("Tabular Data Preview"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use serde_json::json;
    use std::path::Path;
    use util::path;
    use workspace::AppState;

    #[gpui::test]
    async fn test_detects_tabular_files_outside_the_project(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({ "inside.csv": "a,b\n1,2\n", "inside.jsonl": "{}\n", "inside.txt": "plain" }),
        )
        .await;
        fs.insert_tree(
            path!("/elsewhere"),
            json!({ "outside.csv": "a,b\n1,2\n", "outside.NDJSON": "{}\n", "outside.txt": "plain" }),
        )
        .await;

        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;

        for (abs_path, expected) in [
            (path!("/project/inside.csv"), true),
            (path!("/project/inside.jsonl"), true),
            (path!("/project/inside.txt"), false),
            (path!("/elsewhere/outside.csv"), true),
            (path!("/elsewhere/outside.NDJSON"), true),
            (path!("/elsewhere/outside.txt"), false),
        ] {
            let buffer = project
                .update(cx, |project, cx| project.open_local_buffer(abs_path, cx))
                .await
                .unwrap();
            let (editor, _) = cx.add_window_view(|window, cx| {
                Editor::for_buffer(buffer, Some(project.clone()), window, cx)
            });
            let is_tabular =
                cx.update(|cx| TabularDataPreviewPane::is_tabular_data_file(&editor, cx));
            assert_eq!(is_tabular, expected, "{abs_path}");
        }
    }

    #[gpui::test(iterations = 20)]
    async fn test_csv_preview_mapping_stays_valid_after_deleting_multiline_rows(
        cx: &mut TestAppContext,
    ) {
        use types::DisplayRow;

        init_test(cx);
        let result: anyhow::Result<()> = async {
            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(
                path!("/project"),
                json!({ "records.csv": "id,note\n1,first\n2,\"two\nlines\"\n" }),
            )
            .await;
            let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
            let buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(path!("/project/records.csv"), cx)
                })
                .await?;
            let (workspace, cx) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
            let editor = cx.update(|window, cx| {
                cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx))
            });
            let preview = workspace.update_in(cx, |workspace, window, cx| {
                let pane = workspace.active_pane().clone();
                TabularDataPreviewPane::open_preview_in_pane(
                    editor.clone(),
                    pane.clone(),
                    window,
                    cx,
                );
                pane.read(cx)
                    .items_of_type::<TabularDataPreviewPane>()
                    .next()
            });
            let Some(preview) = preview else {
                anyhow::bail!("preview did not open");
            };
            cx.condition(&preview, |preview, cx| {
                !preview.is_parsing
                    && preview
                        .table
                        .read(cx)
                        .engine
                        .d2d_mapping()
                        .visible_row_count()
                        == 2
            })
            .await;

            let _subscription = cx.update(|_, cx| {
                cx.observe(&preview, |preview, cx| {
                    let preview = preview.read(cx);
                    if !preview.is_parsing && preview.parse_error.is_none() {
                        let table = preview.table.read(cx);
                        let mapping = table.engine.d2d_mapping();
                        assert_eq!(table.list_state.item_count(), mapping.visible_row_count());
                        for display_row in 0..mapping.visible_row_count() {
                            assert!(
                                mapping
                                    .get_data_row(DisplayRow(display_row))
                                    .and_then(|data_row| table.engine.contents.get_row(data_row))
                                    .is_some(),
                                "display mapping points to a deleted row"
                            );
                        }
                    }
                })
            });

            for (text, expected_rows) in [
                (
                    "id,note\n1,first\n2,\"two\nlines\"\n3,\"pasted\ntext\"\n",
                    3,
                ),
                ("id,note\n1,first\n", 1),
                ("id,note\n", 0),
                ("id,note\n1,\"restored\ntext\"\n", 1),
            ] {
                editor.update_in(cx, |editor, window, cx| {
                    editor.set_text(text, window, cx);
                });
                cx.condition(&preview, |preview, cx| {
                    let table = preview.table.read(cx);
                    !preview.is_parsing
                        && table.engine.contents.rows.len() == expected_rows
                        && table.engine.d2d_mapping().visible_row_count() == expected_rows
                })
                .await;
            }
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[gpui::test]
    async fn test_json_lines_preview_recovers_after_edits(cx: &mut TestAppContext) {
        use table_data_engine::sorting_by_column::{AppliedSorting, SortDirection};
        use types::{AnyColumn, DataRow, DisplayRow};

        init_test(cx);
        let result: anyhow::Result<()> = async {
            let fs = FakeFs::new(cx.executor());
            fs.insert_tree(
                path!("/project"),
                json!({ "records.jsonl": "{\"name\":\"Grace\"}\n{\"name\":\"Ada\"}\n" }),
            )
            .await;
            let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
            let buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(path!("/project/records.jsonl"), cx)
                })
                .await?;
            let (workspace, cx) =
                cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
            let editor = cx.update(|window, cx| {
                cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx))
            });
            let preview = workspace.update_in(cx, |workspace, window, cx| {
                let pane = workspace.active_pane().clone();
                TabularDataPreviewPane::open_preview_in_pane(
                    editor.clone(),
                    pane.clone(),
                    window,
                    cx,
                );
                pane.read(cx)
                    .items_of_type::<TabularDataPreviewPane>()
                    .next()
            });
            let Some(preview) = preview else {
                anyhow::bail!("preview did not open");
            };
            cx.condition(&preview, |preview, cx| {
                !preview.is_parsing
                    && preview
                        .table
                        .read(cx)
                        .engine
                        .d2d_mapping()
                        .visible_row_count()
                        == 2
            })
            .await;

            preview.update(cx, |preview, cx| {
                preview.table.update(cx, |table, cx| {
                    table.engine.applied_sorting = Some(AppliedSorting {
                        col_idx: AnyColumn(0),
                        direction: SortDirection::Asc,
                    });
                    table.toggle_filter(AnyColumn(0), Some(r#""Ada""#.into()), cx);
                });
            });
            cx.condition(&preview, |preview, cx| {
                preview
                    .table
                    .read(cx)
                    .engine
                    .d2d_mapping()
                    .visible_row_count()
                    == 1
            })
            .await;
            preview.read_with(cx, |preview, cx| {
                assert_eq!(
                    preview
                        .table
                        .read(cx)
                        .engine
                        .d2d_mapping()
                        .get_data_row(DisplayRow(0)),
                    Some(DataRow(1))
                );
            });

            editor.update_in(cx, |editor, window, cx| {
                editor.set_text("{\"name\":\"Ada\"}\n{", window, cx);
            });
            cx.condition(&preview, |preview, _| {
                !preview.is_parsing && preview.parse_error.is_some()
            })
            .await;
            preview.read_with(cx, |preview, _| {
                assert!(
                    preview
                        .parse_error
                        .as_ref()
                        .is_some_and(|error| error.contains("line 2"))
                );
            });

            editor.update_in(cx, |editor, window, cx| {
                editor.set_text("{\"name\":\"Ada\"}\n{\"name\":\"Grace\"}", window, cx);
            });
            cx.condition(&preview, |preview, cx| {
                !preview.is_parsing
                    && preview.parse_error.is_none()
                    && preview
                        .table
                        .read(cx)
                        .engine
                        .d2d_mapping()
                        .get_data_row(DisplayRow(0))
                        == Some(DataRow(0))
            })
            .await;
            preview.read_with(cx, |preview, cx| {
                let table = preview.table.read(cx);
                assert!(table.engine.has_active_filters(AnyColumn(0)));
                assert!(table.engine.applied_sorting.is_some());
            });

            editor.update_in(cx, |editor, window, cx| {
                editor.set_text(
                    "{\"age\":36,\"name\":\"Ada\"}\n{\"name\":\"Grace\"}",
                    window,
                    cx,
                );
            });
            cx.condition(&preview, |preview, cx| {
                let table = preview.table.read(cx);
                !preview.is_parsing
                    && table.engine.contents.number_of_cols == 2
                    && table.engine.d2d_mapping().visible_row_count() == 2
            })
            .await;
            preview.read_with(cx, |preview, cx| {
                let table = preview.table.read(cx);
                assert!(!table.engine.has_active_filters(AnyColumn(0)));
                assert!(table.engine.applied_sorting.is_none());
                assert!(preview.parse_error.is_none());
            });
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn delimited_extensions_match_the_formats() {
        for extension in solo_actions::preview::tabular_data::DELIMITED_EXTENSIONS {
            let format = parser::TabularFormat::from_extension(extension)
                .unwrap_or_else(|| panic!("{extension} is not read as a table"));
            assert_ne!(format, parser::TabularFormat::JsonLines, "{extension}");
            assert!(
                parser::TabularFormat::from_extension(&extension.to_ascii_uppercase()).is_some(),
                "{extension}"
            );
        }
        // The delimited formats the preview reads are all in the list.
        for (extension, format) in parser::TABULAR_FORMATS {
            if *format != parser::TabularFormat::JsonLines {
                assert!(
                    solo_actions::preview::tabular_data::DELIMITED_EXTENSIONS.contains(extension),
                    "{extension}"
                );
            }
        }
    }

    #[gpui::test]
    async fn test_open_preview_for_a_path_inside_and_outside_the_project(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({ "inside.csv": "a,b\n1,2\n" }))
            .await;
        fs.insert_tree(path!("/elsewhere"), json!({ "outside.tsv": "a\tb\n1\t2\n" }))
            .await;
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let previews = |workspace: &gpui::Entity<Workspace>, cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_pane()
                    .read(cx)
                    .items_of_type::<TabularDataPreviewPane>()
                    .map(|preview| preview.read(cx).tab_content_text(0, cx).to_string())
                    .collect::<Vec<_>>()
            })
        };
        // The action itself reaches the workspace through its rendered
        // element tree, which a test workspace doesn't draw: checked in the app.
        // As the terminal's link menu does it: the action, from the focus
        // the workspace has.
        for (path, name) in [
            (path!("/project/inside.csv"), "Preview inside.csv"),
            (path!("/elsewhere/outside.tsv"), "Preview outside.tsv"),
        ] {
            let before = previews(&workspace, cx).len();
            workspace.update_in(cx, |workspace, window, cx| {
                TabularDataPreviewPane::open_for_path(path.into(), workspace, window, cx)
            });
            cx.run_until_parked();
            let after = previews(&workspace, cx);
            assert_eq!(after.len(), before + 1, "{path}: {after:?}");
            assert!(after.iter().any(|title| title == name), "{path}: {after:?}");
            // Again: switches to the tab that is open.
            workspace.update_in(cx, |workspace, window, cx| {
                TabularDataPreviewPane::open_for_path(path.into(), workspace, window, cx)
            });
            cx.run_until_parked();
            assert_eq!(previews(&workspace, cx).len(), before + 1, "{path}");
        }
        // The table is read from the file.
        let rows = workspace.read_with(cx, |workspace, cx| {
            let preview = workspace
                .active_pane()
                .read(cx)
                .active_item()
                .and_then(|item| item.downcast::<TabularDataPreviewPane>())
                .expect("the preview is the active tab");
            preview.read(cx).table.read(cx).engine.contents.rows.len()
        });
        assert_eq!(rows, 1);
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
        });
    }
}
