//! SOLO: the view of what an agent changed in a turn: each changed file's
//! diff against its snapshot from before the turn, with hunks to keep or
//! reject, and the turns to roll back.

use std::any::{Any, TypeId};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use buffer_diff::{BufferDiff, BufferDiffEvent, DiffHunkStatus};
use collections::HashSet;
use editor::display_map::{BlockPlacement, BlockProperties, BlockStyle, CustomBlockId};
use editor::{
    DiffHunkRenderer, Editor, EditorEvent, MultiBuffer, PathKey, multibuffer_context_lines,
};
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Pixels, Subscription, Task, WeakEntity, Window,
};
use language::{
    Buffer, BufferSnapshot, Capability, OffsetRangeExt as _, Point, ToOffset as _, ToPoint as _,
};
use project::{Project, ProjectPath, RemoveOptions};
use ui::{Divider, Tooltip, prelude::*};
use util::ResultExt as _;
use util::i18n::{is_chinese, tr};
use util::rel_path::RelPath;
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
    searchable::SearchableItemHandle,
};

use super::diagnostics::{TurnDiagnostic, TurnDiagnostics};
use super::snapshot::{
    ChangedFile, FileChange, SnapshotStore, TreeId, WorkTreeChanged, linked_ancestor,
};
use super::{AgentTurn, AgentTurns};
use crate::TerminalView;

/// What the view shows.
#[derive(Clone)]
enum Mode {
    /// A turn's changes, against the files as they are now.
    Turn(usize),
    /// What rolling back to before a turn would discard, to confirm it.
    ConfirmRollback(usize),
    /// Rolled back to before a turn, which can be undone.
    RolledBack {
        number: usize,
        store: Arc<SnapshotStore>,
        undo: TreeId,
        file_count: usize,
    },
}

/// A changed file, loaded to show its diff.
pub(crate) struct LoadedFile {
    pub path: String,
    pub abs_path: PathBuf,
    /// The contents before, or `None` if it didn't exist.
    pub base: Option<String>,
    /// The contents the turn left, or `None` if it left no file.
    pub after: Option<String>,
    pub buffer: Entity<Buffer>,
}

/// A hunk of the diff shown, in its file.
struct Hunk {
    buffer_range: Range<language::Anchor>,
    base_range: Range<usize>,
}

struct ReviewFile {
    abs_path: PathBuf,
    path_key: PathKey,
    buffer: Entity<Buffer>,
    diff: Entity<BufferDiff>,
    /// What the file is compared with: its contents before the turn, with the
    /// hunks kept since. `None` if it didn't exist.
    base: Option<Arc<str>>,
    /// The file as the turn left it, with the hunks rejected since (`None`
    /// if it left no file). Anything else in it is someone else's edit, which
    /// rejecting mustn't overwrite.
    expected: Option<Arc<str>>,
    /// Updates to the diff run one after another.
    diff_update: Shared<Task<()>>,
    _subscriptions: [Subscription; 2],
}

pub(crate) struct AgentTurnReview {
    terminal_view: WeakEntity<TerminalView>,
    terminal_id: EntityId,
    project: Entity<Project>,
    multibuffer: Entity<MultiBuffer>,
    editor: Entity<Editor>,
    focus_handle: FocusHandle,
    files: Vec<ReviewFile>,
    /// Files not shown because they aren't text.
    skipped_files: usize,
    mode: Mode,
    loading: bool,
    status: Option<SharedString>,
    show_history: bool,
    /// Review comments written in the editor, not sent yet.
    comment_count: usize,
    /// The lines above hunks naming the function or type they're in.
    scope_blocks: HashSet<CustomBlockId>,
    load_task: Task<()>,
    action_task: Task<()>,
    /// SOLO: When the turn shown started: rejecting a file the turn's first
    /// snapshot doesn't have deletes it only if it was created after this.
    /// `None` (unknown): it isn't deleted.
    turn_started_at: Option<SystemTime>,
    /// SOLO: The work tree of the turn shown: files reached through a link
    /// in it (a junction, say) may be outside it, and aren't deleted.
    work_tree: Option<PathBuf>,
    /// SOLO: Moving rejected new files to the recycle bin.
    trash_task: Task<()>,
    /// SOLO: The work tree the rollback confirmation shows; rolling back
    /// stops if it changed since.
    confirmed_tree: Option<TreeId>,
}

impl AgentTurnReview {
    /// Shows a turn of `terminal_view`'s agent, in the view already showing
    /// its turns if there's one.
    pub(crate) fn open(
        terminal_view: Entity<TerminalView>,
        number: usize,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let terminal_id = terminal_view.entity_id();
        AgentTurns::global(cx).update(cx, |turns, cx| turns.dismiss_offer(terminal_id, cx));
        workspace.update(cx, |workspace, cx| {
            let existing = workspace
                .items_of_type::<AgentTurnReview>(cx)
                .find(|review| review.read(cx).terminal_id == terminal_id);
            if let Some(existing) = existing {
                workspace.activate_item(&existing, true, true, window, cx);
                existing.update(cx, |review, cx| {
                    review.set_mode(Mode::Turn(number), window, cx)
                });
                return;
            }
            let project = workspace.project().clone();
            let review =
                cx.new(|cx| AgentTurnReview::new(terminal_view, project, Some(number), window, cx));
            workspace.add_item_to_active_pane(Box::new(review), None, true, window, cx);
        });
    }

