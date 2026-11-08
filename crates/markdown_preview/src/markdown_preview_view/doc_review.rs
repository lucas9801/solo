//! SOLO: reviewing a document an agent wrote, in its preview: comments on
//! the rendered text, sent to the agent as one message, and what changed in
//! the document since it was last read.

use std::ops::Range;
use std::path::PathBuf;

use editor::Editor;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, Hsla, WeakEntity, Window,
};
use language::{Buffer, BufferSnapshot, Point, ToOffset as _, ToPoint as _};
use ui::{IconButton, IconName, Tooltip, prelude::*};
use util::i18n::{is_chinese, tr};
use zed_actions::agent_terminal::{DocumentComment, SendDocumentCommentsToAgent};

use super::MarkdownPreviewView;

/// How much of the commented text a comment quotes.
const QUOTE_CHARS: usize = 80;

#[derive(Default)]
pub(super) struct DocReview {
    /// The document the comments and the read version are of.
    buffer: Option<WeakEntity<Buffer>>,
    comments: Vec<DocComment>,
    draft: Option<Draft>,
    /// The document as last read: what changed since is offered.
    read: Option<String>,
    /// What changed since it was read, if anything.
    changes: Option<Changes>,
    show_changes: bool,
}

struct DocComment {
    range: Range<language::Anchor>,
    quote: String,
    text: String,
}

/// A comment being written.
struct Draft {
    range: Range<language::Anchor>,
    quote: String,
    editor: Entity<Editor>,
    /// The comment it edits, if not a new one.
    editing: Option<usize>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Changes {
    /// The lines added or changed, as source ranges of the document now.
    pub changed: Vec<Range<usize>>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// The lines of `now` that differ from `before`, and how many were added and
/// removed; `None` if nothing differs.
pub(super) fn changes_since(before: &str, now: &str) -> Option<Changes> {
    if before == now {
        return None;
    }
    let diff = similar::TextDiff::from_lines(before, now);
    let mut changed: Vec<Range<usize>> = Vec::new();
    let mut added_lines = 0;
    let mut removed_lines = 0;
    // Byte offset in `now` of each line, to turn line numbers into ranges.
    let mut line_starts = vec![0];
    line_starts.extend(now.match_indices('\n').map(|(ix, _)| ix + 1));
    let offset_of_line = |line: usize| line_starts.get(line).copied().unwrap_or(now.len());
    for op in diff.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Delete {
                old_len, new_index, ..
            } => {
                removed_lines += old_len;
                // Marks where the lines went.
                let at = offset_of_line(new_index);
                changed.push(at..at);
            }
            similar::DiffOp::Insert {
                new_index, new_len, ..
            } => {
                added_lines += new_len;
                changed.push(offset_of_line(new_index)..offset_of_line(new_index + new_len));
            }
            similar::DiffOp::Replace {
                old_len,
                new_index,
                new_len,
                ..
            } => {
                removed_lines += old_len;
                added_lines += new_len;
                changed.push(offset_of_line(new_index)..offset_of_line(new_index + new_len));
            }
        }
    }
    Some(Changes {
        changed,
        added_lines,
        removed_lines,
    })
}

/// The text a comment is on, on one line and shortened.
pub(super) fn quote(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= QUOTE_CHARS {
        return text;
    }
    let mut quote: String = text.chars().take(QUOTE_CHARS).collect();
    quote.push('…');
    quote
}

/// The 1-based lines `range` covers; a range ending at a line's start doesn't
/// cover that line.
pub(super) fn lines_of(buffer: &BufferSnapshot, range: &Range<language::Anchor>) -> (u32, u32) {
    let start = range.start.to_point(buffer);
    let mut end = range.end.to_point(buffer);
    if end.column == 0 && end.row > start.row {
        end = Point::new(end.row - 1, 0);
    }
    (start.row + 1, end.row + 1)
}

impl MarkdownPreviewView {
    fn source_buffer(&self, cx: &App) -> Option<Entity<Buffer>> {
        self.active_editor
            .as_ref()?
            .editor
            .read(cx)
            .buffer()
            .read(cx)
            .as_singleton()
    }

