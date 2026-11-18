use std::{ops::Range, path::Path, sync::Arc};

use gpui::{
    ClipboardItem, CursorStyle, EventEmitter, FocusHandle, Focusable, HighlightStyle, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, Point, StyledText, TextLayout, actions,
};
use solo_actions::workspace::OpenWithSystem;
use theme::ActiveTheme as _;
use ui::{
    App, Button, ButtonCommon, ButtonStyle, Clickable, Context, FluentBuilder, InteractiveElement,
    KeyBinding, Label, LabelCommon, LabelSize, ParentElement, Render, SharedString, Styled as _,
    StyledTypography as _, TintColor, Window, div, h_flex, v_flex,
};

use crate::Item;

// SOLO: copies the selected part of the file's path (see `InvalidItemView`).
actions!(
    invalid_item,
    [
        /// Copies the selected part of the path of the file that could not be opened.
        Copy
    ]
);

/// A view to display when a certain buffer/image/other item fails to open.
pub struct InvalidItemView {
    /// Which path was attempted to open.
    pub abs_path: Arc<Path>,
    /// An error message, happened when opening the item.
    pub error: SharedString,
    is_local: bool,
    focus_handle: FocusHandle,
    // SOLO: the file's full path is shown under the error, middle-elided when
    // it doesn't fit, and can be selected with the mouse and copied.
    path_text: SharedString,
    path_selection: Option<PathSelection>,
    /// The path's text as laid out in the last frame, for hit testing.
    path_layout: Option<TextLayout>,
}

impl std::fmt::Debug for InvalidItemView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvalidItemView")
            .field("abs_path", &self.abs_path)
            .field("error", &self.error)
            .field("is_local", &self.is_local)
            .field("path_selection", &self.path_selection)
            .finish_non_exhaustive()
    }
}

/// SOLO: a selection of the path, as byte offsets into the full path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PathSelection {
    anchor: usize,
    head: usize,
    dragging: bool,
}

impl PathSelection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

impl InvalidItemView {
    pub fn new(
        abs_path: &Path,
        is_local: bool,
        e: &anyhow::Error,
        _: &mut Window,
        cx: &mut App,
    ) -> Self {
        Self {
            is_local,
            abs_path: Arc::from(abs_path),
            error: format!("{}", e.root_cause()).into(),
            focus_handle: cx.focus_handle(),
            path_text: SharedString::new(abs_path.to_string_lossy()),
            path_selection: None,
            path_layout: None,
        }
    }

    /// SOLO: the selected part of the shown path, if any.
    pub fn selected_path_text(&self) -> Option<&str> {
        let range = self.path_selection?.range();
        (!range.is_empty()).then(|| &self.path_text[range])
    }

    /// The offset in the full path under `position`, from the last frame's
    /// layout, which may show the path middle-elided.
    fn path_index_for_position(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.path_layout.as_ref()?;
        let displayed_index = layout
            .index_for_position(position)
            .unwrap_or_else(|closest| closest);
        Some(full_index_for_displayed_index(
            &self.path_text,
            &layout.text(),
            displayed_index,
        ))
    }

    fn path_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus_handle, cx);
        if event.click_count >= 2 {
            self.path_selection = Some(PathSelection {
                anchor: 0,
                head: self.path_text.len(),
                dragging: false,
            });
        } else if let Some(index) = self.path_index_for_position(event.position) {
            self.path_selection = Some(PathSelection {
                anchor: index,
                head: index,
                dragging: true,
            });
        }
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.path_selection.filter(|selection| selection.dragging) else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_path_drag();
            return;
        }
        if let Some(index) = self.path_index_for_position(event.position)
            && index != selection.head
        {
            self.path_selection = Some(PathSelection {
                head: index,
                ..selection
            });
            cx.notify();
        }
    }

    fn end_path_drag(&mut self) {
        if let Some(selection) = self.path_selection.as_mut() {
            selection.dragging = false;
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        match self.selected_path_text() {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text.to_string())),
            None => cx.propagate(),
        }
    }
}