    fn new(
        terminal_view: Entity<TerminalView>,
        project: Entity<Project>,
        number: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let multibuffer = cx.new(|_| MultiBuffer::new(Capability::ReadWrite));
        let editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(multibuffer.clone(), Some(project.clone()), window, cx);
            editor.set_expand_all_diff_hunks(cx);
            editor.set_read_only(true);
            editor.set_should_serialize(false, cx);
            editor.disable_code_lens(cx);
            // Comments go to the agent in the terminal, not to an agent panel.
            editor.set_show_diff_review_button(true, cx);
            editor.register_addon(editor::DiffReviewWithoutFlag);
            editor
        });
        if let Some(turns) = AgentTurns::try_global(cx).cloned() {
            cx.observe(&turns, |_, _, cx| cx.notify()).detach();
        }
        let renderer: Arc<dyn DiffHunkRenderer> = Arc::new(TurnHunkRenderer {
            review: cx.weak_entity(),
        });
        editor.update(cx, |editor, cx| {
            editor.set_diff_hunk_renderer(Some(renderer), cx)
        });
        cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
            if let EditorEvent::ReviewCommentsChanged { total_count } = event {
                this.comment_count = *total_count;
                cx.notify();
            }
            cx.emit(event.clone())
        })
        .detach();

        let mut this = Self {
            terminal_id: terminal_view.entity_id(),
            terminal_view: terminal_view.downgrade(),
            project,
            multibuffer,
            editor,
            focus_handle: cx.focus_handle(),
            files: Vec::new(),
            skipped_files: 0,
            mode: Mode::Turn(number.unwrap_or_default()),
            loading: false,
            status: None,
            show_history: false,
            comment_count: 0,
            scope_blocks: HashSet::default(),
            load_task: Task::ready(()),
            action_task: Task::ready(()),
            turn_started_at: None,
            work_tree: None,
            trash_task: Task::ready(()),
            confirmed_tree: None,
        };
        if number.is_some() {
            this.load(window, cx);
        }
        this
    }

    fn turn(&self, number: usize, cx: &App) -> Option<AgentTurn> {
        AgentTurns::try_global(cx)?
            .read(cx)
            .turn(self.terminal_id, number)
            .cloned()
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.status = None;
        self.show_history = false;
        self.load(window, cx);
        cx.emit(EditorEvent::TitleChanged);
        cx.notify();
    }

    /// Loads the files the mode shows.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let number = match &self.mode {
            Mode::Turn(number) | Mode::ConfirmRollback(number) => *number,
            Mode::RolledBack { .. } => {
                self.set_files(Vec::new(), 0, cx);
                return;
            }
        };
        let Some(turn) = self.turn(number, cx) else {
            self.set_files(Vec::new(), 0, cx);
            self.status = Some(tr("This turn is no longer available.", "这一轮已不可用。").into());
            return;
        };
        self.turn_started_at = Some(SystemTime::from(turn.started_at));
        self.work_tree = Some(turn.store.work_tree().to_path_buf());
        let confirming = matches!(self.mode, Mode::ConfirmRollback(_));
        self.confirmed_tree = None;
        self.loading = true;
        let project = self.project.clone();
        self.load_task = cx.spawn_in(window, async move |this, cx| {
            let store = turn.store.clone();
            let before = turn.before.clone();
            let loaded = cx
                .background_spawn(async move {
                    // Rolling back discards everything since the turn started,
                    // not only what the turn changed.
                    let (after, changed) = if confirming {
                        let now = store.snapshot()?;
                        let changed = store.changed_files(&before, &now)?;
                        (now, changed)
                    } else {
                        (turn.after.clone(), turn.changed.clone())
                    };
                    let shown = confirming.then(|| after.clone());
                    anyhow::Ok((load_bases(&store, &before, &after, changed), shown))
                })
                .await;
            let loaded = match loaded {
                Ok((loaded, shown)) => {
                    this.update(cx, |this, _| this.confirmed_tree = shown).ok();
                    Ok(loaded)
                }
                Err(error) => Err(error),
            };
            let (bases, mut skipped) = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.loading = false;
                        this.status = Some(format!("{error:#}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let mut files = Vec::new();
            for (path, abs_path, base, after) in bases {
                let buffer = project
                    .update(cx, |project, cx| project.open_local_buffer(&abs_path, cx))
                    .await;
                match buffer {
                    Ok(buffer) => files.push(LoadedFile {
                        path,
                        abs_path,
                        base,
                        after,
                        buffer,
                    }),
                    Err(error) => {
                        log::warn!("can't show changes to {abs_path:?}: {error:#}");
                        skipped += 1;
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.loading = false;
                this.set_files(files, skipped, cx);
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn set_files(
        &mut self,
        files: Vec<LoadedFile>,
        skipped_files: usize,
        cx: &mut Context<Self>,
    ) {
        self.files.clear();
        self.multibuffer
            .update(cx, |multibuffer, cx| multibuffer.clear(cx));
        self.skipped_files = skipped_files;
        for file in files {
            let buffer_snapshot = file.buffer.read(cx).snapshot();
            let language = buffer_snapshot.language().cloned();
            let language_registry = file.buffer.read(cx).language_registry();
            let diff = cx
                .new(|cx| BufferDiff::new(&buffer_snapshot.text, language, language_registry, cx));
            let buffer_id = buffer_snapshot.remote_id();
            let subscriptions = [
                cx.subscribe(&file.buffer, move |this, _, event, cx| match event {
                    language::BufferEvent::Edited { .. }
                    | language::BufferEvent::Reloaded
                    | language::BufferEvent::FileHandleChanged => {
                        if let Some(ix) = this.file_index(buffer_id, cx) {
                            this.update_diff(ix, cx);
                        }
                    }
                    // The outline the scope labels come from may have changed.
                    language::BufferEvent::Reparsed => this.update_scope_labels(cx),
                    _ => {}
                }),
                cx.subscribe(&diff, move |this, _, event, cx| {
                    if let BufferDiffEvent::DiffChanged(_) = event
                        && let Some(ix) = this.file_index(buffer_id, cx)
                    {
                        this.refresh_excerpts(ix, cx);
                    }
                }),
            ];
            let path_key = match RelPath::from_unix_str(&file.path) {
                Ok(path) => PathKey::with_sort_prefix(0, path.into_arc()),
                Err(_) => PathKey::for_buffer(&file.buffer, cx),
            };
            self.files.push(ReviewFile {
                abs_path: file.abs_path,
                path_key,
                buffer: file.buffer,
                diff,
                base: file.base.map(|base| base.into()),
                expected: file.after.map(|after| normalized_text(after).into()),
                diff_update: Task::ready(()).shared(),
                _subscriptions: subscriptions,
            });
            self.update_diff(self.files.len() - 1, cx);
        }
        cx.notify();
    }

    fn file_index(&self, buffer_id: language::BufferId, cx: &App) -> Option<usize> {
        self.files
            .iter()
            .position(|file| file.buffer.read(cx).remote_id() == buffer_id)
    }

    /// Diffs the file against its base again, after any update in progress.
    fn update_diff(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(file) = self.files.get_mut(ix) else {
            return;
        };
        let previous = file.diff_update.clone();
        let diff = file.diff.clone();
        let buffer = file.buffer.clone();
        file.diff_update = cx
            .spawn(async move |this, cx| {
                previous.await;
                let Ok(Some((base, snapshot))) = this.update(cx, |this, cx| {
                    let buffer_id = buffer.read(cx).remote_id();
                    let file = &this.files[this.file_index(buffer_id, cx)?];
                    if file.diff != diff {
                        return None;
                    }
                    Some((file.base.clone(), buffer.read(cx).text_snapshot()))
                }) else {
                    return;
                };
                diff.update(cx, |diff, cx| diff.set_base_text(base, snapshot, cx))
                    .await;
            })
            .shared();
    }

    /// Shows the file's hunks, or nothing when it has none left.
    fn refresh_excerpts(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(ix) else {
            return;
        };
        let snapshot = file.buffer.read(cx).snapshot();
        let ranges: Vec<Range<Point>> = file
            .diff
            .read(cx)
            .snapshot(cx)
            .hunks_intersecting_range(
                language::Anchor::min_max_range_for_buffer(snapshot.remote_id()),
                &snapshot,
            )
            .map(|hunk| hunk.buffer_range.to_point(&snapshot))
            .collect();
        let path_key = file.path_key.clone();
        let buffer = file.buffer.clone();
        let diff = file.diff.clone();
        let context_lines = multibuffer_context_lines(cx);
        self.multibuffer.update(cx, |multibuffer, cx| {
            if ranges.is_empty() {
                multibuffer.remove_excerpts_for_paths(vec![path_key], cx);
                return;
            }
            multibuffer.update_excerpts_for_path(
                path_key,
                buffer.clone(),
                ranges,
                context_lines,
                cx,
            );
            let buffer_id = buffer.read(cx).remote_id();
            if multibuffer
                .diff_for(buffer_id)
                .is_none_or(|existing| existing != diff)
            {
                multibuffer.add_diff(diff, cx);
            }
        });
        self.update_scope_labels(cx);
        cx.notify();
    }

    /// Puts a line above each hunk naming the functions or types it's in,
    /// to tell what a change touches without scrolling up for it.
    fn update_scope_labels(&mut self, cx: &mut Context<Self>) {
        let context_lines = multibuffer_context_lines(cx);
        let snapshot = self.multibuffer.read(cx).snapshot(cx);
        let mut labels: Vec<(editor::Anchor, String)> = Vec::new();
        for hunk in snapshot.diff_hunks() {
            let Some(ix) = self.file_index(hunk.buffer_id, cx) else {
                continue;
            };
            let buffer = self.files[ix].buffer.read(cx).snapshot();
            let start = hunk.buffer_range.start.to_point(&buffer);
            if let Some(scope) = hunk_scope(&buffer, start, context_lines) {
                labels.push((hunk.multi_buffer_range.start, scope));
            }
        }
        let old = std::mem::take(&mut self.scope_blocks);
        self.scope_blocks = self.editor.update(cx, |editor, cx| {
            editor.remove_blocks(old, None, cx);
            let blocks = labels.into_iter().map(|(anchor, scope)| BlockProperties {
                placement: BlockPlacement::Above(anchor),
                height: Some(1),
                style: BlockStyle::Flex,
                render: Arc::new(move |cx| {
                    let font_size = cx
                        .editor_style
                        .text
                        .font_size
                        .to_pixels(cx.window.rem_size());
                    h_flex()
                        .h(cx.line_height)
                        .pl(cx.anchor_x)
                        .overflow_hidden()
                        .child(
                            div()
                                .font(cx.editor_style.text.font())
                                .text_size(font_size * 0.9)
                                .text_color(cx.app.theme().colors().text_muted)
                                .truncate()
                                .child(scope.clone()),
                        )
                        .into_any_element()
                }),
                priority: 0,
            });
            editor.insert_blocks(blocks, None, cx).into_iter().collect()
        });
    }

    /// The hunks touching `ranges`, by file.
    fn hunks_by_file(&self, ranges: &[Range<editor::Anchor>], cx: &App) -> Vec<(usize, Vec<Hunk>)> {
        let snapshot = self.multibuffer.read(cx).snapshot(cx);
        let mut by_file: Vec<(usize, Vec<Hunk>)> = Vec::new();
        for range in ranges {
            for hunk in snapshot.diff_hunks_in_range(range.start..range.end) {
                let Some(ix) = self.file_index(hunk.buffer_id, cx) else {
                    continue;
                };
                let hunks = match by_file.iter_mut().find(|(file_ix, _)| *file_ix == ix) {
                    Some((_, hunks)) => hunks,
                    None => {
                        by_file.push((ix, Vec::new()));
                        &mut by_file.last_mut().expect("just pushed").1
                    }
                };
                let base_range = hunk.diff_base_byte_range.start.0..hunk.diff_base_byte_range.end.0;
                if !hunks
                    .iter()
                    .any(|existing| existing.base_range == base_range)
                {
                    hunks.push(Hunk {
                        buffer_range: hunk.buffer_range,
                        base_range,
                    });
                }
            }
        }
        by_file
    }

    /// Puts the hunks back as they were before the turn, on disk too.
    pub(crate) fn reject_hunks(
        &mut self,
        ranges: Vec<Range<editor::Anchor>>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.mode, Mode::Turn(_)) {
            return;
        }
        let mut created = Vec::new();
        let mut changed_since = Vec::new();
        for (ix, hunks) in self.hunks_by_file(&ranges, cx) {
            let file = &self.files[ix];
            if !file.is_as_the_turn_left_it(cx) {
                changed_since.push(file.path_key.path.as_unix_str().to_string());
                continue;
            }
            if file.base.is_none() {
                // The turn's first snapshot doesn't have the file: the turn
                // created it, or it was ignored (or locked) then.
                created.push((file.abs_path.clone(), file.path_key.clone()));
                continue;
            }
            let base = file.diff.read(cx).base_text(cx);
            let edits: Vec<(Range<language::Anchor>, String)> = hunks
                .iter()
                .map(|hunk| {
                    (
                        hunk.buffer_range.clone(),
                        base.text_for_range(hunk.base_range.clone()).collect(),
                    )
                })
                .collect();
            let buffer = file.buffer.clone();
            let text = buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
                buffer.text()
            });
            self.files[ix].expected = Some(text.into());
            self.project
                .update(cx, |project, cx| project.save_buffer(buffer, cx))
                .detach_and_log_err(cx);
        }
        self.status = if changed_since.is_empty() {
            None
        } else if is_chinese() {
            Some(
                format!(
                    "这一轮之后又有人改过 {}，为了不覆盖这些改动，没有拒绝其中的改动。请手动修改。",
                    changed_since.join("、")
                )
                .into(),
            )
        } else {
            Some(
                format!(
                    "{} changed after the turn, so its changes weren't rejected, not to overwrite those edits. Edit it by hand.",
                    changed_since.join(", ")
                )
                .into(),
            )
        };
        if !created.is_empty() {
            self.trash_created_files(created, cx);
        }
        cx.notify();
    }

    /// SOLO: Moves files the turn created to the recycle bin, rather than
    /// deleting them for good, and stops showing them. Files that were there
    /// before the turn although its first snapshot doesn't have them (ignored
    /// then, say, by a rule the turn took out of .gitignore) are left alone.
    fn trash_created_files(&mut self, files: Vec<(PathBuf, PathKey)>, cx: &mut Context<Self>) {
        let fs = self.project.read(cx).fs().clone();
        let started_at = self.turn_started_at;
        let work_tree = self.work_tree.clone();
        let previous = std::mem::replace(&mut self.trash_task, Task::ready(()));
        self.trash_task = cx.spawn(async move |this, cx| {
            previous.await;
            let mut removed = Vec::new();
            let mut kept = Vec::new();
            for (abs_path, path_key) in files {
                let existed = cx
                    .background_spawn({
                        let abs_path = abs_path.clone();
                        let work_tree = work_tree.clone();
                        async move {
                            work_tree.is_some_and(|work_tree| {
                                linked_ancestor(&abs_path, &work_tree).is_some()
                            }) || existed_before_turn(&abs_path, started_at)
                        }
                    })
                    .await;
                if existed {
                    kept.push(path_key.path.as_unix_str().to_string());
                    continue;
                }
                let options = RemoveOptions {
                    recursive: false,
                    ignore_if_not_exists: true,
                };
                match fs.trash(&abs_path, options).await {
                    Ok(_) => removed.push(path_key),
                    Err(error) => {
                        log::warn!("failed to move {abs_path:?} to the recycle bin: {error:#}");
                        kept.push(path_key.path.as_unix_str().to_string());
                    }
                }
            }
            this.update(cx, |this, cx| {
                for path_key in removed {
                    if let Some(ix) = this.files.iter().position(|file| file.path_key == path_key) {
                        this.files.remove(ix);
                    }
                    this.multibuffer.update(cx, |multibuffer, cx| {
                        multibuffer.remove_excerpts_for_paths(vec![path_key], cx)
                    });
                }
                if !kept.is_empty() {
                    let message = if is_chinese() {
                        format!(
                            "{} 可能在这一轮之前就已存在（例如当时被 .gitignore 忽略），所以没有删除。请手动处理。",
                            kept.join("、")
                        )
                    } else {
                        format!(
                            "{} may have existed before the turn (ignored by .gitignore then, say), so it wasn't deleted. Remove it by hand if you want.",
                            kept.join(", ")
                        )
                    };
                    this.status = Some(match this.status.take() {
                        Some(status) => format!("{status} {message}").into(),
                        None => message.into(),
                    });
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Takes the hunks as they are now: they stop showing as changes.
    pub(crate) fn keep_hunks(
        &mut self,
        ranges: Vec<Range<editor::Anchor>>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.mode, Mode::Turn(_)) {
            return;
        }
        for (ix, mut hunks) in self.hunks_by_file(&ranges, cx) {
            let file = &self.files[ix];
            let buffer = file.buffer.read(cx).snapshot();
            let mut base = file.diff.read(cx).base_text(cx).text();
            hunks.sort_by_key(|hunk| std::cmp::Reverse(hunk.base_range.start));
            for hunk in hunks {
                let new_text: String = buffer.text_for_range(hunk.buffer_range.clone()).collect();
                if base.get(hunk.base_range.clone()).is_some() {
                    base.replace_range(hunk.base_range, &new_text);
                }
            }
            self.files[ix].base = Some(base.into());
            self.update_diff(ix, cx);
        }
        cx.notify();
    }

    fn all_hunks() -> Vec<Range<editor::Anchor>> {
        vec![editor::Anchor::Min..editor::Anchor::Max]
    }

    fn confirm_rollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::ConfirmRollback(number) = self.mode else {
            return;
        };
        let Some(turn) = self.turn(number, cx) else {
            return;
        };
        // SOLO: only what the page shows: not before it's loaded.
        let Some(shown) = self.confirmed_tree.clone() else {
            return;
        };
        let store = turn.store.clone();
        let target = turn.before;
        self.status = Some(tr("Rolling back…", "正在回滚…").into());
        self.action_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let store = store.clone();
                    async move { store.restore_if_unchanged(&target, &shown) }
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(restored) => {
                    let file_count = restored.changed.len();
                    this.set_mode(
                        Mode::RolledBack {
                            number,
                            store,
                            undo: restored.undo,
                            file_count,
                        },
                        window,
                        cx,
                    );
                    this.status = skipped_message(&restored.skipped);
                }
                Err(error) if error.downcast_ref::<WorkTreeChanged>().is_some() => {
                    // Show what rolling back would discard now.
                    this.set_mode(Mode::ConfirmRollback(number), window, cx);
                    this.status = Some(
                        tr(
                            "Files changed since this page was shown, so nothing was rolled back. Check the changes below and confirm again.",
                            "这个页面显示之后又有文件改动，所以没有回滚。请检查下面的改动后再确认。",
                        )
                        .into(),
                    );
                }
                Err(error) => {
                    this.status =
                        Some(format!("{}: {error:#}", tr("Rollback failed", "回滚失败")).into());
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    fn undo_rollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::RolledBack { store, undo, .. } = self.mode.clone() else {
            return;
        };
        self.status = Some(tr("Undoing the rollback…", "正在撤销回滚…").into());
        self.action_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { store.restore(&undo) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(restored) => {
                    let latest = AgentTurns::try_global(cx)
                        .and_then(|turns| turns.read(cx).turns(this.terminal_id).last())
                        .map(|turn| turn.number)
                        .unwrap_or_default();
                    this.set_mode(Mode::Turn(latest), window, cx);
                    let undone = tr("The rollback was undone.", "已撤销回滚。");
                    this.status = Some(match skipped_message(&restored.skipped) {
                        Some(skipped) => format!("{undone} {skipped}").into(),
                        None => undone.into(),
                    });
                }
                Err(error) => {
                    this.status =
                        Some(format!("{}: {error:#}", tr("Undo failed", "撤销失败")).into());
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    /// Takes the review comments out of the editor, with where they are.
    fn take_comments(&mut self, cx: &mut Context<Self>) -> Vec<ReviewComment> {
        let comments = self
            .editor
            .update(cx, |editor, cx| editor.take_review_comments(cx));
        let directory = match &self.mode {
            Mode::Turn(number) | Mode::ConfirmRollback(number) => {
                self.turn(*number, cx).map(|turn| turn.directory)
            }
            Mode::RolledBack { .. } => None,
        };
        let snapshot = self.multibuffer.read(cx).snapshot(cx);
        comments
            .into_iter()
            .filter_map(|(range, comment)| {
                let start = editor::ToPoint::to_point(&range.start, &snapshot);
                let end = editor::ToPoint::to_point(&range.end, &snapshot);
                let (buffer, start) = snapshot.point_to_buffer_point(start)?;
                let (_, end) = snapshot.point_to_buffer_point(end)?;
                let buffer_id = buffer.remote_id();
                let end = end.max(start);
                let code: String = buffer
                    .text_for_range(
                        Point::new(start.row, 0)..Point::new(end.row, buffer.line_len(end.row)),
                    )
                    .collect();
                let file = &self.files[self.file_index(buffer_id, cx)?];
                let path = directory
                    .as_ref()
                    .and_then(|directory| file.abs_path.strip_prefix(directory).ok())
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| file.abs_path.to_string_lossy().into_owned());
                Some(ReviewComment {
                    path,
                    lines: start.row + 1..=end.row + 1,
                    code,
                    comment,
                })
            })
            .collect()
    }

    /// Sends the review comments to the agent's terminal as one message.
    fn send_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let comments = self.take_comments(cx);
        if comments.is_empty() {
            return;
        }
        let message = review_message(&comments);
        let count = comments.len();
        let sent = if is_chinese() {
            format!("已把 {count} 条批注发送给 agent。")
        } else if count == 1 {
            "Sent 1 comment to the agent.".to_string()
        } else {
            format!("Sent {count} comments to the agent.")
        };
        self.paste_to_agent(message, sent, window, cx);
    }

    /// Sends the problems the turn added to its agent, to fix them.
    fn send_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::Turn(number) = self.mode else {
            return;
        };
        let Some(turn) = self.turn(number, cx) else {
            return;
        };
        let TurnDiagnostics::Checked { added, .. } = &turn.diagnostics else {
            return;
        };
        if added.is_empty() {
            return;
        }
        let message = diagnostics_message(added, &turn.directory);
        let sent = tr(
            "Sent the new problems to the agent.",
            "已把新增的问题发送给 agent。",
        )
        .to_string();
        self.paste_to_agent(message, sent, window, cx);
    }

    /// Pastes `message` into the agent's prompt, or else copies it.
    fn paste_to_agent(
        &mut self,
        message: String,
        sent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal_view = self.terminal_view.upgrade();
        let workspace = terminal_view
            .as_ref()
            .and_then(|view| view.read(cx).workspace.upgrade());
        let (Some(terminal_view), Some(workspace)) = (terminal_view, workspace) else {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message));
            self.status = Some(
                tr(
                    "The agent's terminal is gone; copied the message instead.",
                    "agent 终端已关闭，已改为复制。",
                )
                .into(),
            );
            cx.notify();
            return;
        };
        if !crate::agent_context::can_paste_to_agent(terminal_view.read(cx), &message, cx) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message));
            self.status = Some(
                tr(
                    "The agent isn't running in its terminal, or isn't taking pasted text; copied the message instead.",
                    "agent 已不在终端里运行，或现在不接受粘贴，已改为复制。",
                )
                .into(),
            );
            cx.notify();
            return;
        }
        terminal_view.update(cx, |view, cx| {
            view.terminal
                .update(cx, |terminal, _| terminal.paste(&message))
        });
        self.status = Some(sent.into());
        cx.notify();
        // Switching to the terminal deactivates this view, which is being updated.
        window.defer(cx, move |window, cx| {
            crate::agent_context::reveal_terminal_view(&workspace, &terminal_view, window, cx)
        });
    }

    /// Opens a diagnostic's file at its position.
    fn open_diagnostic(
        &self,
        diagnostic: &TurnDiagnostic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .terminal_view
            .upgrade()
            .and_then(|view| view.read(cx).workspace.upgrade())
        else {
            return;
        };
        let point = Point::new(
            diagnostic.line.saturating_sub(1),
            diagnostic.column.saturating_sub(1),
        );
        let opened = workspace.update(cx, |workspace, cx| {
            workspace.open_abs_path(
                diagnostic.path.clone(),
                workspace::OpenOptions::default(),
                window,
                cx,
            )
        });
        cx.spawn_in(window, async move |_, cx| {
            let Ok(item) = opened.await else {
                return;
            };
            if let Ok(Some(editor)) = cx.update(|_, cx| item.act_as::<Editor>(cx)) {
                editor
                    .update_in(cx, |editor, window, cx| {
                        editor.go_to_singleton_buffer_point(point, window, cx)
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// What the turn did to the language servers' diagnostics.
    fn render_diagnostics(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Mode::Turn(number) = self.mode else {
            return None;
        };
        let turn = self.turn(number, cx)?;
        let chinese = is_chinese();
        let muted = |text: String| {
            Some(
                h_flex()
                    .px_3()
                    .py_1()
                    .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
                    .into_any_element(),
            )
        };
        let (added, added_uncertain, fixed_errors) = match &turn.diagnostics {
            TurnDiagnostics::Checking => {
                return muted(tr("Checking diagnostics…", "正在检查诊断…").to_string());
            }
            TurnDiagnostics::Unchecked => {
                return muted(
                    tr(
                        "Diagnostics: not checked (no language server, or it didn't finish)",
                        "诊断：未检查（没有语言服务器，或它没有及时完成）",
                    )
                    .to_string(),
                );
            }
            TurnDiagnostics::Checked {
                added,
                added_uncertain,
                fixed_errors,
            } => (added, *added_uncertain, *fixed_errors),
        };
        let errors = added
            .iter()
            .filter(|diagnostic| diagnostic.is_error)
            .count();
        let warnings = added.len() - errors;
        if added.is_empty() && added_uncertain == 0 {
            let text = match (fixed_errors, chinese) {
                (0, true) => "诊断：没有新的错误或警告".to_string(),
                (0, false) => "Diagnostics: no new errors or warnings".to_string(),
                (fixed, true) => format!("诊断：没有新问题，修掉了 {fixed} 个错误"),
                (fixed, false) => format!("Diagnostics: nothing new, {fixed} errors fixed"),
            };
            return muted(text);
        }
        let summary = if chinese {
            format!(
                "这一轮新增 {} 个错误、{warnings} 个警告，修掉了 {fixed_errors} 个错误",
                errors + added_uncertain
            )
        } else {
            format!(
                "This turn added {} errors and {warnings} warnings, and fixed {fixed_errors} errors",
                errors + added_uncertain
            )
        };
        const MAX_SHOWN: usize = 20;
        let rows = added
            .iter()
            .take(MAX_SHOWN)
            .enumerate()
            .map(|(ix, diagnostic)| {
                let path = diagnostic
                    .path
                    .strip_prefix(&turn.directory)
                    .unwrap_or(&diagnostic.path)
                    .display()
                    .to_string();
                let location = format!("{path}:{}:{}", diagnostic.line, diagnostic.column);
                let message = diagnostic
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let target = diagnostic.clone();
                h_flex()
                    .id(("turn-diagnostic", ix))
                    .gap_2()
                    .px_3()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        Icon::new(if diagnostic.is_error {
                            IconName::XCircle
                        } else {
                            IconName::Warning
                        })
                        .size(IconSize::Small)
                        .color(if diagnostic.is_error {
                            Color::Error
                        } else {
                            Color::Warning
                        }),
                    )
                    .child(
                        Label::new(location)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(message).size(LabelSize::Small).truncate())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_diagnostic(&target, window, cx)
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let more = added.len().saturating_sub(MAX_SHOWN);
        Some(
            v_flex()
                .py_1()
                .gap_0p5()
                .child(
                    h_flex()
                        .px_3()
                        .gap_2()
                        .child(Label::new(summary).size(LabelSize::Small))
                        .when(!added.is_empty(), |this| {
                            this.child(
                                Button::new(
                                    "send-diagnostics",
                                    tr("Send to Agent", "发给 agent"),
                                )
                                .label_size(LabelSize::Small)
                                .tooltip(Tooltip::text(tr(
                                    "Paste the new problems into the agent's terminal to fix them",
                                    "把新增的问题粘贴到 agent 终端，让它修复",
                                )))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_diagnostics(window, cx)
                                })),
                            )
                        }),
                )
                .children(rows)
                .when(more > 0, |this| {
                    this.child(
                        h_flex().px_3().child(
                            Label::new(if chinese {
                                format!("还有 {more} 个")
                            } else {
                                format!("and {more} more")
                            })
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        ),
                    )
                })
                .when(added_uncertain > 0, |this| {
                    this.child(
                        h_flex().px_3().child(
                            Label::new(if chinese {
                                format!(
                                    "另有 {added_uncertain} 个新错误在改动前没有打开的文件里，分不出是哪几个"
                                )
                            } else {
                                format!(
                                    "and {added_uncertain} more errors in files that weren't open before the turn, which can't be told apart from the old ones"
                                )
                            })
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                        ),
                    )
                })
                .child(Divider::horizontal())
                .into_any_element(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, actions): (String, AnyElement) = match &self.mode {
            Mode::Turn(number) => {
                let turn = self.turn(*number, cx);
                let title = turn
                    .as_ref()
                    .map(|turn| turn_title(turn))
                    .unwrap_or_default();
                let has_hunks = !self.multibuffer.read(cx).is_empty();
                let comment_count = self.comment_count;
                let actions = h_flex()
                    .gap_1()
                    .when(comment_count > 0, |this| {
                        this.child(
                            Button::new(
                                "send-comments",
                                if is_chinese() {
                                    format!("发送 {comment_count} 条批注给 agent")
                                } else if comment_count == 1 {
                                    "Send 1 Comment to Agent".to_string()
                                } else {
                                    format!("Send {comment_count} Comments to Agent")
                                },
                            )
                            .style(ButtonStyle::Filled)
                            .tooltip(Tooltip::text(tr(
                                "Paste the comments into the agent's terminal as one message",
                                "把批注合成一条消息粘贴到 agent 终端",
                            )))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_comments(window, cx)),
                            ),
                        )
                    })
                    .child(
                        Button::new("reject-all", tr("Reject All", "全部拒绝"))
                            .disabled(!has_hunks)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reject_hunks(Self::all_hunks(), cx)
                            })),
                    )
                    .child(
                        Button::new("keep-all", tr("Keep All", "全部保留"))
                            .disabled(!has_hunks)
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.keep_hunks(Self::all_hunks(), cx)
                                }),
                            ),
                    )
                    .into_any_element();
                (title, actions)
            }
            Mode::ConfirmRollback(number) => {
                let count = self.files.len() + self.skipped_files;
                let title = if is_chinese() {
                    format!("回滚到第 {number} 轮开始之前，将丢弃下面 {count} 个文件的改动")
                } else {
                    format!(
                        "Rolling back to before turn {number} discards the changes below to {count} files"
                    )
                };
                let number = *number;
                let actions =
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("cancel-rollback", tr("Cancel", "取消")).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.set_mode(Mode::Turn(number), window, cx)
                                }),
                            ),
                        )
                        .child(
                            Button::new("confirm-rollback", tr("Roll Back", "回滚"))
                                .style(ButtonStyle::Tinted(ui::TintColor::Error))
                                .disabled(self.loading)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_rollback(window, cx)
                                })),
                        )
                        .into_any_element();
                (title, actions)
            }
            Mode::RolledBack {
                number, file_count, ..
            } => {
                let title = if is_chinese() {
                    format!("已回滚到第 {number} 轮开始之前（{file_count} 个文件）")
                } else {
                    format!("Rolled back to before turn {number} ({file_count} files)")
                };
                let actions = Button::new("undo-rollback", tr("Undo Rollback", "撤销回滚"))
                    .on_click(cx.listener(|this, _, window, cx| this.undo_rollback(window, cx)))
                    .into_any_element();
                (title, actions)
            }
        };

        let history_count = AgentTurns::try_global(cx)
            .map(|turns| turns.read(cx).turns(self.terminal_id).count())
            .unwrap_or_default();
        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(Label::new(title).single_line().truncate())
                    .child(
                        h_flex().gap_1().child(actions).child(
                            Button::new(
                                "turn-history",
                                if is_chinese() {
                                    format!("轮次 ({history_count})")
                                } else {
                                    format!("Turns ({history_count})")
                                },
                            )
                            .toggle_state(self.show_history)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_history = !this.show_history;
                                cx.notify();
                            })),
                        ),
                    ),
            )
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    Label::new(status)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .when(self.skipped_files > 0, |this| {
                let count = self.skipped_files;
                this.child(
                    Label::new(if is_chinese() {
                        format!("{count} 个非文本文件未显示")
                    } else {
                        format!("{count} files that aren't text aren't shown")
                    })
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
            })
            .when(self.show_history, |this| {
                this.child(Divider::horizontal())
                    .children(self.render_history(cx))
            })
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(turns) = AgentTurns::try_global(cx).cloned() else {
            return Vec::new();
        };
        let turns: Vec<AgentTurn> = turns
            .read(cx)
            .turns(self.terminal_id)
            .rev()
            .cloned()
            .collect();
        turns
            .into_iter()
            .map(|turn| {
                let number = turn.number;
                let files = turn.changed.len();
                let files = if is_chinese() {
                    format!("{files} 个文件")
                } else if files == 1 {
                    "1 file".to_string()
                } else {
                    format!("{files} files")
                };
                h_flex()
                    .id(("turn", number))
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Label::new(turn_title(&turn))
                                    .size(LabelSize::Small)
                                    .truncate(),
                            )
                            .child(Label::new(files).size(LabelSize::Small).color(Color::Muted)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("view-turn", number), tr("View", "查看"))
                                    .label_size(LabelSize::Small)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_mode(Mode::Turn(number), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(
                                    ("roll-back-turn", number),
                                    tr("Roll Back to Before", "回滚到这一轮之前"),
                                )
                                .label_size(LabelSize::Small)
                                .tooltip(Tooltip::text(tr(
                                    "Shows what would be discarded first",
                                    "会先展示将被丢弃的改动",
                                )))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.set_mode(Mode::ConfirmRollback(number), window, cx)
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// SOLO: What to tell about files a rollback left alone because they're in
/// a linked folder (a junction or symlink).
fn skipped_message(skipped: &[String]) -> Option<SharedString> {
    if skipped.is_empty() {
        return None;
    }
    const MAX_NAMED: usize = 5;
    let mut names = skipped.iter().take(MAX_NAMED).cloned().collect::<Vec<_>>();
    if skipped.len() > MAX_NAMED {
        names.push("…".to_string());
    }
    Some(if is_chinese() {
        format!(
            "{} 个文件位于链接的文件夹（目录联接或符号链接）中，可能指向项目之外，所以没有动：{}",
            skipped.len(),
            names.join("、")
        )
        .into()
    } else {
        format!(
            "{} files are in a linked folder (a junction or symlink) that may lead outside the project, so they were left alone: {}",
            skipped.len(),
            names.join(", ")
        )
        .into()
    })
}

/// SOLO: Whether the file at `abs_path`, which the turn's first snapshot
/// doesn't have, was there before the turn started at `started_at` (or that
/// can't be told): it was ignored or couldn't be read then, and the turn
/// didn't create it.
fn existed_before_turn(abs_path: &Path, started_at: Option<SystemTime>) -> bool {
    let Some(started_at) = started_at else {
        return true;
    };
    match std::fs::symlink_metadata(abs_path) {
        // Nothing to delete.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(metadata) => match metadata.created() {
            Ok(created) => created < started_at,
            Err(_) => true,
        },
    }
}

impl ReviewFile {
    /// Whether nothing but the review changed the file since the turn, and
    /// its buffer has no unsaved edits.
    fn is_as_the_turn_left_it(&self, cx: &App) -> bool {
        let buffer = self.buffer.read(cx);
        if buffer.is_dirty() || buffer.has_conflict() {
            return false;
        }
        match &self.expected {
            Some(expected) => buffer.text() == **expected,
            None => buffer.len() == 0,
        }
    }
}

/// Text as a buffer holds it: without a byte order mark, with `\n` line
/// endings.
/// The functions or types a hunk starting at `start` is in, outermost first,
/// unless the innermost one starts close enough above to show in the hunk's
/// context lines anyway.
fn hunk_scope(buffer: &BufferSnapshot, start: Point, context_lines: u32) -> Option<String> {
    let offset = start.to_offset(buffer);
    let mut scopes = buffer.outline_items_containing(offset..offset, false, None);
    // Only what encloses the hunk, not what it begins.
    scopes.retain(|item| {
        item.range.start.to_point(buffer).row < start.row
            && item.range.end.to_offset(buffer) > offset
    });
    scopes.sort_by_key(|item| item.depth);
    let innermost = scopes.last()?;
    if innermost.range.start.to_point(buffer).row + context_lines >= start.row {
        return None;
    }
    Some(
        scopes
            .iter()
            .map(|item| item.text.as_ref())
            .collect::<Vec<_>>()
            .join(" › "),
    )
}

fn normalized_text(text: String) -> String {
    let mut text = match text.strip_prefix('\u{feff}') {
        Some(text) => text.to_string(),
        None => text,
    };
    language::LineEnding::normalize(&mut text);
    text
}

/// A changed file's path, absolute path, contents before and contents after.
type FileContents = (String, PathBuf, Option<String>, Option<String>);

/// The message that asks the agent to fix the problems its turn added.
fn diagnostics_message(diagnostics: &[TurnDiagnostic], directory: &std::path::Path) -> String {
    let mut message = String::from(tr(
        "Your last turn added these problems; please fix them:\n",
        "你上一轮的改动引入了下面这些问题，请修复：\n",
    ));
    for diagnostic in diagnostics {
        let path = diagnostic
            .path
            .strip_prefix(directory)
            .unwrap_or(&diagnostic.path)
            .display()
            .to_string()
            .replace('\\', "/");
        let severity = if diagnostic.is_error {
            "error"
        } else {
            "warning"
        };
        let source = diagnostic
            .source
            .as_deref()
            .map(|source| format!(" [{source}]"))
            .unwrap_or_default();
        let text = diagnostic.message.trim_end().replace('\n', "\n  ");
        message.push_str(&format!(
            "- {path}:{}:{} {severity}{source}: {text}\n",
            diagnostic.line, diagnostic.column
        ));
    }
    message
}

/// The text files among `changed`, with their contents before `before` and
/// in `after`; and how many weren't text.
fn load_bases(
    store: &SnapshotStore,
    before: &str,
    after: &str,
    changed: Vec<ChangedFile>,
) -> (Vec<FileContents>, usize) {
    let text_in = |tree: &str, path: &str| {
        store
            .file_contents(tree, path)
            .log_err()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|text| !text.contains('\0'))
    };
    let mut skipped = 0;
    let mut files = Vec::new();
    for file in changed {
        let base = if file.change == FileChange::Added {
            None
        } else {
            let Some(text) = text_in(before, &file.path) else {
                skipped += 1;
                continue;
            };
            Some(text)
        };
        let after = if file.change == FileChange::Deleted {
            None
        } else {
            let Some(text) = text_in(after, &file.path) else {
                skipped += 1;
                continue;
            };
            Some(text)
        };
        let abs_path = store.abs_path(&file.path);
        files.push((file.path, abs_path, base, after));
    }
    (files, skipped)
}

/// A review comment on lines of a file.
#[derive(Clone, Debug, PartialEq)]
struct ReviewComment {
    /// Relative to the agent's working directory when it's in there.
    path: String,
    /// From 1.
    lines: std::ops::RangeInclusive<u32>,
    code: String,
    comment: String,
}

/// Comments on more lines than this go without the code: the location says
/// where.
const MAX_QUOTED_LINES: usize = 12;

/// The message that gives the agent the review comments: each one's location
/// and comment, with the code it's about when that's short.
fn review_message(comments: &[ReviewComment]) -> String {
    let mut message = String::from(tr(
        "Please address these review comments on your changes:\n",
        "请处理以下针对你的改动的审查意见：\n",
    ));
    for comment in comments {
        let location = if comment.lines.start() == comment.lines.end() {
            format!("{}:{}", comment.path, comment.lines.start())
        } else {
            format!(
                "{}:{}-{}",
                comment.path,
                comment.lines.start(),
                comment.lines.end()
            )
        };
        message.push_str(&format!("\n- {location} — {}\n", comment.comment.trim()));
        let code = comment.code.trim_end();
        if !code.trim().is_empty() && code.lines().count() <= MAX_QUOTED_LINES {
            let fence = if code.contains("```") { "````" } else { "```" };
            message.push_str(&format!("  {fence}\n"));
            for line in code.lines() {
                message.push_str("  ");
                message.push_str(line);
                message.push('\n');
            }
            message.push_str(&format!("  {fence}\n"));
        }
    }
    message
}

fn turn_title(turn: &AgentTurn) -> String {
    let time = time_format::format_time(turn.started_at);
    let prompt = turn
        .prompt
        .as_deref()
        .and_then(|prompt| prompt.lines().find(|line| !line.trim().is_empty()))
        .map(|line| util::truncate_and_trailoff(line.trim(), 60));
    let number = turn.number;
    let mut title = if is_chinese() {
        format!("第 {number} 轮 · {time}")
    } else {
        format!("Turn {number} · {time}")
    };
    if let Some(prompt) = prompt {
        title.push_str(" · ");
        title.push_str(&prompt);
    }
    title
}

struct TurnHunkRenderer {
    review: WeakEntity<AgentTurnReview>,
}

impl DiffHunkRenderer for TurnHunkRenderer {
    fn render_hunk_controls(
        &self,
        row: u32,
        _status: &DiffHunkStatus,
        hunk_range: Range<editor::Anchor>,
        is_created_file: bool,
        line_height: Pixels,
        _editor: &Entity<Editor>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(review) = self.review.upgrade() else {
            return gpui::Empty.into_any_element();
        };
        if !matches!(review.read(cx).mode, Mode::Turn(_)) {
            return gpui::Empty.into_any_element();
        }
        let opaque_window =
            cx.theme().window_background_appearance() == gpui::WindowBackgroundAppearance::Opaque;
        h_flex()
            .h(line_height)
            .mr_0p5()
            .gap_1()
            .px_0p5()
            .pb_1()
            .border_x_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .rounded_b_md()
            .bg(cx.theme().colors().editor_background)
            .block_mouse_except_scroll()
            .when(opaque_window, |this| this.shadow_md())
            .child(
                Button::new(("reject-hunk", row as u64), tr("Reject", "拒绝"))
                    .tooltip(Tooltip::text(if is_created_file {
                        tr("Delete the file the agent created", "删除 agent 新建的文件")
                    } else {
                        tr(
                            "Put this back as it was before the turn",
                            "恢复为本轮之前的内容",
                        )
                    }))
                    .on_click({
                        let review = review.clone();
                        let hunk_range = hunk_range.clone();
                        move |_, _, cx| {
                            review.update(cx, |review, cx| {
                                review.reject_hunks(vec![hunk_range.clone()], cx)
                            });
                        }
                    }),
            )
            .child(
                Button::new(("keep-hunk", row as u64), tr("Keep", "保留")).on_click({
                    move |_, _, cx| {
                        review.update(cx, |review, cx| {
                            review.keep_hunks(vec![hunk_range.clone()], cx)
                        });
                    }
                }),
            )
            .into_any_element()
    }

    fn render_hunk_as_staged(&self, _status: &DiffHunkStatus, _cx: &App) -> bool {
        false
    }
}

impl EventEmitter<EditorEvent> for AgentTurnReview {}

impl Focusable for AgentTurnReview {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.multibuffer.read(cx).is_empty() {
            self.focus_handle.clone()
        } else {
            self.editor.focus_handle(cx)
        }
    }
}