    /// The preview now shows `contents` of its document.
    pub(super) fn doc_review_source_updated(&mut self, contents: &str, cx: &mut Context<Self>) {
        let buffer = self.source_buffer(cx);
        let same_document = match (&self.doc_review.buffer, &buffer) {
            (Some(previous), Some(buffer)) => previous == &buffer.downgrade(),
            _ => false,
        };
        if !same_document {
            // Another document: what was written and read was of the old one.
            self.doc_review = DocReview {
                buffer: buffer.map(|buffer| buffer.downgrade()),
                ..DocReview::default()
            };
        }
        let review = &mut self.doc_review;
        match &review.read {
            None => review.read = Some(contents.to_string()),
            Some(read) => {
                review.changes = changes_since(read, contents);
                if review.changes.is_none() {
                    review.show_changes = false;
                }
            }
        }
        self.update_doc_marks(cx);
    }

    /// Colors the gutter of what has comments and, when shown, what changed.
    fn update_doc_marks(&mut self, cx: &mut Context<Self>) {
        let mut marks: Vec<(Range<usize>, Hsla)> = Vec::new();
        let source_len = self.markdown.read(cx).source().len();
        if self.doc_review.show_changes
            && let Some(changes) = &self.doc_review.changes
        {
            let color = cx.theme().colors().version_control_added;
            marks.extend(changes.changed.iter().map(|range| {
                (
                    range.start.min(source_len)..range.end.min(source_len),
                    color,
                )
            }));
        }
        if let Some(buffer) = self.source_buffer(cx) {
            let buffer = buffer.read(cx).snapshot();
            let color = cx.theme().colors().text_accent;
            for comment in &self.doc_review.comments {
                let range =
                    comment.range.start.to_offset(&buffer)..comment.range.end.to_offset(&buffer);
                marks.push((
                    range.start.min(source_len)..range.end.min(source_len),
                    color,
                ));
            }
        }
        self.markdown.update(cx, |markdown, cx| {
            markdown.set_marked_source_ranges(marks, cx)
        });
    }

    /// Starts a comment on the source `range` of the document.
    pub(super) fn start_comment(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(buffer) = self.source_buffer(cx) else {
            return;
        };
        let snapshot = buffer.read(cx).snapshot();
        if range.end > snapshot.len() || range.is_empty() {
            return;
        }
        let quote = quote(&snapshot.text_for_range(range.clone()).collect::<String>());
        let range = snapshot.anchor_before(range.start)..snapshot.anchor_after(range.end);
        self.open_draft(range, quote, None, String::new(), window, cx);
    }

    fn open_draft(
        &mut self,
        range: Range<language::Anchor>,
        quote: String,
        editing: Option<usize>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(
                tr("Your comment; Enter saves it", "写下批注，按回车保存"),
                window,
                cx,
            );
            editor.set_text(text, window, cx);
            editor
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.doc_review.draft = Some(Draft {
            range,
            quote,
            editor,
            editing,
        });
        cx.notify();
    }

    fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.doc_review.draft.take() else {
            return;
        };
        let text = draft.editor.read(cx).text(cx).trim().to_string();
        match draft.editing {
            Some(ix) if ix < self.doc_review.comments.len() => {
                if text.is_empty() {
                    self.doc_review.comments.remove(ix);
                } else {
                    self.doc_review.comments[ix].text = text;
                }
            }
            _ if text.is_empty() => {}
            _ => self.doc_review.comments.push(DocComment {
                range: draft.range,
                quote: draft.quote,
                text,
            }),
        }
        window.focus(&self.focus_handle, cx);
        self.update_doc_marks(cx);
        cx.notify();
    }

    fn cancel_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_review.draft.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn edit_comment(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.doc_review.comments.get(ix) else {
            return;
        };
        let (range, quote, text) = (
            comment.range.clone(),
            comment.quote.clone(),
            comment.text.clone(),
        );
        self.open_draft(range, quote, Some(ix), text, window, cx);
    }

    fn delete_comment(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.doc_review.comments.len() {
            self.doc_review.comments.remove(ix);
            if let Some(draft) = &mut self.doc_review.draft {
                draft.editing = match draft.editing {
                    Some(editing) if editing == ix => None,
                    Some(editing) if editing > ix => Some(editing - 1),
                    editing => editing,
                };
            }
            self.update_doc_marks(cx);
            cx.notify();
        }
    }

    /// The comments, as the action that sends them to the agent.
    fn comments_action(&self, cx: &App) -> Option<SendDocumentCommentsToAgent> {
        let buffer = self.source_buffer(cx)?;
        let buffer = buffer.read(cx);
        let path: PathBuf = buffer.file()?.as_local()?.abs_path(cx);
        let snapshot = buffer.snapshot();
        let mut comments: Vec<DocumentComment> = self
            .doc_review
            .comments
            .iter()
            .map(|comment| {
                let (start_line, end_line) = lines_of(&snapshot, &comment.range);
                DocumentComment {
                    start_line,
                    end_line,
                    quote: comment.quote.clone(),
                    comment: comment.text.clone(),
                }
            })
            .collect();
        comments.sort_by_key(|comment| (comment.start_line, comment.end_line));
        Some(SendDocumentCommentsToAgent { path, comments })
    }

    fn send_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.comments_action(cx) else {
            return;
        };
        if action.comments.is_empty() {
            return;
        }
        self.doc_review.comments.clear();
        self.update_doc_marks(cx);
        cx.notify();
        window.dispatch_action(Box::new(action), cx);
    }

    fn mark_read(&mut self, cx: &mut Context<Self>) {
        let source = self.markdown.read(cx).source().to_string();
        let review = &mut self.doc_review;
        review.read = Some(source);
        review.changes = None;
        review.show_changes = false;
        self.update_doc_marks(cx);
        cx.notify();
    }

    fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        self.doc_review.show_changes = !self.doc_review.show_changes;
        self.update_doc_marks(cx);
        cx.notify();
    }

    /// The bar saying the document changed since it was read.
    pub(super) fn render_changes_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let changes = self.doc_review.changes.as_ref()?;
        let summary = if is_chinese() {
            format!(
                "读过之后文档有改动：+{} 行 / −{} 行",
                changes.added_lines, changes.removed_lines
            )
        } else {
            format!(
                "Changed since you read it: +{} / −{} lines",
                changes.added_lines, changes.removed_lines
            )
        };
        let show_label = if self.doc_review.show_changes {
            tr("Hide Changes", "隐藏改动")
        } else {
            tr("Show Changes", "显示改动")
        };
        Some(
            h_flex()
                .w_full()
                .px_4()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .bg(cx.theme().colors().surface_background)
                .child(
                    Icon::new(IconName::Info)
                        .size(IconSize::Small)
                        .color(Color::Accent),
                )
                .child(Label::new(summary).size(LabelSize::Small))
                .child(div().flex_1())
                .child(
                    Button::new("doc-review-show-changes", show_label)
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_changes(cx))),
                )
                .child(
                    Button::new("doc-review-mark-read", tr("Mark as Read", "标为已读"))
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.mark_read(cx))),
                )
                .into_any_element(),
        )
    }

    /// The comments written and the one being written, under the document.
    pub(super) fn render_comments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let review = &self.doc_review;
        if review.comments.is_empty() && review.draft.is_none() {
            return None;
        }
        let snapshot = self
            .source_buffer(cx)
            .map(|buffer| buffer.read(cx).snapshot());
        let lines = |range: &Range<language::Anchor>| -> SharedString {
            match &snapshot {
                Some(snapshot) => match lines_of(snapshot, range) {
                    (start, end) if start == end => {
                        if is_chinese() {
                            format!("第 {start} 行").into()
                        } else {
                            format!("Line {start}").into()
                        }
                    }
                    (start, end) => {
                        if is_chinese() {
                            format!("第 {start}–{end} 行").into()
                        } else {
                            format!("Lines {start}–{end}").into()
                        }
                    }
                },
                None => SharedString::default(),
            }
        };
        let count = review.comments.len();
        let title = if is_chinese() {
            format!("{count} 条批注")
        } else if count == 1 {
            "1 comment".to_string()
        } else {
            format!("{count} comments")
        };
        let mut panel = v_flex()
            .w_full()
            .max_h(rems(20.))
            .px_4()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .gap_2()
                    .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
                    .child(div().flex_1())
                    .when(count > 0, |this| {
                        this.child(
                            Button::new("doc-review-send", tr("Send to Agent", "发给 agent"))
                                .style(ButtonStyle::Filled)
                                .label_size(LabelSize::Small)
                                .tooltip(Tooltip::text(tr(
                                    "Paste the comments, with their lines and text, into the agent that last changed this document",
                                    "把批注连同行号和原文粘贴给最近改过这份文档的 agent",
                                )))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_comments(window, cx)
                                })),
                        )
                    }),
            );
        let mut list = v_flex()
            .id("doc-review-comments")
            .gap_1()
            .overflow_y_scroll();
        for (ix, comment) in review.comments.iter().enumerate() {
            list = list.child(
                h_flex()
                    .id(("doc-review-comment", ix))
                    .gap_2()
                    .items_start()
                    .rounded_sm()
                    .px_1()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_comment(ix, window, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                Label::new(format!(
                                    "{} · “{}”",
                                    lines(&comment.range),
                                    comment.quote
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                            )
                            .child(Label::new(comment.text.clone())),
                    )
                    .child(
                        IconButton::new(("doc-review-delete", ix), IconName::Trash)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text(tr("Delete", "删除")))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.delete_comment(ix, cx)),
                            ),
                    ),
            );
        }
        panel = panel.child(list);
        if let Some(draft) = &review.draft {
            panel = panel.child(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                        this.save_draft(window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                        this.cancel_draft(window, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &editor::actions::Cancel, window, cx| {
                            this.cancel_draft(window, cx)
                        }),
                    )
                    .child(
                        Label::new(format!("{} · “{}”", lines(&draft.range), draft.quote))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(cx.theme().colors().border_focused)
                                    .bg(cx.theme().colors().editor_background)
                                    .child(draft.editor.clone()),
                            )
                            .child(
                                Button::new("doc-review-save", tr("Save", "保存"))
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_draft(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("doc-review-cancel", tr("Cancel", "取消"))
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_draft(window, cx)
                                    })),
                            ),
                    ),
            );
        }
        Some(panel.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::time::Duration;
    use util::path;

    use super::super::tests::{open_markdown_file, open_preview_for_active_editor};

    #[gpui::test]
    async fn test_comments_and_changes(cx: &mut TestAppContext) {
        let (window, editor) = open_markdown_file(
            cx,
            "plan.md",
            "# Plan

Step one.

Step two.
",
        )
        .await;
        let preview = open_preview_for_active_editor(cx, &window);
        cx.run_until_parked();

        window
            .update(cx, |_, window, cx| {
                preview.update(cx, |preview, cx| {
                    let source = preview.markdown.read(cx).source().to_string();
                    let start = source.find("Step two").unwrap();
                    preview.start_comment(start..start + "Step two".len(), window, cx);
                    let draft = preview.doc_review.draft.as_ref().unwrap().editor.clone();
                    draft.update(cx, |editor, cx| editor.set_text("Split it", window, cx));
                    preview.save_draft(window, cx);
                    let action = preview.comments_action(cx).unwrap();
                    assert_eq!(action.path, PathBuf::from(path!("/dir/plan.md")));
                    assert_eq!(
                        action.comments,
                        vec![DocumentComment {
                            start_line: 5,
                            end_line: 5,
                            quote: "Step two".into(),
                            comment: "Split it".into(),
                        }]
                    );
                    assert!(preview.doc_review.changes.is_none());
                })
            })
            .unwrap();

        // An agent changes the document: offered as changed since it was read.
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.set_text(
                        "# Plan

Step one.

Step two.

Step three.
",
                        window,
                        cx,
                    )
                })
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        preview.read_with(cx, |preview, _| {
            let changes = preview.doc_review.changes.as_ref().unwrap();
            assert_eq!((changes.added_lines, changes.removed_lines), (2, 0));
            // The comment stays on its text.
            assert_eq!(preview.doc_review.comments.len(), 1);
        });
        preview.update(cx, |preview, cx| preview.mark_read(cx));
        preview.read_with(cx, |preview, _| {
            assert!(preview.doc_review.changes.is_none())
        });
    }

    #[test]
    fn test_changes_since() {
        assert_eq!(changes_since("a\nb\n", "a\nb\n"), None);
        let now = "# Plan\n\nStep one.\nStep two, changed.\nStep three.\n";
        let changes = changes_since("# Plan\n\nStep one.\nStep two.\n", now).unwrap();
        assert_eq!(changes.added_lines, 2);
        assert_eq!(changes.removed_lines, 1);
        assert_eq!(
            changes
                .changed
                .iter()
                .map(|range| &now[range.clone()])
                .collect::<Vec<_>>(),
            vec!["Step two, changed.\nStep three.\n"]
        );
        // A removed line is marked where it was.
        let now = "a\nc\n";
        let changes = changes_since("a\nb\nc\n", now).unwrap();
        assert_eq!(changes.removed_lines, 1);
        assert_eq!(changes.changed, vec![2..2]);
    }

    #[test]
    fn test_quote() {
        assert_eq!(quote("  one\n  two  "), "one two");
        let long = "字".repeat(100);
        let quoted = quote(&long);
        assert_eq!(quoted.chars().count(), QUOTE_CHARS + 1);
        assert!(quoted.ends_with('…'));
    }
}