/// SOLO: maps an offset in the path as shown, which middle elision may have
/// turned into `prefix…suffix`, to the same place in the full path. An offset
/// inside the ellipsis lands after the hidden part, so a selection that
/// reaches into the ellipsis includes it.
fn full_index_for_displayed_index(full: &str, displayed: &str, displayed_index: usize) -> usize {
    let displayed_index = displayed_index.min(displayed.len());
    if full == displayed {
        return displayed_index;
    }
    let prefix_len: usize = full
        .chars()
        .zip(displayed.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let suffix_len: usize = full[prefix_len..]
        .chars()
        .rev()
        .zip(displayed[prefix_len..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    if displayed_index <= prefix_len {
        displayed_index
    } else {
        let hidden_end = displayed.len() - suffix_len;
        full.len() - (displayed.len() - displayed_index.max(hidden_end))
    }
}

impl Item for InvalidItemView {
    type Event = ();

    fn tab_content_text(&self, mut detail: usize, _: &App) -> SharedString {
        // Ensure we always render at least the filename.
        detail += 1;

        let path = self.abs_path.as_ref();

        let mut prefix = path;
        while detail > 0 {
            if let Some(parent) = prefix.parent() {
                prefix = parent;
                detail -= 1;
            } else {
                break;
            }
        }

        let path = if detail > 0 {
            path
        } else {
            path.strip_prefix(prefix).unwrap_or(path)
        };

        SharedString::new(path.to_string_lossy())
    }
}

impl EventEmitter<()> for InvalidItemView {}

impl Focusable for InvalidItemView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for InvalidItemView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let abs_path = self.abs_path.clone();

        // SOLO: the full path in the buffer font, middle-elided, selectable.
        let selection_background = cx.theme().colors().element_selection_background;
        let selection = self
            .path_selection
            .map(|selection| selection.range())
            .filter(|range| !range.is_empty())
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        background_color: Some(selection_background),
                        ..Default::default()
                    },
                )
            });
        let path_text = StyledText::new(self.path_text.clone()).with_highlights(selection);
        self.path_layout = Some(path_text.layout().clone());
        let path = div()
            .id("invalid-item-path")
            .debug_selector(|| "invalid-item-path".into())
            .min_w_0()
            .max_w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_middle()
            .font_buffer(cx)
            .text_ui_sm(cx)
            .text_color(cx.theme().colors().text_muted)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::path_mouse_down))
            .child(path_text);

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .justify_center()
            .overflow_hidden()
            .key_context("InvalidItem")
            .on_action(cx.listener(Self::copy))
            // SOLO: a click elsewhere drops the path's selection; a drag that
            // started on the path keeps extending it anywhere in the view.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.path_selection.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_path_drag()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_path_drag()),
            )
            .child(
                h_flex().size_full().justify_center().px_8().child(
                    v_flex()
                        .min_w_0()
                        .max_w_full()
                        .justify_center()
                        .items_center()
                        .gap_2()
                        // SOLO: a `Label`, so the title is shown in the interface language.
                        .child(Label::new("Could not open file"))
                        .child(Label::new(self.error.clone()).size(LabelSize::Small))
                        .child(path)
                        .when(self.is_local, |contents| {
                            let reveal_path = abs_path.clone();
                            contents.child(
                                h_flex()
                                    .mt_1()
                                    .justify_center()
                                    .gap_2()
                                    // SOLO: shows the file selected in the system's
                                    // file manager, for files Solo can't open.
                                    .child(
                                        div()
                                            .debug_selector(|| "reveal-in-file-manager".into())
                                            .child(
                                                Button::new(
                                                    "reveal-in-file-manager",
                                                    ui::utils::reveal_in_file_manager_label(false),
                                                )
                                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                                .on_click(move |_, _, cx| {
                                                    cx.reveal_path(&reveal_path);
                                                }),
                                            ),
                                    )
                                    .child(
                                        div().debug_selector(|| "open-with-system".into()).child(
                                            Button::new("open-with-system", "Open in Default App")
                                                .on_click(move |_, _, cx| {
                                                    cx.open_with_system(&abs_path);
                                                })
                                                .style(ButtonStyle::Outlined)
                                                .key_binding(KeyBinding::for_action(
                                                    &OpenWithSystem,
                                                    cx,
                                                )),
                                        ),
                                    ),
                            )
                        }),
                ),
            )
    }
}