impl Item for AgentTurnReview {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let agent = self.terminal_view.upgrade().and_then(|view| {
            view.read(cx)
                .agent_status()
                .map(|(name, _)| name.to_string())
        });
        match agent {
            Some(agent) if is_chinese() => format!("{agent} 的改动").into(),
            Some(agent) => format!("{agent} Changes").into(),
            None => tr("Agent Changes", "Agent 改动").into(),
        }
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.deactivated(window, cx));
    }

    // Go to definition, find references and the way back need the editor to
    // know its workspace and history.
    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            Item::set_nav_history(editor, nav_history, window, cx)
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        self.editor.read(cx).for_each_project_item(cx, f)
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.editor.read(cx).active_project_path(cx)
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }
}

impl Render for AgentTurnReview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.multibuffer.read(cx).is_empty();
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().colors().editor_background)
            .child(self.render_header(cx))
            .children(self.render_diagnostics(cx))
            .child(if empty {
                let message = if self.loading {
                    tr("Loading…", "正在加载…")
                } else if matches!(self.mode, Mode::RolledBack { .. }) {
                    ""
                } else {
                    tr("No changes left to review.", "没有待审查的改动。")
                };
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(Label::new(message).color(Color::Muted))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.editor.clone())
                    .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::ToPoint as _;
    use futures::StreamExt as _;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Fs as _};
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    #[test]
    fn test_diagnostics_message() {
        let directory = PathBuf::from(path!("/project"));
        let message = diagnostics_message(
            &[TurnDiagnostic {
                path: directory.join("src").join("main.rs"),
                line: 3,
                column: 9,
                is_error: true,
                message: "mismatched types\nexpected u32".into(),
                source: Some("rustc".into()),
            }],
            &directory,
        );
        let heading = tr(
            "Your last turn added these problems; please fix them:\n",
            "你上一轮的改动引入了下面这些问题，请修复：\n",
        );
        assert_eq!(
            message,
            format!("{heading}- src/main.rs:3:9 error [rustc]: mismatched types\n  expected u32\n")
        );
    }

    #[test]
    fn test_review_message() {
        let message = review_message(&[
            ReviewComment {
                path: "src/main.rs".into(),
                lines: 12..=12,
                code: "    let x = 1;\n".into(),
                comment: "Name this better ".into(),
            },
            ReviewComment {
                path: "README.md".into(),
                lines: 3..=40,
                code: "line\n".repeat(38),
                comment: "Too long".into(),
            },
        ]);
        let heading = tr(
            "Please address these review comments on your changes:\n",
            "请处理以下针对你的改动的审查意见：\n",
        );
        assert_eq!(
            message,
            format!(
                "{heading}\n- src/main.rs:12 — Name this better\n  ```\n      let x = 1;\n  ```\n\n- README.md:3-40 — Too long\n"
            )
        );
    }

    /// A review of a turn that left `/project` as `tree`, changing the files
    /// in `bases` from those contents (`None`: the turn created the file).
    async fn open_review<'a>(
        cx: &'a mut TestAppContext,
        tree: serde_json::Value,
        bases: &[(&str, Option<&str>)],
    ) -> (
        Arc<FakeFs>,
        Entity<AgentTurnReview>,
        &'a mut VisualTestContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), tree).await;
        let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
        let (_, review, cx) = show_review(project, &fs, bases, cx).await;
        (fs, review, cx)
    }

    /// Shows the turn that changed the files in `bases` of `project`, in a
    /// workspace, as opening it does.
    async fn show_review<'a>(
        project: Entity<Project>,
        fs: &Arc<FakeFs>,
        bases: &[(&str, Option<&str>)],
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<AgentTurnReview>,
        &'a mut VisualTestContext,
    ) {
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let terminal_view = workspace.update_in(cx, |workspace, window, cx| {
            let terminal = cx.new(|cx| {
                terminal::TerminalBuilder::new_display_only(
                    Default::default(),
                    terminal::terminal_settings::AlternateScroll::On,
                    None,
                    0,
                    cx.background_executor(),
                    util::paths::PathStyle::local(),
                )
                .subscribe(cx)
            });
            cx.new(|cx| {
                TerminalView::new(
                    terminal,
                    workspace.weak_handle(),
                    None,
                    project.downgrade(),
                    window,
                    cx,
                )
            })
        });

        let mut files = Vec::new();
        for (name, base) in bases {
            let abs_path = PathBuf::from(path!("/project")).join(name);
            let after = fs.load(&abs_path).await.ok();
            let buffer = project
                .update(cx, |project, cx| project.open_local_buffer(&abs_path, cx))
                .await
                .unwrap();
            files.push(LoadedFile {
                path: name.to_string(),
                abs_path,
                base: base.map(str::to_string),
                after,
                buffer,
            });
        }
        let review = cx.new_window_entity(|window, cx| {
            AgentTurnReview::new(terminal_view, project.clone(), None, window, cx)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(review.clone()), None, true, window, cx)
        });
        review.update(cx, |review, cx| {
            // The turn started now: these files aren't on disk, so none was
            // there before it.
            review.turn_started_at = Some(SystemTime::now());
            review.set_files(files, 0, cx)
        });
        cx.run_until_parked();
        (workspace, review, cx)
    }

    #[gpui::test]
    async fn test_hunk_scope(cx: &mut TestAppContext) {
        let text = "struct A;\n\nimpl A {\n    fn one(&self) {\n        let a = 1;\n        let b = 2;\n        let c = 3;\n        let d = 4;\n        let e = 5;\n    }\n}\n";
        let buffer = cx.new(|cx| Buffer::local(text, cx).with_language(language::rust_lang(), cx));
        cx.run_until_parked();
        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        // Far enough into `one` that its signature isn't in the context lines.
        assert_eq!(
            hunk_scope(&snapshot, Point::new(8, 8), 3).as_deref(),
            Some("impl A › fn one")
        );
        // Close enough that it is.
        assert_eq!(hunk_scope(&snapshot, Point::new(5, 8), 3), None);
        // A hunk beginning a function is in what encloses that.
        assert_eq!(
            hunk_scope(&snapshot, Point::new(3, 0), 0).as_deref(),
            Some("impl A")
        );
        // Nothing encloses it.
        assert_eq!(hunk_scope(&snapshot, Point::new(0, 0), 0), None);
    }

    #[gpui::test]
    async fn test_review_goes_to_definitions(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/project"),
            json!({
                "a.rs": "fn one() {\n    two();\n}\n",
                "b.rs": "pub fn two() {}\n",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
        project.read_with(cx, |project, _| {
            project.languages().add(language::rust_lang())
        });
        let mut servers = project.read_with(cx, |project, _| {
            project.languages().register_fake_lsp(
                "Rust",
                language::FakeLspAdapter {
                    capabilities: lsp::ServerCapabilities {
                        definition_provider: Some(lsp::OneOf::Left(true)),
                        ..lsp::ServerCapabilities::default()
                    },
                    ..language::FakeLspAdapter::default()
                },
            )
        });
        let (workspace, review, cx) =
            show_review(project, &fs, &[("a.rs", Some("fn one() {\n}\n"))], cx).await;
        let server = servers.next().await.expect("the review starts the server");
        let b = lsp::Uri::from_file_path(path!("/project/b.rs")).unwrap();
        let mut definitions =
            server.set_request_handler::<lsp::request::GotoDefinition, _, _>(move |_, _| {
                let b = b.clone();
                async move {
                    Ok(Some(lsp::GotoDefinitionResponse::Scalar(
                        lsp::Location::new(
                            b,
                            lsp::Range::new(lsp::Position::new(0, 7), lsp::Position::new(0, 10)),
                        ),
                    )))
                }
            });

        let editor = review.read_with(cx, |review, _| review.editor.clone());
        let navigated = editor.update_in(cx, |editor, window, cx| {
            let text = editor.buffer().read(cx).snapshot(cx).text();
            let offset = text.find("two").expect("the call is shown");
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([
                    editor::MultiBufferOffset(offset)..editor::MultiBufferOffset(offset)
                ])
            });
            editor.go_to_definition(&editor::actions::GoToDefinition::default(), window, cx)
        });
        definitions.next().await.expect("asked the server");
        let navigated = navigated.await.expect("went to the definition");
        assert_eq!(navigated, editor::Navigated::Yes);
        cx.run_until_parked();

        // b.rs opened at the definition.
        let (path, cursor) = workspace.update(cx, |workspace, cx| {
            let editor = workspace
                .active_item_as::<Editor>(cx)
                .expect("an editor is active");
            let editor = editor.read(cx);
            let path = editor
                .buffer()
                .read(cx)
                .as_singleton()
                .and_then(|buffer| buffer.read(cx).file().map(|file| file.path().clone()));
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let cursor = editor.selections.newest_anchor().head().to_point(&snapshot);
            (path, cursor)
        });
        assert_eq!(path.as_deref().map(|path| path.as_unix_str()), Some("b.rs"));
        assert_eq!(cursor.row, 0);
    }

    #[gpui::test]
    async fn test_turn_review_reject_and_keep(cx: &mut TestAppContext) {
        // As the agent left them; the bases are as they were before its turn.
        let (fs, review, cx) = open_review(
            cx,
            json!({
                "a.txt": "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n",
                "b.txt": "b changed\n",
                "c.txt": "created\n",
            }),
            &[
                (
                    "a.txt",
                    Some("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n"),
                ),
                ("b.txt", Some("b\n")),
                ("c.txt", None),
            ],
        )
        .await;

        let hunk_count = |review: &Entity<AgentTurnReview>, cx: &mut VisualTestContext| {
            review.update(cx, |review, cx| {
                review
                    .multibuffer
                    .read(cx)
                    .snapshot(cx)
                    .diff_hunks_in_range(Point::zero()..Point::MAX)
                    .count()
            })
        };
        // Two hunks in a.txt, one each in b.txt and c.txt.
        assert_eq!(hunk_count(&review, cx), 4);

        // Reject the first hunk of a.txt: it's put back on disk, the rest stays.
        let first_hunk = review.update(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            let hunk = snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .next()
                .unwrap();
            hunk.multi_buffer_range
        });
        review.update(cx, |review, cx| review.reject_hunks(vec![first_hunk], cx));
        cx.run_until_parked();
        assert_eq!(
            fs.load(path!("/project/a.txt").as_ref()).await.unwrap(),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n"
        );
        assert_eq!(hunk_count(&review, cx), 3);

        // Keep b.txt's change: it stops showing and the file isn't touched.
        let b_hunk = review.update(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            let b_buffer = review.files[1].buffer.read(cx).remote_id();
            snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .find(|hunk| hunk.buffer_id == b_buffer)
                .unwrap()
                .multi_buffer_range
        });
        review.update(cx, |review, cx| review.keep_hunks(vec![b_hunk], cx));
        cx.run_until_parked();
        assert_eq!(hunk_count(&review, cx), 2);
        assert_eq!(
            fs.load(path!("/project/b.txt").as_ref()).await.unwrap(),
            "b changed\n"
        );

        // Rejecting the created file deletes it.
        let c_hunk = review.update(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            let c_buffer = review.files[2].buffer.read(cx).remote_id();
            snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .find(|hunk| hunk.buffer_id == c_buffer)
                .unwrap()
                .multi_buffer_range
        });
        review.update(cx, |review, cx| review.reject_hunks(vec![c_hunk], cx));
        cx.run_until_parked();
        assert!(!fs.is_file(path!("/project/c.txt").as_ref()).await);
        // To the recycle bin, not deleted for good.
        assert_eq!(
            fs.trashed_paths(),
            vec![PathBuf::from(path!("/project/c.txt"))]
        );
        assert_eq!(hunk_count(&review, cx), 1);

        // Changes made after the review opened show up.
        fs.save(
            path!("/project/b.txt").as_ref(),
            &"b changed\nand more\n".into(),
            language::LineEnding::Unix,
        )
        .await
        .unwrap();
        cx.run_until_parked();
        assert_eq!(hunk_count(&review, cx), 2);

        // Rejecting the rest puts a.txt back, after its first hunk was
        // rejected; b.txt changed since the turn, so it's left alone.
        review.update(cx, |review, cx| {
            review.reject_hunks(AgentTurnReview::all_hunks(), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            fs.load(path!("/project/a.txt").as_ref()).await.unwrap(),
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n"
        );
        assert_eq!(
            fs.load(path!("/project/b.txt").as_ref()).await.unwrap(),
            "b changed\nand more\n"
        );
        assert_eq!(hunk_count(&review, cx), 1);
    }

    /// A `.env` that was there all along, ignored, until the turn took it out
    /// of .gitignore: the turn's first snapshot doesn't have it, so it shows
    /// as created. Rejecting everything must not delete it.
    #[gpui::test]
    async fn test_rejecting_keeps_files_that_were_ignored_before_the_turn(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        // On disk, from before the turn.
        std::fs::write(temp.path().join(".env"), "SECRET=1\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let turn_started_at = SystemTime::now();
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Created by the turn.
        std::fs::write(temp.path().join("new.txt"), "new\n").unwrap();

        let (fs, review, cx) = open_review(
            cx,
            json!({
                ".env": "SECRET=1\n",
                "new.txt": "new\n",
            }),
            &[(".env", None), ("new.txt", None)],
        )
        .await;
        // The project's files are fake; when the turn started is told by the
        // real ones.
        let real = |name: &str| temp.path().join(name);
        review.update(cx, |review, cx| {
            review.turn_started_at = Some(turn_started_at);
            for file in &mut review.files {
                let name = file.abs_path.file_name().unwrap().to_owned();
                file.abs_path = real(&name.to_string_lossy());
            }
            cx.notify();
        });
        fs.insert_tree(
            temp.path(),
            json!({ ".env": "SECRET=1\n", "new.txt": "new\n" }),
        )
        .await;

        review.update(cx, |review, cx| {
            review.reject_hunks(AgentTurnReview::all_hunks(), cx)
        });
        cx.run_until_parked();
        assert!(fs.is_file(&real(".env")).await);
        assert!(!fs.is_file(&real("new.txt")).await);
        assert_eq!(fs.trashed_paths(), vec![real("new.txt")]);
        review.read_with(cx, |review, _| {
            assert_eq!(review.files.len(), 1);
            assert!(
                review
                    .status
                    .as_ref()
                    .is_some_and(|status| status.contains(".env")),
                "{:?}",
                review.status
            );
        });

        // Without knowing when the turn started, nothing is deleted.
        assert!(existed_before_turn(&real(".env"), None));
        assert!(existed_before_turn(&real("new.txt"), None));
        assert!(!existed_before_turn(
            &real("new.txt"),
            Some(turn_started_at)
        ));
        assert!(!existed_before_turn(
            &real("gone.txt"),
            Some(turn_started_at)
        ));
    }

    #[gpui::test]
    async fn test_rejecting_keeps_edits_made_after_the_turn(cx: &mut TestAppContext) {
        let (fs, review, cx) = open_review(
            cx,
            json!({
                "edited.txt": "one\nTWO\nthree\n",
                "created.txt": "created\n",
                "unsaved.txt": "agent\n",
                "untouched.txt": "agent\n",
            }),
            &[
                ("edited.txt", Some("one\ntwo\nthree\n")),
                ("created.txt", None),
                ("unsaved.txt", Some("before\n")),
                ("untouched.txt", Some("before\n")),
            ],
        )
        .await;

        // After the turn, the user edits two files on disk and one in its
        // buffer, without saving.
        for (path, text) in [
            (path!("/project/edited.txt"), "one\nTWO, mine\nthree\n"),
            (path!("/project/created.txt"), "created\nmine\n"),
        ] {
            fs.save(path.as_ref(), &text.into(), language::LineEnding::Unix)
                .await
                .unwrap();
        }
        cx.run_until_parked();
        review.update(cx, |review, cx| {
            review.files[2]
                .buffer
                .update(cx, |buffer, cx| buffer.edit([(0..0, "mine ")], None, cx));
        });
        cx.run_until_parked();

        review.update(cx, |review, cx| {
            review.reject_hunks(AgentTurnReview::all_hunks(), cx)
        });
        cx.run_until_parked();

        assert_eq!(
            fs.load(path!("/project/edited.txt").as_ref())
                .await
                .unwrap(),
            "one\nTWO, mine\nthree\n"
        );
        assert_eq!(
            fs.load(path!("/project/created.txt").as_ref())
                .await
                .unwrap(),
            "created\nmine\n"
        );
        assert_eq!(
            fs.load(path!("/project/unsaved.txt").as_ref())
                .await
                .unwrap(),
            "agent\n"
        );
        review.update(cx, |review, cx| {
            assert_eq!(review.files[2].buffer.read(cx).text(), "mine agent\n");
            let status = review.status.clone().unwrap();
            for name in ["edited.txt", "created.txt", "unsaved.txt"] {
                assert!(status.contains(name), "{status}");
            }
            assert!(!status.contains("untouched.txt"), "{status}");
        });
        // The file nobody touched since is rejected.
        assert_eq!(
            fs.load(path!("/project/untouched.txt").as_ref())
                .await
                .unwrap(),
            "before\n"
        );
    }
}