// SOLO: the "Could not open file" page's path and buttons.
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fs::FakeFs;
    use gpui::{Entity, IntoElement, Modifiers, TestAppContext, VisualTestContext, point, px};
    use project::Project;
    use serde_json::json;
    use util::path;

    use super::*;
    use crate::{
        ItemHandle, OpenOptions, Pane, Workspace, item::ProjectItem, register_project_item,
        tests::init_test,
    };

    /// Opens text buffers, like the editor, and shows the page for the files
    /// it can't open.
    struct TestTextView {
        focus_handle: FocusHandle,
    }

    impl Item for TestTextView {
        type Event = ();

        fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
            "text".into()
        }
    }

    impl EventEmitter<()> for TestTextView {}

    impl Focusable for TestTextView {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for TestTextView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            div().size_full().into_element()
        }
    }

    impl ProjectItem for TestTextView {
        type Item = language::Buffer;

        fn for_project_item(
            _: Entity<Project>,
            _: Option<&Pane>,
            _: Entity<language::Buffer>,
            _: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            Self {
                focus_handle: cx.focus_handle(),
            }
        }

        fn for_broken_project_item(
            abs_path: &Path,
            is_local: bool,
            e: &anyhow::Error,
            window: &mut Window,
            cx: &mut App,
        ) -> Option<InvalidItemView> {
            Some(InvalidItemView::new(abs_path, is_local, e, window, cx))
        }
    }

    const APK: &str = path!("/root/build/PerfCmp.apk");

    async fn open_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        init_test(cx);
        cx.update(register_project_item::<TestTextView>);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({ "notes.txt": "hello\n", "build": {} }),
        )
        .await;
        fs.insert_file(
            APK,
            b"PK\x03\x04\x14\0\0\0\x08\0classes.dex\0\0\x01\x02".to_vec(),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        cx.run_until_parked();
        (workspace, cx)
    }

    fn open(
        workspace: &Entity<Workspace>,
        path: &str,
        cx: &mut VisualTestContext,
    ) -> gpui::Task<anyhow::Result<Box<dyn ItemHandle>>> {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_abs_path(PathBuf::from(path), OpenOptions::default(), window, cx)
        })
    }

    #[gpui::test]
    async fn test_binary_file_shows_its_path_and_both_buttons(cx: &mut TestAppContext) {
        let (workspace, cx) = open_workspace(cx).await;

        // A file Solo can open is unaffected.
        let item = open(&workspace, path!("/root/notes.txt"), cx)
            .await
            .unwrap();
        assert!(item.downcast::<TestTextView>().is_some());

        let item = open(&workspace, APK, cx).await.unwrap();
        let view = item
            .downcast::<InvalidItemView>()
            .expect("a binary file should open the \"Could not open file\" page");
        view.read_with(cx, |view, _| {
            assert_eq!(view.abs_path.as_ref(), Path::new(APK));
            assert_eq!(view.error.as_ref(), "Binary files are not supported");
            assert_eq!(view.path_text.as_ref(), APK);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("invalid-item-path").is_some());
        let reveal = cx
            .debug_bounds("reveal-in-file-manager")
            .expect("local files have the Reveal button");
        let open_with_system = cx
            .debug_bounds("open-with-system")
            .expect("local files keep the Open in Default App button");
        assert!(
            reveal.origin.x < open_with_system.origin.x,
            "Reveal comes first"
        );
        assert!(cx.revealed_paths().is_empty());
        assert!(
            cx.opened_with_system().is_empty(),
            "nothing is opened with the system without a click"
        );

        cx.simulate_click(reveal.center(), Modifiers::none());
        assert_eq!(cx.revealed_paths(), vec![PathBuf::from(APK)]);
        assert!(cx.opened_with_system().is_empty());

        cx.simulate_click(open_with_system.center(), Modifiers::none());
        assert_eq!(cx.opened_with_system(), vec![PathBuf::from(APK)]);
        assert_eq!(cx.revealed_paths().len(), 1);
    }

    #[gpui::test]
    async fn test_remote_file_has_no_buttons(cx: &mut TestAppContext) {
        init_test(cx);
        let path = Path::new("/home/me/app.apk");
        let (view, cx) = cx.add_window_view(|window, cx| {
            InvalidItemView::new(
                path,
                false,
                &anyhow::anyhow!("Binary files are not supported"),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(!view.is_local));

        assert!(cx.debug_bounds("invalid-item-path").is_some());
        assert!(cx.debug_bounds("reveal-in-file-manager").is_none());
        assert!(cx.debug_bounds("open-with-system").is_none());
    }

    #[gpui::test]
    async fn test_path_can_be_selected_and_copied(cx: &mut TestAppContext) {
        init_test(cx);
        let path = PathBuf::from(APK);
        let (view, cx) = cx.add_window_view(|window, cx| {
            InvalidItemView::new(&path, true, &anyhow::anyhow!("nope"), window, cx)
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("invalid-item-path").unwrap();

        // Dragging across the whole path, and past its end, selects all of it.
        let start = point(bounds.left() + px(0.5), bounds.center().y);
        let end = point(bounds.right() + px(40.), bounds.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_path_text(), Some(APK));
        });

        cx.dispatch_action(Copy);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(APK.to_string())
        );

        // A click elsewhere drops the selection.
        cx.simulate_click(point(px(2.), px(2.)), Modifiers::none());
        view.read_with(cx, |view, _| assert_eq!(view.selected_path_text(), None));

        // A double click selects the whole path.
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: bounds.center(),
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_path_text(), Some(APK));
        });
    }

    #[test]
    fn test_full_index_for_middle_elided_path() {
        let full = r"G:\work\perfcmp_android\build\PerfCmp.apk";
        // Not elided: the same offsets.
        assert_eq!(full_index_for_displayed_index(full, full, 0), 0);
        assert_eq!(full_index_for_displayed_index(full, full, 12), 12);
        assert_eq!(full_index_for_displayed_index(full, full, 999), full.len());

        let displayed = "G:\\work\\\u{2026}\\PerfCmp.apk";
        let ellipsis = displayed.find('\u{2026}').unwrap();
        // The kept beginning maps to itself.
        assert_eq!(full_index_for_displayed_index(full, displayed, 3), 3);
        assert_eq!(
            full_index_for_displayed_index(full, displayed, ellipsis),
            ellipsis
        );
        // Inside the ellipsis or after it: past the hidden part.
        let suffix = r"\PerfCmp.apk";
        let hidden_end = full.len() - suffix.len();
        assert_eq!(
            full_index_for_displayed_index(full, displayed, ellipsis + 1),
            hidden_end
        );
        assert_eq!(
            full_index_for_displayed_index(full, displayed, displayed.len() - suffix.len()),
            hidden_end
        );
        assert_eq!(
            full_index_for_displayed_index(full, displayed, displayed.len()),
            full.len()
        );
    }
}
